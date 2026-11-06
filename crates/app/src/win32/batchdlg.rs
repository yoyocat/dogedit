//! 批量替换 — the batch-replace dialog behind 编辑 > 批量替换
//! (`cm_replace_batch`, 1420).
//!
//! One ordered list of 查找/替换 pairs runs over the current document or over
//! every open one, pair after pair: the second pair sees the first pair's
//! output. The three option checkboxes (匹配大小写 / 全字匹配 / 正则表达式)
//! feed the same [`dg_core::FindOptions`] the ordinary search uses. 替换所选
//! restricts the run to the active selection — each selection range becomes
//! one undo step, replaced as a whole through a scratch document. 统计 counts
//! matches per pair without editing anything.
//!
//! The pairs survive sessions in `setting.ini`: `[BatchReplace] Count` plus a
//! `FindN`/`RepN` pair per row, written by 保存 and read back when the box
//! opens. 导出/导入 carry the same list through a text file, one
//! `查找<TAB>替换` row per line; tabs inside a field travel as the two
//! characters `\t`, so a pasted tab does not shift the columns.
//!
//! Geometry follows the 批量替换 box's own template: the 453×176 units dialog
//! with the pairs list filling the left three quarters, the pair-entry strip
//! and the list commands stacked in the right column, and the option row plus
//! the two run buttons along the bottom. Captions come from the `[DLG208]`
//! table by control id.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::wstr::wide;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, RedrawWindow, UpdateWindow,
    RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_OVERWRITEPROMPT, OPENFILENAMEW,
};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::UI::Controls::{CheckRadioButton, IsDlgButtonChecked};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    DialogBoxIndirectParamW, DLGTEMPLATE, EndDialog, GetClientRect, GetDlgItem,
    GetDlgItemTextW, GWLP_USERDATA, GetWindowLongPtrW, IDCANCEL, LB_ADDSTRING,
    LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, MB_OK, MessageBoxW, SendMessageW,
    SetWindowLongPtrW, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_CTLCOLORBTN,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG, WM_SETTEXT,
};

use crate::win32::chrome;
use crate::win32::customdraw;
use crate::win32::lang::{format as lfmt, localize_dialog, string as lstr, trs};
use crate::win32::optionsdlg::{build_template, Class, Item};
use crate::win32::view::instance;
use crate::win32::window::{to_colorref, EditorState};
use dg_core::FindOptions;

// ---------------------------------------------------------------------------
// Control ids — the `[DLG208]` table keys captions by these
// ---------------------------------------------------------------------------

const ID_LIST: i32 = 1200;
const ID_UP: i32 = 1201;
const ID_DOWN: i32 = 1202;
const ID_EXPORT: i32 = 1206;
const ID_IMPORT: i32 = 1207;
const ID_SAVE: i32 = 1208;
const ID_REPLACE_ALL: i32 = 1211;
const ID_REPLACE_SEL: i32 = 1212;
const ID_STATS: i32 = 1221;
const ID_SCOPE_CUR: i32 = 1223;
const ID_SCOPE_ALL: i32 = 1224;
const ID_FIND_LBL: i32 = 1230;
const ID_REP_LBL: i32 = 1231;
const ID_ADD: i32 = 1232;
const ID_MOD: i32 = 1233;
const ID_DEL: i32 = 1234;
const ID_CASE: i32 = 1235;
const ID_WORD: i32 = 1236;
const ID_REGEX: i32 = 1237;
/// The two pair-entry edits; no caption rows, so they stay off the table.
const ID_FIND: i32 = 1240;
const ID_REP: i32 = 1241;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_STATIC: u16 = 0x0082;
const ATOM_LISTBOX: u16 = 0x0083;

// ---------------------------------------------------------------------------
// The template
// ---------------------------------------------------------------------------

const DLG_STYLE: u32 = 0x90c8_08cc;
const DLG_W: u16 = 453;
const DLG_H: u16 = 176;
const DLG_TITLE: &str = "批量替换";

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_BORDER: u32 = 0x0080_0000;
const BS_AUTOCHECKBOX: u32 = 0x0000_0003;
const BS_AUTORADIOBUTTON: u32 = 0x0000_0009;
const ES_AUTOHSCROLL: u32 = 0x0000_0080;
const LBS_NOTIFY: u32 = 0x0000_0001;
const LBS_NOINTEGRALHEIGHT: u32 = 0x0000_0100;

const STYLE_BUTTON: u32 = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
const STYLE_DEFBUTTON: u32 = STYLE_BUTTON | 0x0000_0001;
const STYLE_CHECK: u32 = STYLE_BUTTON | BS_AUTOCHECKBOX;
const STYLE_RADIO: u32 = WS_CHILD | WS_VISIBLE | BS_AUTORADIOBUTTON;
const STYLE_STATIC: u32 = WS_CHILD | WS_VISIBLE | WS_GROUP;
const STYLE_EDIT: u32 = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL;
const STYLE_LIST: u32 = WS_CHILD
    | WS_VISIBLE
    | WS_TABSTOP
    | WS_BORDER
    | WS_VSCROLL
    | LBS_NOTIFY
    | LBS_NOINTEGRALHEIGHT;

/// The children, in template order. The right column stacks the pair-entry
/// strip (labels, edits, 添加/修改/删除) over the list commands (向上/向下,
/// 导出/导入, 保存/统计); the bottom row carries the three option checkboxes,
/// the scope radios and the two run buttons.
const ITEMS: &[Item] = &[
    Item { id: ID_LIST, class: Class::Atom(ATOM_LISTBOX), style: STYLE_LIST, x: 7, y: 7, w: 375, h: 145, label: "" },
    Item { id: ID_FIND_LBL, class: Class::Atom(ATOM_STATIC), style: STYLE_STATIC, x: 389, y: 7, w: 57, h: 9, label: "" },
    Item { id: ID_FIND, class: Class::Atom(ATOM_EDIT), style: STYLE_EDIT, x: 389, y: 17, w: 57, h: 13, label: "" },
    Item { id: ID_REP_LBL, class: Class::Atom(ATOM_STATIC), style: STYLE_STATIC, x: 389, y: 33, w: 57, h: 9, label: "" },
    Item { id: ID_REP, class: Class::Atom(ATOM_EDIT), style: STYLE_EDIT, x: 389, y: 43, w: 57, h: 13, label: "" },
    Item { id: ID_ADD, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 389, y: 58, w: 57, h: 14, label: "" },
    Item { id: ID_MOD, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 389, y: 74, w: 57, h: 14, label: "" },
    Item { id: ID_DEL, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 389, y: 90, w: 57, h: 14, label: "" },
    Item { id: ID_UP, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 389, y: 107, w: 27, h: 13, label: "" },
    Item { id: ID_DOWN, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 418, y: 107, w: 27, h: 13, label: "" },
    Item { id: ID_EXPORT, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 389, y: 122, w: 27, h: 13, label: "" },
    Item { id: ID_IMPORT, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 418, y: 122, w: 27, h: 13, label: "" },
    Item { id: ID_SAVE, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 389, y: 137, w: 27, h: 13, label: "" },
    Item { id: ID_STATS, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 418, y: 137, w: 27, h: 13, label: "" },
    Item { id: ID_CASE, class: Class::Atom(ATOM_BUTTON), style: STYLE_CHECK, x: 7, y: 159, w: 52, h: 10, label: "" },
    Item { id: ID_WORD, class: Class::Atom(ATOM_BUTTON), style: STYLE_CHECK, x: 62, y: 159, w: 46, h: 10, label: "" },
    Item { id: ID_REGEX, class: Class::Atom(ATOM_BUTTON), style: STYLE_CHECK, x: 112, y: 159, w: 60, h: 10, label: "" },
    Item { id: ID_SCOPE_CUR, class: Class::Atom(ATOM_BUTTON), style: STYLE_RADIO | WS_GROUP, x: 180, y: 159, w: 42, h: 10, label: "" },
    Item { id: ID_SCOPE_ALL, class: Class::Atom(ATOM_BUTTON), style: STYLE_RADIO, x: 226, y: 159, w: 66, h: 10, label: "" },
    Item { id: ID_REPLACE_SEL, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 300, y: 156, w: 70, h: 14, label: "" },
    Item { id: ID_REPLACE_ALL, class: Class::Atom(ATOM_BUTTON), style: STYLE_DEFBUTTON, x: 376, y: 156, w: 70, h: 14, label: "" },
];

// ---------------------------------------------------------------------------
// The dialog object
// ---------------------------------------------------------------------------

/// What the window proc owns. `state` is the frame: the run buttons reach back
/// into it for the actual replacements, the same borrow the settings dialogs
/// hold for their reads.
struct Ctx {
    state: *mut EditorState,
    frame: HWND,
    pairs: Vec<(String, String)>,
    theme: dg_render::Theme,
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
}

fn lt(id: u32, fallback: &str) -> String {
    lstr(id).unwrap_or(fallback).to_string()
}

/// The list row for a pair; tabs and line breaks are shown escaped so one
/// pair stays one row.
fn display_pair(find: &str, rep: &str) -> String {
    format!("{} → {}", escape_field(find), escape_field(rep))
}

/// The find field as the report shows it — the same escaping the list uses.
/// The frame builds the per-pair report rows with it.
pub(super) fn display_for_report(find: &str) -> String {
    escape_field(find)
}

/// The exporter's field encoding: tabs and line breaks become the two
/// characters `\t` / `\n` / `\r`, backslashes double.
fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Inverse of [`escape_field`].
fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Opens the box over the frame. The frame loads its saved pairs and hands
/// the dialog a pointer; every button acts in place, the box closes with
/// `Esc` or the close box.
pub(super) fn open(state: &mut EditorState, hwnd: HWND) {
    let pairs = state.batch_load_pairs();
    let theme = state.docs.act().theme.clone();
    let mut ctx = Box::new(Ctx {
        state: state as *mut EditorState,
        frame: hwnd,
        pairs,
        theme: theme.clone(),
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        ink: to_colorref(theme.toolbar_fg),
        face: to_colorref(theme.toolbar_bg),
        field_face: to_colorref(theme.toolbar_field),
    });
    let ptr = &mut *ctx as *mut Ctx;
    let tmpl = build_template(DLG_TITLE, DLG_STYLE, DLG_W, DLG_H, 0, ITEMS);
    let mut aligned = vec![0u32; (tmpl.len() + 3) / 4];
    unsafe {
        std::ptr::copy_nonoverlapping(tmpl.as_ptr(), aligned.as_mut_ptr() as *mut u8, tmpl.len());
    }
    unsafe {
        let _ = DialogBoxIndirectParamW(
            Some(instance()),
            aligned.as_ptr() as *const DLGTEMPLATE,
            Some(hwnd),
            Some(dialog_proc),
            LPARAM(ptr as isize),
        );
    }
    // The modal call has returned: the dialog proc is done with the state, so
    // the brushes go back now and the box drops.
    unsafe {
        let _ = DeleteObject(ctx.bg.into());
        let _ = DeleteObject(ctx.field.into());
    }
}


/// The list/text messages this box sends, through the `Option`-wrapped
/// signature the bindings give `SendMessageW`.
unsafe fn send(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    SendMessageW(hwnd, msg, Some(wparam), Some(lparam)).0
}

unsafe fn ctx(hwnd: HWND) -> &'static mut Ctx {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ctx)
}

unsafe fn dlg_text(hwnd: HWND, id: i32) -> String {
    let mut buf = [0u16; 1024];
    let _ = GetDlgItemTextW(hwnd, id, &mut buf);
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

unsafe fn list_hwnd(hwnd: HWND) -> Option<HWND> {
    GetDlgItem(Some(hwnd), ID_LIST).ok()
}

unsafe fn list_sel(hwnd: HWND) -> Option<usize> {
    let idx = send(list_hwnd(hwnd)?, LB_GETCURSEL, WPARAM(0), LPARAM(0));
    if idx < 0 {
        None
    } else {
        Some(idx as usize)
    }
}

unsafe fn refill_list(hwnd: HWND, sel: Option<usize>) {
    let Some(list) = list_hwnd(hwnd) else {
        return;
    };
    let ctx = ctx(hwnd);
    send(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for (find, rep) in &ctx.pairs {
        let text = wide(&display_pair(find, rep));
        send(list, LB_ADDSTRING, WPARAM(0), LPARAM(text.as_ptr() as isize));
    }
    let sel = sel.unwrap_or_else(|| ctx.pairs.len().wrapping_sub(1));
    if sel < ctx.pairs.len() {
        send(list, LB_SETCURSEL, WPARAM(sel), LPARAM(0));
    }
}

/// Loads the selected pair back into the entry edits (a list double-click).
unsafe fn load_selected(hwnd: HWND) {
    let Some(sel) = list_sel(hwnd) else {
        return;
    };
    let ctx = ctx(hwnd);
    if let Some((find, rep)) = ctx.pairs.get(sel) {
        if let Ok(edit) = GetDlgItem(Some(hwnd), ID_FIND) {
            let _ = send(edit, WM_SETTEXT, WPARAM(0), LPARAM(wide(find).as_ptr() as isize));
        }
        if let Ok(edit) = GetDlgItem(Some(hwnd), ID_REP) {
            let _ = send(edit, WM_SETTEXT, WPARAM(0), LPARAM(wide(rep).as_ptr() as isize));
        }
    }
}

/// The find options the three checkboxes name.
unsafe fn read_opts(hwnd: HWND) -> FindOptions {
    FindOptions {
        case_sensitive: IsDlgButtonChecked(hwnd, ID_CASE) != 0,
        whole_word: IsDlgButtonChecked(hwnd, ID_WORD) != 0,
        regex: IsDlgButtonChecked(hwnd, ID_REGEX) != 0,
        backward: false,
    }
}

unsafe fn scope_all(hwnd: HWND) -> bool {
    IsDlgButtonChecked(hwnd, ID_SCOPE_ALL) != 0
}

/// The box's own caption, from the `[DLG208]` title row.
fn box_title() -> String {
    trs(DLG_TITLE)
}

/// The report the 统计 and run buttons show: one line per pair, then the
/// total and the run summary.
fn stats_text(lines: &[String], total: usize, docs: usize) -> String {
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&lt(40141, "合计:%d").replace("%d", &total.to_string()));
    out.push('\n');
    out.push_str(
        &lfmt(40142, &[&total.to_string(), &docs.to_string()])
            .unwrap_or_else(|| format!("已替换:{total} 处, 文档:{docs} 个")),
    );
    out
}

unsafe fn show_stats(hwnd: HWND, lines: &[String], total: usize, docs: usize) {
    let text = wide(&stats_text(lines, total, docs));
    let title = wide(&box_title());
    MessageBoxW(Some(hwnd), PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK);
}

unsafe fn show_error(hwnd: HWND, text: &str) {
    let text = wide(text);
    let title = wide(&box_title());
    MessageBoxW(Some(hwnd), PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK);
}

/// The 导出/导入 common dialog: one filter row from the language table
/// (40143, `配对文件 (*.txt)|*.txt|…`), pipe-separated like the shell wants.
unsafe fn pair_file_dialog(hwnd: HWND, save: bool) -> Option<String> {
    let mut file = [0u16; 520];
    let filter: Vec<u16> = lt(40143, "配对文件 (*.txt)|*.txt|所有文件 (*.*)|*.*")
        .replace('|', "\0")
        .encode_utf16()
        .chain([0, 0])
        .collect();
    let title: Vec<u16> = wide(&box_title());
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    let ok = if save {
        ofn.Flags |= OFN_OVERWRITEPROMPT;
        GetSaveFileNameW(&mut ofn)
    } else {
        GetOpenFileNameW(&mut ofn)
    };
    if !ok.as_bool() {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(0);
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&file[..len]))
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
            let ctx = ctx(hwnd);
            localize_dialog(hwnd, 208);
            let dark = chrome::theme_is_dark(&ctx.theme);
            chrome::set_chrome_theme(&ctx.theme);
            chrome::dark_theme_children(hwnd, dark);
            chrome::title_bar(hwnd, dark);
            let _ = CheckRadioButton(hwnd, ID_SCOPE_CUR, ID_SCOPE_ALL, ID_SCOPE_CUR);
            refill_list(hwnd, None);
            if let Ok(edit) = GetDlgItem(Some(hwnd), ID_FIND) {
                let _ = SetFocus(Some(edit));
            }
            0 // the focus is set here, not by the dialog manager
        }
        WM_ERASEBKGND => {
            let st = ctx(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = ctx(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        WM_CTLCOLOREDIT => {
            let st = ctx(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.field_face, st.field)
        }
        WM_COMMAND => {
            handle_command(hwnd, wparam);
            // The run buttons edit the documents behind the box; make the
            // pairs list and the frame show the moment's truth.
            let _ = RedrawWindow(
                Some(hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN | RDW_ERASE,
            );
            let _ = UpdateWindow(hwnd);
            1
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, 0);
            1
        }
        WM_DESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            0
        }
        _ => 0,
    }
}

unsafe fn handle_command(hwnd: HWND, wparam: WPARAM) {
    let id = (wparam.0 & 0xFFFF) as i32;
    let note = (wparam.0 >> 16) as i32;
    // A list double-click loads the pair back into the entry edits.
    if id == ID_LIST && note == 2 {
        load_selected(hwnd);
        return;
    }
    if id == IDCANCEL.0 {
        let _ = EndDialog(hwnd, 0);
        return;
    }
    match id {
        ID_ADD => {
            let find = dlg_text(hwnd, ID_FIND);
            if find.trim().is_empty() {
                return;
            }
            let rep = dlg_text(hwnd, ID_REP);
            let ctx = ctx(hwnd);
            ctx.pairs.push((find, rep));
            refill_list(hwnd, None);
        }
        ID_MOD => {
            let Some(sel) = list_sel(hwnd) else {
                return;
            };
            let find = dlg_text(hwnd, ID_FIND);
            if find.trim().is_empty() {
                return;
            }
            let rep = dlg_text(hwnd, ID_REP);
            let ctx = ctx(hwnd);
            if sel < ctx.pairs.len() {
                ctx.pairs[sel] = (find, rep);
                refill_list(hwnd, Some(sel));
            }
        }
        ID_DEL => {
            let Some(sel) = list_sel(hwnd) else {
                return;
            };
            let ctx = ctx(hwnd);
            if sel < ctx.pairs.len() {
                ctx.pairs.remove(sel);
                refill_list(hwnd, Some(sel));
            }
        }
        ID_UP | ID_DOWN => {
            let Some(sel) = list_sel(hwnd) else {
                return;
            };
            let ctx = ctx(hwnd);
            let to = if id == ID_UP {
                sel.checked_sub(1)
            } else if sel + 1 < ctx.pairs.len() {
                Some(sel + 1)
            } else {
                None
            };
            let Some(to) = to else {
                return;
            };
            ctx.pairs.swap(sel, to);
            refill_list(hwnd, Some(to));
        }
        ID_EXPORT => {
            let Some(path) = pair_file_dialog(hwnd, true) else {
                return;
            };
            let ctx = ctx(hwnd);
            let body: String = ctx
                .pairs
                .iter()
                .map(|(f, r)| format!("{}\t{}\n", escape_field(f), escape_field(r)))
                .collect();
            if std::fs::write(&path, body).is_err() {
                // 15206: 无法保存该文件,请确认您是否具有权限且该文件未被锁定或者是只读!\n%s\n%s
                let text = lfmt(15206, &[&path, ""]).unwrap_or_else(|| {
                    format!("无法保存该文件!\n{path}")
                });
                show_error(hwnd, &text);
            }
        }
        ID_IMPORT => {
            let Some(path) = pair_file_dialog(hwnd, false) else {
                return;
            };
            let Ok(body) = std::fs::read_to_string(&path) else {
                return;
            };
            let ctx = ctx(hwnd);
            for line in body.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let (f, r) = match line.split_once('\t') {
                    Some((f, r)) => (unescape_field(f), unescape_field(r)),
                    None => (unescape_field(line), String::new()),
                };
                if !f.trim().is_empty() {
                    ctx.pairs.push((f, r));
                }
            }
            refill_list(hwnd, None);
        }
        ID_SAVE => {
            let ctx = ctx(hwnd);
            let pairs = ctx.pairs.clone();
            (*ctx.state).batch_save_pairs(&pairs);
        }
        ID_STATS => {
            let ctx = ctx(hwnd);
            let pairs = ctx.pairs.clone();
            let opts = read_opts(hwnd);
            let all = scope_all(hwnd);
            let (lines, total, docs) = (*ctx.state).batch_stats(&pairs, opts, all);
            show_stats(hwnd, &lines, total, docs);
        }
        ID_REPLACE_ALL | ID_REPLACE_SEL => {
            let ctx = ctx(hwnd);
            let pairs = ctx.pairs.clone();
            let opts = read_opts(hwnd);
            let all = scope_all(hwnd);
            let sel_only = id == ID_REPLACE_SEL;
            let (lines, total, docs) =
                (*ctx.state).batch_run(ctx.frame, &pairs, opts, all, sel_only);
            show_stats(hwnd, &lines, total, docs);
        }
        _ => {}
    }
}
