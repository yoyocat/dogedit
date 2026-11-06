//! Drop effects follow `CIDropTarget::ComputeDropEffect` Ctrl
//! requests COPY, Shift requests MOVE, Ctrl+Shift LINK; with no
//! modifier the default priority is MOVE > COPY > LINK. This module
//! is a thin FFI layer over COM: the `unsafe` blocks at the raw
//! pointer boundaries (FORMATETC/STGMEDIUM access, HGLOBAL locks) are
//! the safety contract.

use std::cell::Cell;
use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, POINT, POINTL,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, FORMATETC, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumFORMATETC_Impl, STGMEDIUM, STGMEDIUM_0,
};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropSource,
    IDropSource_Impl, IDropTarget, IDropTarget_Impl, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::{
    MK_CONTROL, MK_LBUTTON, MK_RBUTTON, MK_SHIFT, MODIFIERKEYS_FLAGS,
};
use windows::Win32::UI::Shell::{
    CLSID_DragDropHelper, DragFinish, DragQueryFileW, HDROP, IDropTargetHelper,
};
use windows::core::{Error, HRESULT, implement};

use crate::win32::window::{drop_move_internal, drop_on_dir_tree, drop_paths, drop_text, state_mut};

const CF_UNICODETEXT: u16 = 13;
/// CF_HDROP (15) - file drop: a file opens on its own, a folder asks what to do
/// with it first.
const CF_HDROP: u16 = 15;

/// DV_E_FORMATETC / DV_E_TYMED
/// (0x80040068 / 0x80040069), returned
/// for unsupported format requests.
const DV_E_FORMATETC: HRESULT = HRESULT(0x80040068_u32 as _);
const DV_E_TYMED: HRESULT = HRESULT(0x80040069_u32 as _);

fn unicode_fmt() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_UNICODETEXT,
        ptd: std::ptr::null_mut(),
        dwAspect: 1, // DVASPECT_CONTENT
        lindex: -1,
        tymed: 1, // TYMED_HGLOBAL
    }
}

fn hdrop_fmt() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP,
        ptd: std::ptr::null_mut(),
        dwAspect: 1,
        lindex: -1,
        tymed: 1,
    }
}

/// `CIDropTarget::ComputeDropEffect`
/// parity: an explicit
/// key request wins when the allowed
/// set contains it; otherwise the
/// default priority is MOVE > COPY >
/// LINK. Returns DROPEFFECT_NONE when
/// no supported format is offered.
fn compute_drop_effect(
    key_state: MODIFIERKEYS_FLAGS,
    allowed: DROPEFFECT,
    offered: bool,
) -> DROPEFFECT {
    if !offered {
        return DROPEFFECT_NONE;
    }
    let requested = if key_state.contains(MK_CONTROL) {
        if key_state.contains(MK_SHIFT) {
            DROPEFFECT_LINK
        } else {
            DROPEFFECT_COPY
        }
    } else if key_state.contains(MK_SHIFT) {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_NONE
    };
    if requested != DROPEFFECT_NONE {
        if (requested & allowed) != DROPEFFECT_NONE {
            requested
        } else {
            DROPEFFECT_NONE
        }
    } else if (allowed & DROPEFFECT_MOVE) != DROPEFFECT_NONE {
        DROPEFFECT_MOVE
    } else if (allowed & DROPEFFECT_COPY) != DROPEFFECT_NONE {
        DROPEFFECT_COPY
    } else if (allowed & DROPEFFECT_LINK) != DROPEFFECT_NONE {
        DROPEFFECT_LINK
    } else {
        DROPEFFECT_NONE
    }
}

/// Whether the data object offers a format we accept.
fn offers(pdataobj: &IDataObject, fmt: &FORMATETC) -> bool {
    unsafe { pdataobj.QueryGetData(fmt) == HRESULT(0) }
}

/// Extract the CF_UNICODETEXT payload
/// from a data object, or an empty
/// string when unavailable.
fn get_unicode_text(pdataobj: &IDataObject) -> String {
    let fmt = unicode_fmt();
    let stg = match unsafe { pdataobj.GetData(&fmt) } {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if stg.tymed != 1 {
        let mut s = stg;
        unsafe { ReleaseStgMedium(&mut s) };
        return String::new();
    }
    let hglob = unsafe { stg.u.hGlobal };
    let mut text = String::new();
    unsafe {
        let ptr = windows::Win32::System::Memory::GlobalLock(hglob) as *const u16;
        if !ptr.is_null() {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            text = String::from_utf16_lossy(slice);
            let _ = windows::Win32::System::Memory::GlobalUnlock(hglob);
        }
    }
    let mut s = stg;
    unsafe { ReleaseStgMedium(&mut s) };
    text
}

/// Extract the CF_HDROP file list from
/// a data object.
fn get_hdrop_files(pdataobj: &IDataObject) -> Vec<String> {
    let fmt = hdrop_fmt();
    let stg = match unsafe { pdataobj.GetData(&fmt) } {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut files = Vec::new();
    if stg.tymed == 1 {
        let hglob = unsafe { stg.u.hGlobal };
        let hdrop = HDROP(hglob.0);
        unsafe {
            let count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                if len == 0 {
                    continue;
                }
                let mut buf = vec![0u16; len + 1];
                let _ = DragQueryFileW(hdrop, i, Some(&mut buf));
                buf.truncate(len);
                if let Some(path) = String::from_utf16(&buf).ok() {
                    files.push(path);
                }
            }
            DragFinish(hdrop);
        }
    }
    let mut s = stg;
    unsafe { ReleaseStgMedium(&mut s) };
    files
}

/// `CIDropSource` parity
/// Escape cancels, any
/// mouse button held keeps the drag
/// running, button release completes it.
#[implement(IDropSource)]
pub struct EditorDropSource;

impl IDropSource_Impl for EditorDropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows::core::BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if !grfkeystate.contains(MK_LBUTTON) && !grfkeystate.contains(MK_RBUTTON) {
            return DRAGDROP_S_DROP;
        }
        HRESULT(0)
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// `CIDataObject` parity: serves the dragged selection as
/// CF_UNICODETEXT / TYMED_HGLOBAL, the only format this drag source
/// registers.
#[implement(IDataObject)]
pub struct EditorDataObject {
    text: Vec<u16>,
}

impl EditorDataObject {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.encode_utf16().collect(),
        }
    }
}

impl IDataObject_Impl for EditorDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let fmt = unsafe { &*pformatetcin };
        if fmt.cfFormat == CF_UNICODETEXT && (fmt.tymed & 1) != 0 {
            let bytes = (self.text.len() + 1) * 2;
            unsafe {
                if let Ok(hglob) = windows::Win32::System::Memory::GlobalAlloc(
                    windows::Win32::System::Memory::GHND,
                    bytes,
                ) {
                    let ptr = windows::Win32::System::Memory::GlobalLock(hglob) as *mut u16;
                    if !ptr.is_null() {
                        std::ptr::copy_nonoverlapping(self.text.as_ptr(), ptr, self.text.len());
                        *ptr.add(self.text.len()) = 0;
                        let _ = windows::Win32::System::Memory::GlobalUnlock(hglob);
                        return Ok(STGMEDIUM {
                            tymed: 1,
                            u: STGMEDIUM_0 { hGlobal: hglob },
                            pUnkForRelease: ManuallyDrop::new(None),
                        });
                    }
                    let _ = windows::Win32::Foundation::GlobalFree(Some(hglob));
                }
            }
            return Err(Error::from_hresult(HRESULT(0x8007000E_u32 as _)));
        }
        Err(Error::from_hresult(DV_E_FORMATETC))
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(Error::from_hresult(DV_E_TYMED))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        let fmt = unsafe { &*pformatetc };
        if fmt.cfFormat == CF_UNICODETEXT && (fmt.tymed & 1) != 0 {
            HRESULT(0)
        } else if fmt.cfFormat == CF_UNICODETEXT {
            DV_E_TYMED
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        _pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        HRESULT(0x80040069_u32 as _) // DV_E_FORMATETC family: not supported
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: windows::core::BOOL,
    ) -> windows::core::Result<()> {
        Err(Error::from_hresult(HRESULT(0x80040064_u32 as _))) // OLE_E_ADVISENOTSUPPORTED
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        // DATADIR_GET (0 per the COM spec;
        // the crate constant is 1) serves
        // the offered formats.
        if dwdirection == 0 || dwdirection == 1 {
            Ok(FormatEnum {
                formats: vec![unicode_fmt()],
                index: Cell::new(0),
            }
            .into())
        } else {
            Err(Error::from_hresult(HRESULT(0x80040069_u32 as _)))
        }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: windows::core::Ref<'_, windows::Win32::System::Com::IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(Error::from_hresult(HRESULT(0x80040003_u32 as _))) // OLE_E_ADVISENOTSUPPORTED
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(Error::from_hresult(HRESULT(0x80040003_u32 as _)))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<windows::Win32::System::Com::IEnumSTATDATA> {
        Err(Error::from_hresult(HRESULT(0x80040003_u32 as _)))
    }
}

/// `CEnumFormatEtc` parity: enumerates
/// the single CF_UNICODETEXT format.
#[implement(IEnumFORMATETC)]
pub struct FormatEnum {
    formats: Vec<FORMATETC>,
    index: Cell<usize>,
}

impl IEnumFORMATETC_Impl for FormatEnum_Impl {
    fn Next(&self, celt: u32, rgelt: *mut FORMATETC, pceltfetched: *mut u32) -> HRESULT {
        let start = self.index.get();
        let mut i = 0usize;
        while (i as u32) < celt && start + i < self.formats.len() {
            if rgelt.is_null() {
                break;
            }
            unsafe {
                *rgelt.add(i) = self.formats[start + i];
            }
            i += 1;
        }
        let fetched = i as u32;
        self.index.set(start + i);
        if !pceltfetched.is_null() {
            unsafe {
                *pceltfetched = fetched;
            }
        }
        if fetched == celt {
            HRESULT(0)
        } else {
            HRESULT(1) // S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> windows::core::Result<()> {
        let start = self.index.get();
        let avail = self.formats.len().saturating_sub(start);
        if (celt as usize) <= avail {
            self.index.set(start + celt as usize);
            Ok(())
        } else {
            self.index.set(self.formats.len());
            Err(Error::from_hresult(HRESULT(1_u32 as _))) // S_FALSE
        }
    }

    fn Reset(&self) -> windows::core::Result<()> {
        self.index.set(0);
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumFORMATETC> {
        Ok(FormatEnum {
            formats: self.formats.clone(),
            index: Cell::new(self.index.get()),
        }
        .into())
    }
}

/// `ViewDropTarget` parity: drop-in target registered on the editor view —
/// the text area accepts, the toolbar and the status strip do not, so the
/// system draws its 禁止标 over the chrome. Every callback is handed to the
/// shell's drag-drop helper first, which is what keeps the dragged thing's own
/// picture under the cursor (a file shows its icon, a folder a folder).
/// DragOver moves the caret to the drop point; Drop inserts text at the drop
/// point through the undo stack, opens dropped files, or puts up the folder
/// menu when what was dropped is one directory.
#[implement(IDropTarget)]
pub struct EditorDropTarget {
    hwnd: HWND,
    view: HWND,
    helper: Option<IDropTargetHelper>,
}

impl EditorDropTarget {
    pub(super) fn new(hwnd: HWND, view: HWND) -> Self {
        // 拖影由 shell 这个 helper 画：它把拖拽方塞进数据对象里的那张图，按光标
        // 位置贴到 target 窗口上。不建它，拖动时就只剩一个光标、没有图。
        let helper: Option<IDropTargetHelper> = unsafe {
            CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok()
        };
        Self {
            hwnd,
            view,
            helper,
        }
    }
}

impl IDropTarget_Impl for EditorDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let offered = offers(pdataobj.as_ref().unwrap(), &unicode_fmt())
            || offers(pdataobj.as_ref().unwrap(), &hdrop_fmt());
        let allowed = unsafe { *pdweffect };
        let effect = compute_drop_effect(grfkeystate, allowed, offered);
        unsafe {
            *pdweffect = effect;
        }
        if let Some(helper) = &self.helper {
            let at = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = helper.DragEnter(self.view, pdataobj.as_ref().unwrap(), &at, effect);
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let allowed = unsafe { *pdweffect };
        let effect = compute_drop_effect(grfkeystate, allowed, true);
        unsafe {
            *pdweffect = effect;
        }
        if let Some(helper) = &self.helper {
            let at = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = helper.DragOver(&at, effect);
            }
        }
        // DragOver: the caret follows the
        // drop point so the user sees
        // where the text will land.
        if effect != DROPEFFECT_NONE {
            if let Some(st) = state_mut(self.hwnd) {
                let (x, y) = (pt.x, pt.y);
                if st.drop_point(x, y).is_some() {
                    st.move_caret_to_point(x, y);
                }
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        // 图是 helper 贴上去的，离开窗口得让它自己收走，否则那张图会留在原地。
        if let Some(helper) = &self.helper {
            unsafe {
                let _ = helper.DragLeave();
            }
        }
        // caret here; this implementation's caret is
        // always visible, so nothing to
        // restore until the drag ends.
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let (x, y) = (pt.x, pt.y);
        // 松手这一下和悬停时走同一套算法，不另写 COPY：光标在悬停时是「移动」，
        // 一松手就换一种效果，Explorer 的落点提示会跳一下。
        let offered = offers(pdataobj.as_ref().unwrap(), &hdrop_fmt())
            || offers(pdataobj.as_ref().unwrap(), &unicode_fmt());
        let allowed = unsafe { *pdweffect };
        let effect = compute_drop_effect(grfkeystate, allowed, offered);
        unsafe {
            *pdweffect = effect;
        }
        if let Some(helper) = &self.helper {
            let at = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = helper.Drop(pdataobj.as_ref().unwrap(), &at, effect);
            }
        }
        if effect == DROPEFFECT_NONE {
            return Ok(());
        }
        let internal = state_mut(self.hwnd).map(|s| s.drag_active).unwrap_or(false);
        if offers(pdataobj.as_ref().unwrap(), &hdrop_fmt()) {
            let files = get_hdrop_files(pdataobj.as_ref().unwrap());
            if !files.is_empty() {
                drop_paths(self.hwnd, &files, x, y);
                return Ok(());
            }
        }
        if offers(pdataobj.as_ref().unwrap(), &unicode_fmt()) {
            let text = get_unicode_text(pdataobj.as_ref().unwrap());
            if !text.is_empty() {
                if internal {
                    if let Some(st) = state_mut(self.hwnd) {
                        st.internal_move_done = true;
                    }
                    drop_move_internal(self.hwnd, x, y, &text);
                } else {
                    drop_text(self.hwnd, x, y, &text);
                }
            }
        }
        Ok(())
    }
}

/// 面板只收文件列表，且落地是「复制」这一路：文件夹成为快捷目录的根、文件开成
/// 页签，两者都没有把原件搬走，所以光标按 COPY 走，允许集里没有 COPY 才退到 MOVE。
fn copy_first(allowed: DROPEFFECT) -> DROPEFFECT {
    if (allowed & DROPEFFECT_COPY) != DROPEFFECT_NONE {
        DROPEFFECT_COPY
    } else if (allowed & DROPEFFECT_MOVE) != DROPEFFECT_NONE {
        DROPEFFECT_MOVE
    } else if (allowed & DROPEFFECT_LINK) != DROPEFFECT_NONE {
        DROPEFFECT_LINK
    } else {
        DROPEFFECT_NONE
    }
}

/// 快捷目录面板的拖放目标，挂在目录树那个窗口上。拖进来的文件夹往清单里加一行
/// （不是把原来的换掉），拖进来的文件按打开处理；拖影同样交给 shell 的 helper 贴。
#[implement(IDropTarget)]
pub struct DockTreeDropTarget {
    frame: HWND,
    tree: HWND,
    helper: Option<IDropTargetHelper>,
    /// DragEnter 时数据对象带不带文件列表。DragOver 拿不到数据对象，只能记着。
    offered: Cell<bool>,
}

impl DockTreeDropTarget {
    pub(super) fn new(frame: HWND, tree: HWND) -> Self {
        let helper: Option<IDropTargetHelper> = unsafe {
            CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok()
        };
        Self {
            frame,
            tree,
            helper,
            offered: Cell::new(false),
        }
    }
}

impl IDropTarget_Impl for DockTreeDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let offered = offers(pdataobj.as_ref().unwrap(), &hdrop_fmt());
        self.offered.set(offered);
        let allowed = unsafe { *pdweffect };
        let effect = if offered {
            copy_first(allowed)
        } else {
            DROPEFFECT_NONE
        };
        unsafe {
            *pdweffect = effect;
        }
        if let Some(helper) = &self.helper {
            let at = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = helper.DragEnter(self.tree, pdataobj.as_ref().unwrap(), &at, effect);
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let allowed = unsafe { *pdweffect };
        let effect = if self.offered.get() {
            copy_first(allowed)
        } else {
            DROPEFFECT_NONE
        };
        unsafe {
            *pdweffect = effect;
        }
        if let Some(helper) = &self.helper {
            let at = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = helper.DragOver(&at, effect);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.offered.set(false);
        if let Some(helper) = &self.helper {
            unsafe {
                let _ = helper.DragLeave();
            }
        }
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let allowed = unsafe { *pdweffect };
        let effect = if self.offered.get() {
            copy_first(allowed)
        } else {
            DROPEFFECT_NONE
        };
        unsafe {
            *pdweffect = effect;
        }
        self.offered.set(false);
        if let Some(helper) = &self.helper {
            let at = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = helper.Drop(pdataobj.as_ref().unwrap(), &at, effect);
            }
        }
        if effect == DROPEFFECT_NONE {
            return Ok(());
        }
        let files = get_hdrop_files(pdataobj.as_ref().unwrap());
        if !files.is_empty() {
            drop_on_dir_tree(self.frame, &files);
        }
        Ok(())
    }
}
