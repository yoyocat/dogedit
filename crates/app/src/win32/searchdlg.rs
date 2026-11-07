//! 查找中 — the find-in-files progress dialog.
//!
//! Geometry and control ids:
//!
//! * **Geometry** — the box is a DLGTEMPLATEEX, style `0x90C808CC`
//!   (`DS_CENTER | DS_MODALFRAME | DS_3DLOOK | DS_SETFONT`), no extended style,
//!   271×90 dialog units, font `Ms Shell Dlg 8`, three children in this order:
//!   the default Button `BS_DEFPUSHBUTTON` at 107,69 50×14, a read-only multiline
//!   Edit `ES_MULTILINE | ES_READONLY` at 7,7 257×40, and a Static at 7,48 257×20.
//!   The 8 pt block stays as the dialog-unit ruler; the children's text is
//!   replaced with the app's UI font in `WM_INITDIALOG`.
//! * **Captions** — from the language table: 查找中 as the title (untranslated it
//!   reads `Grep ing`), 停止 on the button, and `行数:%d` in the static.
//! * **The updater** — format `行数:%d` with the running count, then set the text
//!   of the static the object holds — one call per report, no repaint logic of
//!   its own.
//!
//! The search always runs on a worker, so this dialog is a modeless window the
//! frame's own loop serves through [`translate`] — which is what lets its caption
//! be dragged and its button be pressed while the scan is in flight. 锁定主窗口
//! chooses only whether the frame is *disabled* meanwhile: with the flag the main
//! window answers nothing until the scan unwinds; without it the editor keeps
//! working behind the dialog.  停止 (and `Esc`, and the close box) only raises the
//! flag; the caller owns the window's lifetime and destroys it once the search
//! unwinds, so the partial result set can be reported first.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlgtpl::{self, Class, Item};
use crate::win32::wstr::wide;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT, RedrawWindow, UpdateWindow,
    RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateDialogIndirectParamW, DefWindowProcW, DLGTEMPLATE, DestroyWindow, GWL_STYLE, GetAncestor,
    GA_ROOT, GetClientRect, GetWindowLongPtrW, GWLP_USERDATA, IDCANCEL, IsDialogMessageW, MSG,
    SetDlgItemTextW, SetWindowLongPtrW, ShowWindow, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT,
    WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG, WM_TIMER,
    WS_CLIPCHILDREN,
};

use crate::win32::customdraw;
use crate::win32::view::instance;
use crate::win32::window::to_colorref;
use dg_render::theme::Theme;

// ---------------------------------------------------------------------------
// The dialog template, byte for byte
// ---------------------------------------------------------------------------

const DLG_STYLE: u32 = 0x90c8_08cc;
const DLG_EXSTYLE: u32 = 0x0000_0000;
const DLG_W: u16 = 271;
const DLG_H: u16 = 90;
/// The window title, from the language table; untranslated it reads `Grep ing`.
const DLG_TITLE: &str = "查找中";
/// The one button's caption.
const STOP_TEXT: &str = "停止";

const ID_STOP: i32 = 1;
const ID_PATH: i32 = 1200;
const ID_COUNT: i32 = 1201;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_STATIC: u16 = 0x0082;

/// The three children, in template order. The two captions that the runtime
/// fills are empty in the template, exactly as the recorded one has them.
const ITEMS: &[Item] = &[
    Item { id: ID_STOP, class: Class::Atom(ATOM_BUTTON), style: 0x50010001, x: 107, y: 69, w: 50, h: 14, label: STOP_TEXT },
    Item { id: ID_PATH, class: Class::Atom(ATOM_EDIT), style: 0x50010804, x: 7, y: 7, w: 257, h: 40, label: "" },
    Item { id: ID_COUNT, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 48, w: 257, h: 20, label: "" },
];

/// Build the DLGTEMPLATEEX buffer, the same shape [`grepdlg`](super::grepdlg)
/// writes for the 在文件中查找和替换 box.
fn build_template(title: &str) -> Vec<u8> {
    dlgtpl::build_legacy(title, DLG_STYLE, DLG_EXSTYLE, DLG_W, DLG_H, 0, ITEMS)
}

// ---------------------------------------------------------------------------
// The dialog object
// ---------------------------------------------------------------------------

/// What the window proc owns: the stop flag the caller reads, the frame the
/// search locked, and the two brushes the chrome colours need.
struct Inner {
    stop: Arc<AtomicBool>,
    /// The dialog face and the edit field's face — the same chrome block the
    /// 在文件中查找和替换 dialog is painted from.
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
    /// The UI font for the three children; the template's 8 pt `DS_SETFONT`
    /// block stays as the dialog-unit ruler.
    font: HFONT,
}

/// The live progress dialog's `HWND`, so a second scan finds the window it already
/// has instead of opening another.
static DLG: AtomicIsize = AtomicIsize::new(0);

unsafe fn state(hwnd: HWND) -> &'static Inner {
    &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Inner)
}

/// The 查找中 dialog while it is up. Dropping it takes the window away and
/// unlocks the frame again.
pub(super) struct Prog {
    dlg: HWND,
    frame: HWND,
    stop: Arc<AtomicBool>,
    /// `report` is called for every match; rewriting both captions that often is
    /// the slow part of a scan, so it runs on a time budget.
    last: Instant,
}

/// Two caption rewrites per centisecond is more than the eye can read and takes
/// the SetWindowText cost out of the inner loop.
const REPORT_MS: u128 = 50;

/// Opens 查找中 over `frame`. `lock` is 锁定主窗口: with it the frame is
/// disabled for the whole search: the main window answers nothing while 查找中
/// is up, and the dialog keeps its own caption drag and button.
pub(super) fn open(frame: HWND, theme: &Theme, lock: bool) -> Prog {
    open_named(frame, theme, lock, DLG_TITLE)
}

/// With the caption the caller wants; [`open`] uses the box's own 查找中.
pub(super) fn open_named(frame: HWND, theme: &Theme, lock: bool, title: &str) -> Prog {
    let stop = Arc::new(AtomicBool::new(false));
    let boxed = Box::new(Inner {
        stop: stop.clone(),
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        ink: to_colorref(theme.toolbar_fg),
        face: to_colorref(theme.toolbar_bg),
        field_face: to_colorref(theme.toolbar_field),
        font: unsafe {
            crate::win32::uifont::create(crate::win32::uifont::window_dpi(frame))
        },
    });
    let param = LPARAM(Box::into_raw(boxed) as isize);
    let tmpl = build_template(title);
    let dlg = match unsafe {
        CreateDialogIndirectParamW(
            Some(instance()),
            &*(tmpl.as_ptr() as *const DLGTEMPLATE),
            Some(frame),
            Some(dialog_proc),
            param,
        )
    } {
        Ok(h) => h,
        Err(_) => {
            unsafe {
                let inner = Box::from_raw(param.0 as *mut Inner);
                let _ = DeleteObject(inner.font.into());
            }
            return Prog {
                dlg: HWND::default(),
                frame,
                stop,
                last: Instant::now(),
            };
        }
    };
    DLG.store(dlg.0 as isize, Ordering::Release);
    unsafe {
        let dark = crate::win32::chrome::theme_is_dark(theme);
        crate::win32::chrome::set_chrome_theme(theme);
        // The controls get the dark palette and the caption the dark attribute,
        // but the dialog window itself is never named to `SetWindowTheme`: that
        // recalculates a `#32770`'s non-client frame with doubled metrics, which
        // keeps the window size and takes a whole frame (18x47 px here) out of
        // the client — and the template's bottom-most control, 停止 at y 69..83 of
        // a 90-high dialog, falls off the bottom edge and is never seen.
        crate::win32::chrome::dark_theme_children(dlg, dark);
        crate::win32::chrome::title_bar(dlg, dark);
        // The template's style has no WS_CLIPCHILDREN (the 在文件中查找和替换 box's
        // does). In light mode that costs nothing; with the chrome block as the
        // dialog face the parent's whole-client erase also covers the children,
        // and only the controls that change repaint — the 停止 button would stay
        // erased.
        let style = GetWindowLongPtrW(dlg, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
        SetWindowLongPtrW(dlg, GWL_STYLE, style);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = UpdateWindow(dlg);
        if lock {
            let _ = EnableWindow(frame, false);
        }
    }
    Prog {
        dlg,
        frame,
        stop,
        last: Instant::now(),
    }
}

impl Prog {
    /// What the dialog shows the moment it opens: the folder being searched and
    /// `行数:0`. The template leaves both captions empty for the runtime to fill,
    /// and the first report only comes when the scan reaches a file — with
    /// ripgrep that can be seconds, during which the window is an empty box with
    /// a lone 停止 button.
    pub(super) fn seed(&mut self, folder: &str) {
        if !self.dlg.0.is_null() {
            self.set_text(folder, 0);
        }
    }

    /// The flag for a worker thread to watch: 停止 sets it, and the search's
    /// `Progress` callback reads it — the only way a scan that does not run on
    /// the frame's thread can be cancelled.
    pub(super) fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// The live report: the file that was just scanned into the read-only edit,
    /// `行数:%d` with the running total into the static. Throttled to
    /// [`REPORT_MS`]; the final call is made by [`Prog::finish`].
    pub(super) fn report(&mut self, path: &str, count: usize) {
        if self.dlg.0.is_null() {
            return;
        }
        if self.last.elapsed().as_millis() < REPORT_MS {
            return;
        }
        self.last = Instant::now();
        self.set_text(path, count);
    }

    /// The last report, whatever the budget says: the file the search stopped on
    /// and the total it reached.
    pub(super) fn finish(&mut self, path: &str, count: usize) {
        if self.dlg.0.is_null() {
            return;
        }
        self.set_text(path, count);
        unsafe {
            let _ = UpdateWindow(self.dlg);
        }
    }

    fn set_text(&self, path: &str, count: usize) {
        let rows = format!("行数:{count}");
        unsafe {
            let file = wide(path);
            let total = wide(&rows);
            let _ = SetDlgItemTextW(self.dlg, ID_PATH, PCWSTR(file.as_ptr()));
            let _ = SetDlgItemTextW(self.dlg, ID_COUNT, PCWSTR(total.as_ptr()));
            // Paint it here, synchronously: `RDW_UPDATENOW` sends the paint
            // rather than queueing it, and `RDW_ALLCHILDREN` carries it to the
            // path edit, the 行数 static and the button, so the report is on
            // screen before the next file is scanned.
            let _ = RedrawWindow(
                Some(self.dlg),
                None,
                None,
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN | RDW_ERASE,
            );
        }
    }

    /// the teardown of a running search: the window goes, and
    /// the frame is enabled again whether or not the search stopped early.
    fn dismiss(&mut self) {
        if !self.dlg.0.is_null() {
            unsafe {
                let _ = DestroyWindow(self.dlg);
            }
            self.dlg = HWND::default();
        }
        unsafe {
            let _ = EnableWindow(self.frame, true);
        }
    }
}

impl Drop for Prog {
    fn drop(&mut self) {
        self.dismiss();
    }
}

/// Whether the frame's loop should feed `msg` to the dialog, so its keyboard map
/// (`Tab`, `Enter` → 停止, `Esc`) works while the scan runs on the worker.
pub(super) fn translate(msg: &MSG) -> bool {
    let dlg = DLG.load(Ordering::Acquire);
    if dlg == 0 {
        return false;
    }
    let dlg = HWND(dlg as *mut core::ffi::c_void);
    if unsafe { GetAncestor(msg.hwnd, GA_ROOT) } != dlg {
        return false;
    }
    unsafe { IsDialogMessageW(dlg, msg).as_bool() }
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            crate::win32::uifont::set_children(hwnd, state(hwnd).font);
            crate::win32::lang::localize_dialog(hwnd, 222);
            1
        }
        WM_ERASEBKGND => {
            let st = state(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        WM_CTLCOLOREDIT => {
            // The path edit is the read-only one
            // field face, not `COLOR_WINDOW`.
            let st = state(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.field_face, st.field)
        }
        WM_COMMAND => {
            // 停止 is the template's default button (`IDOK`), so `Enter` and the
            // click land here; `Esc` reaches the dialog as `IDCANCEL`. Both mean
            // the same thing: stop scanning, show what was found.
            let id = (wparam.0 & 0xFFFF) as i32;
            if id == ID_STOP || id == IDCANCEL.0 {
                state(hwnd).stop.store(true, Ordering::Relaxed);
            }
            1
        }
        WM_CLOSE => {
            // The close box stops the search; the caller owns the window and
            // destroys it once the scan unwinds.
            state(hwnd).stop.store(true, Ordering::Relaxed);
            1
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Inner;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DLG.store(0, Ordering::Release);
            if !ptr.is_null() {
                let b = Box::from_raw(ptr);
                let _ = DeleteObject(b.bg.into());
                let _ = DeleteObject(b.field.into());
                let _ = DeleteObject(b.font.into());
            }
            0
        }
        // Nothing here arms a timer, so swallowing
        // the message costs the frame nothing.
        WM_TIMER => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam).0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The template has to be the recorded one: three children, the stop
    /// button first, `BS_DEFPUSHBUTTON`, and the ids the updater writes into.
    #[test]
    fn template_matches_the_resource() {
        assert_eq!(ITEMS.len(), 3);
        assert_eq!(ITEMS[0].id, ID_STOP);
        assert_eq!(ITEMS[0].style, 0x50010001);
        assert_eq!(ITEMS[1].id, ID_PATH);
        assert_eq!(ITEMS[1].style, 0x50010804);
        assert_eq!(ITEMS[2].id, ID_COUNT);
        let b = build_template(DLG_TITLE);
        // dlgVer + signature, then the header's styles.
        assert_eq!(&b[0..4], &[1, 0, 0xFF, 0xFF]);
        assert_eq!(&b[12..16], &DLG_STYLE.to_le_bytes());
        assert_eq!(u16::from_le_bytes([b[16], b[17]]), 3);
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), DLG_W);
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), DLG_H);
    }

    #[test]
    fn rows_caption_is_the_resources_own_format() {
        assert_eq!(format!("行数:{}", 7), "行数:7");
    }
}
