//! The native 自定义日期/时间 dialog, which the 自定义日期/时间 command opens with `DialogBoxParamW`. The dialog object holds the two
//! `[DateTime]` strings it read from `setting.ini`; **one** edit shows whichever of them the combo has selected, and 确定
//! commits the edit back into that field and writes **both** fields out. The combo's items are not localised: the
//! date-time dialog init adds the literals `Date` and `Time` and selects 0, so the dialog opens on the date format. The
//! preview *is* a formatter. An empty string earns `GetDateFormatW(lcid, DATE_LONGDATE, …)` / `GetTimeFormatW(lcid,
//! TIME_NOFORMAT, …)` instead. The same routine runs on the insert path — its callers include the 日期 and 时间 insert
//! commands — so 预览 and what lands in the document are produced by [`preview_text`] here. Geometry comes from the
//! dialog template rather than guesswork: the 自定义日期/时间的格式 box is 209×72 dialog units with the seven items listed below.
//! Divided by that 1.25 scale this is a uniform **1.6 logical px per dialog unit**, which is what the rectangles here
//! are; the window's own 436×191 becomes 349×153. Control 1202 is the dropdown arrow at the edit's right edge. The
//! date-time dialog init gives it a 9-point `Marlett` font and captions it with the single character `4`, which in
//! Marlett is the down arrow. The other six controls get the app's UI font (`uifont::create`) rather than the 8 pt
//! `Ms Shell Dlg` this window used to hardcode, so they read the same size as the other dialogs. The dropdown click
//! handles its click: load the date/time token menu, take its submenu, `TrackPopupMenu` it at the button's bottom-left
//! corner with `TPM_RETURNCMD | TPM_NONOTIFY`, and insert the chosen token into edit 1201 at the caret. The nineteen
//! rows and their captions are [`TOKEN_ROWS`]; the font pass is explicit because these children come from
//! `CreateWindowExW`, which no dialog manager is filling in.

#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Globalization::{GetDateFormatW, GetTimeFormatW, GetUserDefaultLCID};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, FONT_CHARSET, GetSysColorBrush, HFONT,
    LOGFONTW,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::{EM_GETSEL, EM_REPLACESEL, EM_SCROLLCARET, EM_SETSEL};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BN_CLICKED, BS_DEFPUSHBUTTON, CB_ADDSTRING, CB_SETCURSEL, CBN_SELCHANGE,
    CBS_DROPDOWNLIST, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, EN_CHANGE, ES_AUTOHSCROLL,
    ES_READONLY, GWLP_USERDATA, GetDlgItem, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, IDCANCEL, IDOK, MF_SEPARATOR, MF_STRING, PostQuitMessage, RegisterClassW,
    SendMessageW, SetWindowLongPtrW, SetWindowTextW, TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_SETFONT, WNDCLASSW, WS_BORDER,
    WS_CAPTION, WS_EX_DLGMODALFRAME, WS_GROUP, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
};
use windows::core::{PCWSTR, w};

use crate::win32::prompt::{
    CLASS_BUTTON, CLASS_COMBO, CLASS_EDIT, CLASS_STATIC, add_child, run_modal_loop, show_modal,
    wide,
};
use crate::win32::view::instance;

/// The dialog's window class, registered once like the prompt box's.
const DLG_CLASS: PCWSTR = w!("DogEditDateTimeDlg");
/// The caption the menu bar shows for this command, which is also
/// this box's title.
const TITLE: &str = "自定义日期/时间";

/// Combo (1200), its edit (1201), the arrow button (1202), the 预览: label (3000)
/// and the read-only preview (1203)
/// its children.
const ID_COMBO: i32 = 1_200;
const ID_EDIT: i32 = 1_201;
const ID_ARROW: i32 = 1_202;
const ID_PREVIEW: i32 = 1_203;
const ID_LABEL: i32 = 3_000;

/// Which of the two fields the edit is showing, i.e. `CB_GETCURSEL`'s answer.
const SHOWN_DATE: u32 = 0;
const SHOWN_TIME: u32 = 1;

/// The template's items, converted at 1.6 logical px per dialog unit and shifted
/// into client coordinates: `DLU × 1.6` for x and y, with the caption and border
/// accounted for by the window itself rather than by these numbers.
const RECT_COMBO: (i32, i32, i32, i32) = (11, 11, 224, 48);
const RECT_EDIT: (i32, i32, i32, i32) = (11, 37, 195, 22);
const RECT_ARROW: (i32, i32, i32, i32) = (211, 37, 24, 22);
const RECT_LABEL: (i32, i32, i32, i32) = (11, 64, 112, 18);
const RECT_PREVIEW: (i32, i32, i32, i32) = (11, 82, 224, 22);
const RECT_OK: (i32, i32, i32, i32) = (243, 11, 80, 22);
const RECT_CANCEL: (i32, i32, i32, i32) = (243, 37, 80, 22);
/// The dialog's window rect at 100%: 436×191 physical at 120 px/inch.
const DLG_W: i32 = 349;
const DLG_H: i32 = 153;

/// The arrow button's 9 pt Marlett; the rest of the controls use the app's UI
/// font (`uifont::create`), not the 8 pt shell dialog font this window used to
/// hardcode.
const MARLETT_FONT_TENTHS: i32 = 90;

/// One row of the arrow's menu: a date/time token's command id and caption, and
/// the text the dropdown click inserts for it. `id` 0 marks a separator, which is
/// where the template puts its eight `---` rows.
struct TokenRow {
    id: u32,
    label: &'static str,
    token: &'static str,
}

const SEPARATOR: TokenRow = TokenRow {
    id: 0,
    label: "",
    token: "",
};

/// The token list in the order the token expander tests them, and the menu that
/// offers nineteen of them (`${w}` has no row, and the padded form precedes the
/// bare one for month, day, hour, minute and second).
const TOKENS: [&str; 20] = [
    "${y}", "${y2}", "${M}", "${M2}", "${d}", "${d2}", "${h}", "${h2}", "${H}", "${H2}", "${m}",
    "${m2}", "${s}", "${s2}", "${w}", "${a}", "${A}", "${j}", "${u}", "${p}",
];

const TOKEN_ROWS: &[TokenRow] = &[
    TokenRow {
        id: 1220,
        label: "Year (0000)",
        token: "${y}",
    },
    TokenRow {
        id: 1221,
        label: "Year (00)",
        token: "${y2}",
    },
    SEPARATOR,
    TokenRow {
        id: 1222,
        label: "Month (00)",
        token: "${M2}",
    },
    TokenRow {
        id: 1223,
        label: "Month (0)",
        token: "${M}",
    },
    SEPARATOR,
    TokenRow {
        id: 1224,
        label: "Day (00)",
        token: "${d2}",
    },
    TokenRow {
        id: 1225,
        label: "Day (0)",
        token: "${d}",
    },
    SEPARATOR,
    TokenRow {
        id: 1226,
        label: "Hour (00)",
        token: "${h2}",
    },
    TokenRow {
        id: 1227,
        label: "Hour (0)",
        token: "${h}",
    },
    TokenRow {
        id: 1228,
        label: "Hour 24 (00)",
        token: "${H2}",
    },
    TokenRow {
        id: 1229,
        label: "Hour 24 (0)",
        token: "${H}",
    },
    TokenRow {
        id: 1236,
        label: "AM/PM",
        token: "${p}",
    },
    SEPARATOR,
    TokenRow {
        id: 1230,
        label: "Minute (00)",
        token: "${m2}",
    },
    TokenRow {
        id: 1231,
        label: "Minute (0)",
        token: "${m}",
    },
    SEPARATOR,
    TokenRow {
        id: 1232,
        label: "Second (00)",
        token: "${s2}",
    },
    TokenRow {
        id: 1233,
        label: "Second (0)",
        token: "${s}",
    },
    SEPARATOR,
    TokenRow {
        id: 1234,
        label: "Week (Full name)",
        token: "${A}",
    },
    TokenRow {
        id: 1235,
        label: "Week (Short name)",
        token: "${a}",
    },
    SEPARATOR,
    TokenRow {
        id: 1237,
        label: "Day of year",
        token: "${j}",
    },
    TokenRow {
        id: 1238,
        label: "Week of year",
        token: "${u}",
    },
];

/// The dialog's two strings plus how the loop ends, held by the caller and
/// reached from `dlg_proc` through `GWLP_USERDATA`.
struct DlgState {
    /// `[DateTime] Date`, initialised when the date-time dialog opens.
    date: String,
    /// `[DateTime] Time`.
    time: String,
    shown: u32,
    /// The pair 确定 committed; `None` for 取消 and for the caption's X.
    result: Option<(String, String)>,
    destroyed: bool,
}

impl DlgState {
    /// Puts the edit's text into the current value.
    unsafe fn commit(&mut self, hwnd: HWND) {
        let text = item_text(hwnd, ID_EDIT);
        if self.shown == SHOWN_TIME {
            self.time = text;
        } else {
            self.date = text;
        }
    }
}

/// Runs the dialog modally. `None` means it was cancelled; `Some((date, time))`
/// is the pair to persist as `[DateTime] Date` / `Time`.
pub(super) fn dialog(owner: HWND, date: &str, time: &str) -> Option<(String, String)> {
    let mut state = DlgState {
        date: date.to_string(),
        time: time.to_string(),
        shown: SHOWN_DATE,
        result: None,
        destroyed: false,
    };
    unsafe {
        register_class();
        let dpi = GetDpiForWindow(owner).max(96) as f32 / 96.0;
        let dpi_i = GetDpiForWindow(owner).max(96) as i32;
        let scale = |v: i32| (v as f32 * dpi).round() as i32;
        // They live as long as the window: `dialog` blocks until it is gone.
        // `CreateWindowExW` gives the children the stock system font, which is
        // 8 pt and reads small next to the rest of the chrome; the UI font is
        // what every other dialog hands its children.
        let shell = crate::win32::uifont::create(dpi_i);
        let marlett = make_font("Marlett", MARLETT_FONT_TENTHS, dpi_i);

        let title16 = wide(crate::win32::lang::dlg(259, 0).unwrap_or(TITLE));
        let date16 = wide("Date");
        let time16 = wide("Time");
        let label16 = wide("预览:");
        let ok16 = wide("确定");
        let cancel16 = wide("取消");
        let state_ptr = &mut state as *mut DlgState as *const core::ffi::c_void;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            DLG_CLASS,
            PCWSTR::from_raw(title16.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            scale(DLG_W),
            scale(DLG_H),
            Some(owner),
            None,
            Some(instance()),
            Some(state_ptr),
        ) else {
            let _ = DeleteObject(shell.into());
            let _ = DeleteObject(marlett.into());
            return None;
        };

        let combo = add_child(
            hwnd,
            CLASS_COMBO,
            PCWSTR::null(),
            ID_COMBO,
            WINDOW_STYLE(CBS_DROPDOWNLIST as u32 | WS_TABSTOP.0 | WS_GROUP.0),
            RECT_COMBO,
            dpi,
        );
        let edit = add_child(
            hwnd,
            CLASS_EDIT,
            PCWSTR::null(),
            ID_EDIT,
            WINDOW_STYLE(WS_BORDER.0 | ES_AUTOHSCROLL as u32 | WS_TABSTOP.0),
            RECT_EDIT,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::null(),
            ID_ARROW,
            WINDOW_STYLE::default(),
            RECT_ARROW,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_STATIC,
            PCWSTR::from_raw(label16.as_ptr()),
            ID_LABEL,
            WINDOW_STYLE::default(),
            RECT_LABEL,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_EDIT,
            PCWSTR::null(),
            ID_PREVIEW,
            WINDOW_STYLE(WS_BORDER.0 | ES_AUTOHSCROLL as u32 | ES_READONLY as u32),
            RECT_PREVIEW,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(ok16.as_ptr()),
            IDOK.0 as i32,
            // The template carries `BS_DEFPUSHBUTTON` on 确定 (`0x50010001`), and
            // that is what lets Enter close the dialog.
            WINDOW_STYLE(BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0),
            RECT_OK,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(cancel16.as_ptr()),
            IDCANCEL.0 as i32,
            WS_TABSTOP,
            RECT_CANCEL,
            dpi,
        );

        // the date-time dialog init's first two steps, in its own order: the arrow gets
        // Marlett and the caption `4`, everything else the app's UI font. The
        // id list below deliberately leaves ID_ARROW out so its glyph font wins.
        crate::win32::lang::localize_dialog(hwnd, 259);
        if let Ok(a) = GetDlgItem(Some(hwnd), ID_ARROW) {
            set_font(a, marlett);
            let four = wide("4");
            let _ = SetWindowTextW(a, PCWSTR::from_raw(four.as_ptr()));
        }
        for id in [
            ID_COMBO,
            ID_EDIT,
            ID_LABEL,
            ID_PREVIEW,
            IDOK.0 as i32,
            IDCANCEL.0 as i32,
        ] {
            if let Ok(item) = GetDlgItem(Some(hwnd), id) {
                set_font(item, shell);
            }
        }

        // The combo is filled after creation because a `CreateWindowExW` text is
        // the window's caption, not a list: `CB_ADDSTRING` twice, `CB_SETCURSEL`
        // 0, then the Date value goes into the edit.
        if let Some(c) = combo {
            SendMessageW(
                c,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(date16.as_ptr() as isize)),
            );
            SendMessageW(
                c,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(time16.as_ptr() as isize)),
            );
            SendMessageW(c, CB_SETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        if let Some(e) = edit {
            let v = wide(&state.date);
            let _ = SetWindowTextW(e, PCWSTR::from_raw(v.as_ptr()));
        }
        refresh(hwnd);

        show_modal(hwnd, owner);
        if let Some(e) = edit {
            let _ = SetFocus(Some(e));
        }
        run_modal_loop(hwnd, owner, std::ptr::addr_of!(state.destroyed));
        let _ = DeleteObject(shell.into());
        let _ = DeleteObject(marlett.into());
        state.result.take()
    }
}

/// the preview edit carries the typed format with its tokens
/// expanded, and an empty one carries the locale's own date / time.
unsafe fn refresh(hwnd: HWND) {
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DlgState;
    if state.is_null() {
        return;
    }
    let text = preview_text((*state).shown == SHOWN_DATE, &item_text(hwnd, ID_EDIT));
    if let Ok(preview) = GetDlgItem(Some(hwnd), ID_PREVIEW) {
        let w = wide(&text);
        let _ = SetWindowTextW(preview, PCWSTR::from_raw(w.as_ptr()));
    }
}

/// What 1417 / 1418 insert, and what 预览 shows, for one `[DateTime]` value: the token
/// expander expands every `${...}` token in it against the local time, and an empty
/// string instead takes `GetDateFormatW(lcid, DATE_LONGDATE, …)` /
/// `GetTimeFormatW(lcid, TIME_NOFORMAT, …)`. It is the same LCID [`crate`] reads.
pub(super) fn preview_text(date: bool, fmt: &str) -> String {
    if fmt.is_empty() {
        return locale_form(date);
    }
    expand(fmt, &unsafe { GetLocalTime() })
}

/// the token expander's loop: every token in [`TOKENS`] is looked for in the format
/// and replaced wherever it appears with its value at `t`.
fn expand(fmt: &str, t: &SYSTEMTIME) -> String {
    let mut out = fmt.to_string();
    for token in TOKENS {
        if out.contains(token) {
            out = out.replace(token, &token_value(t, token));
        }
    }
    out
}

/// One `${...}` token's replacement. The numeric fields are `%d` / `%02d` of the
/// broken-out time (`${y2}` is the year string with its first two characters
/// dropped, which is what the token expander does by advancing the buffer four
/// bytes); `${w}`, `${j}` and `${u}` are `strftime`'s `%w`, `%j` and `%U` — day of
/// week with Sunday as 0, the 1-based day of the year, and the Sunday-first week
/// number whose leading partial week is 0; and `${a}`, `${A}`, `${p}` are the
/// locale's weekday and AM/PM names
/// `strftime`-style formatting.
fn token_value(t: &SYSTEMTIME, token: &str) -> String {
    let year = t.wYear.max(0) as u32;
    match token {
        "${y}" => year.to_string(),
        "${y2}" => {
            let s = year.to_string();
            if s.chars().count() >= 4 {
                s.chars().skip(2).collect()
            } else {
                s
            }
        }
        "${M}" => t.wMonth.to_string(),
        "${M2}" => format!("{:02}", t.wMonth),
        "${d}" => t.wDay.to_string(),
        "${d2}" => format!("{:02}", t.wDay),
        // Midnight reads 0 rather than 12.
        "${h}" => hour12(t).to_string(),
        "${h2}" => format!("{:02}", hour12(t)),
        "${H}" => t.wHour.to_string(),
        "${H2}" => format!("{:02}", t.wHour),
        "${m}" => t.wMinute.to_string(),
        "${m2}" => format!("{:02}", t.wMinute),
        "${s}" => t.wSecond.to_string(),
        "${s2}" => format!("{:02}", t.wSecond),
        "${w}" => t.wDayOfWeek.to_string(),
        "${a}" => locale_name(true, "ddd", t),
        "${A}" => locale_name(true, "dddd", t),
        "${j}" => format!("{:03}", day_of_year(t)),
        "${u}" => format!("{:02}", week_of_year(t)),
        "${p}" => locale_name(false, "tt", t),
        _ => String::new(),
    }
}

fn hour12(t: &SYSTEMTIME) -> u16 {
    if t.wHour < 12 { t.wHour } else { t.wHour - 12 }
}

const DAYS_BEFORE: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

/// 1-based, as `%j` wants it.
fn day_of_year(t: &SYSTEMTIME) -> u32 {
    let month = t.wMonth.clamp(1, 12) as usize - 1;
    let year = t.wYear as i32;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    DAYS_BEFORE[month] + t.wDay as u32 + u32::from(leap && month > 1)
}

/// `%U`: weeks start on Sunday, and the days before the first Sunday of the year
/// are still week 0.
fn week_of_year(t: &SYSTEMTIME) -> u32 {
    (day_of_year(t) - 1 + 7 - t.wDayOfWeek as u32) / 7
}

/// The locale's own long date / no-format time, for an empty `[DateTime]`.
fn locale_form(date: bool) -> String {
    let t = unsafe { GetLocalTime() };
    let mut buf = [0u16; 128];
    let written = unsafe {
        let lcid = GetUserDefaultLCID();
        if date {
            GetDateFormatW(lcid, 2, Some(&t), PCWSTR::null(), Some(&mut buf))
        } else {
            GetTimeFormatW(lcid, 0, Some(&t), PCWSTR::null(), Some(&mut buf))
        }
    };
    read_formatted(written, &buf)
}

/// A date or time pattern through the same two APIs: `ddd` / `dddd` give the
/// weekday's short and full name, `tt` the AM/PM designator.
fn locale_name(date: bool, pattern: &str, t: &SYSTEMTIME) -> String {
    let mut buf = [0u16; 64];
    let p = wide(pattern);
    let written = unsafe {
        let lcid = GetUserDefaultLCID();
        if date {
            GetDateFormatW(
                lcid,
                0,
                Some(t),
                PCWSTR::from_raw(p.as_ptr()),
                Some(&mut buf),
            )
        } else {
            GetTimeFormatW(
                lcid,
                0,
                Some(t),
                PCWSTR::from_raw(p.as_ptr()),
                Some(&mut buf),
            )
        }
    };
    read_formatted(written, &buf)
}

/// The text a formatter wrote. `GetDateFormatW` / `GetTimeFormatW` report their
/// own terminating NUL in the count, and that NUL has to go: it travels into the
/// `${a}` / `${A}` / `${p}` replacement, and `SetWindowTextW` stops at it, which
/// silently cuts the rest of the 预览 string — and the rest of an inserted stamp
/// — off after the first name token.
fn read_formatted(written: i32, buf: &[u16]) -> String {
    if written <= 0 {
        return String::new();
    }
    let end = (written as usize).min(buf.len());
    let body = &buf[..end];
    let body = body.strip_suffix(&[0u16]).unwrap_or(body);
    String::from_utf16_lossy(body)
}

/// the dropdown click: the arrow's menu, tracked at the button's bottom-left corner,
/// with the chosen token inserted at the edit's caret.
unsafe fn show_token_menu(hwnd: HWND) {
    let Some(button) = GetDlgItem(Some(hwnd), ID_ARROW).ok() else {
        return;
    };
    let mut rect = RECT::default();
    if GetWindowRect(button, &mut rect).is_err() {
        return;
    }
    let Ok(menu) = CreatePopupMenu() else { return };
    let mut complete = true;
    for row in TOKEN_ROWS {
        let label = wide(row.label);
        let flags = if row.id == 0 { MF_SEPARATOR } else { MF_STRING };
        let text = if row.id == 0 {
            PCWSTR::null()
        } else {
            PCWSTR::from_raw(label.as_ptr())
        };
        if AppendMenuW(menu, flags, row.id as usize, text).is_err() {
            complete = false;
            break;
        }
    }
    if !complete {
        let _ = DestroyMenu(menu);
        return;
    }
    // the dropdown click passes the owner's rect through untouched; the dialog owns
    // the foreground here because `show_modal` activated it.
    let chosen = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY,
        rect.left,
        rect.bottom,
        None,
        hwnd,
        None,
    )
    .0;
    let _ = DestroyMenu(menu);
    let Some(row) = TOKEN_ROWS.iter().find(|r| r.id == chosen as u32) else {
        return;
    };
    if let Ok(edit) = GetDlgItem(Some(hwnd), ID_EDIT) {
        insert_at_caret(edit, row.token);
    }
}

/// Which ends in `EN_CHANGE`, so the preview follows.
unsafe fn insert_at_caret(edit: HWND, token: &str) {
    let (mut start, mut end) = (0i32, 0i32);
    SendMessageW(
        edit,
        EM_GETSEL,
        Some(WPARAM(&raw mut start as usize)),
        Some(LPARAM(&raw mut end as isize)),
    );
    if start != end {
        let empty = [0u16; 1];
        SendMessageW(
            edit,
            EM_REPLACESEL,
            Some(WPARAM(0)),
            Some(LPARAM(empty.as_ptr() as isize)),
        );
    }
    let caret = start.max(0);
    let _ = SendMessageW(
        edit,
        EM_SETSEL,
        Some(WPARAM(caret as usize)),
        Some(LPARAM(caret as isize)),
    );
    let _ = SendMessageW(edit, EM_SCROLLCARET, None, None);
    let text = wide(token);
    let _ = SendMessageW(
        edit,
        EM_REPLACESEL,
        Some(WPARAM(0)),
        Some(LPARAM(text.as_ptr() as isize)),
    );
    let _ = SetFocus(Some(edit));
}

/// A font at `tenths` / 10 of a point, sized for the dialog's DPI
/// does the same arithmetic against the screen's `LOGPIXELSY`.
unsafe fn make_font(face: &str, tenths: i32, dpi: i32) -> HFONT {
    let mut lf = LOGFONTW::default();
    lf.lfHeight = -((tenths * dpi + 360) / 720);
    // A charset of 1 (DEFAULT_CHARSET), not ANSI_CHARSET: the latter makes GDI
    // pick a substitute face, and Marlett's arrow captions then render as the
    // digits they are coded as.
    lf.lfCharSet = FONT_CHARSET(1);
    for (c, slot) in face.encode_utf16().zip(lf.lfFaceName.iter_mut()) {
        *slot = c;
    }
    CreateFontIndirectW(&lf)
}

unsafe fn set_font(item: HWND, font: HFONT) {
    let _ = SendMessageW(
        item,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
}

unsafe fn item_text(hwnd: HWND, id: i32) -> String {
    let Ok(item) = GetDlgItem(Some(hwnd), id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(item);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(item, &mut buf);
    buf.truncate(n.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

unsafe fn register_class() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(dlg_proc),
            hInstance: instance(),
            lpszClassName: DLG_CLASS,
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            eprintln!("date/time dialog class registration failed");
        }
    });
}

unsafe extern "system" fn dlg_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return LRESULT(0);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DlgState;
    if msg == WM_COMMAND {
        let id = (wparam.0 & 0xFFFF) as i32;
        let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
        if id == IDOK.0 || id == IDCANCEL.0 {
            if id == IDOK.0 && !state.is_null() {
                let st = &mut *state;
                st.commit(hwnd);
                st.result = Some((st.date.clone(), st.time.clone()));
            }
            DestroyWindow(hwnd).ok();
            return LRESULT(0);
        }
        if id == ID_ARROW && code == BN_CLICKED {
            show_token_menu(hwnd);
            return LRESULT(0);
        }
        if id == ID_COMBO && code == CBN_SELCHANGE && !state.is_null() {
            // hand the typed text to the field it belonged to,
            // then reload the edit from the other one.
            let st = &mut *state;
            st.commit(hwnd);
            st.shown ^= 1;
            let v = wide(if st.shown == SHOWN_TIME {
                &st.time
            } else {
                &st.date
            });
            if let Ok(e) = GetDlgItem(Some(hwnd), ID_EDIT) {
                let _ = SetWindowTextW(e, PCWSTR::from_raw(v.as_ptr()));
            }
            refresh(hwnd);
            return LRESULT(0);
        }
        if id == ID_EDIT && code == EN_CHANGE {
            refresh(hwnd);
            return LRESULT(0);
        }
    } else if msg == WM_CLOSE {
        DestroyWindow(hwnd).ok();
        return LRESULT(0);
    } else if msg == WM_DESTROY {
        if !state.is_null() {
            (*state).destroyed = true;
        }
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-01-04, a Sunday, at 13:05:07 — chosen so `${w}`=0, `${j}`=004 and
    /// `${u}`=01, the three values computed rather than read off the clock.
    fn sample(hour: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: 2026,
            wMonth: 1,
            wDayOfWeek: 0,
            wDay: 4,
            wHour: hour,
            wMinute: 5,
            wSecond: 7,
            ..Default::default()
        }
    }

    #[test]
    fn every_token_expands() {
        let t = sample(13);
        let got = expand(
            "${y} ${y2} ${M}/${M2} ${d}/${d2} ${h}:${h2} ${H}:${H2} ${m}:${m2} \
             ${s}:${s2} ${w} ${j} ${u}",
            &t,
        );
        assert_eq!(got, "2026 26 1/01 4/04 1:01 13:13 5:05 7:07 0 004 01");
    }

    #[test]
    fn midnight_stays_zero() {
        assert_eq!(expand("${h}${H}", &sample(0)), "00");
    }

    /// A token that is not in the table is left alone, and one that repeats is
    /// replaced at every position.
    #[test]
    fn unknown_tokens_survive_and_repeats_expand() {
        assert_eq!(expand("${HH}-${M2}-${M2}", &sample(9)), "${HH}-01-01");
    }

    /// The three names come from `GetDateFormatW` / `GetTimeFormatW`, which report
    /// their own terminator in the character count. Keeping it puts a NUL in the
    /// middle of the preview string, and `SetWindowTextW` then drops everything
    /// after the first weekday name — what the live dialog did before this.
    #[test]
    fn name_tokens_carry_no_terminator() {
        let t = sample(9);
        for token in ["${a}", "${A}", "${p}"] {
            let v = token_value(&t, token);
            assert!(!v.contains('\0'), "{token} expanded to {v:?}");
        }
        assert!(!locale_form(true).contains('\0'));
        assert!(!locale_form(false).contains('\0'));
    }
}
