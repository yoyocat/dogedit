//! 切换文件类型 — the box behind 文档 → 切换文件类型 (`cm_doc_changetype`).

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlgtpl::{self, Class, Item};
use dg_render::theme::Theme;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT, HGDIOBJ,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateDialogIndirectParamW, DefWindowProcW, DestroyWindow, EN_CHANGE, GetClientRect, GetDlgItem,
    GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GWL_STYLE, GW_CHILD,
    GW_HWNDNEXT, GWLP_USERDATA, LB_ADDSTRING, LB_GETCOUNT, LB_GETCURSEL, LB_GETITEMDATA,
    LB_RESETCONTENT, LB_SETCURSEL, LB_SETITEMDATA, PostQuitMessage, SendMessageW,
    SetWindowLongPtrW, DLGTEMPLATE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG, WM_SETFONT,
    WS_CLIPCHILDREN,
};

use super::customdraw;
use super::prompt::{run_modal_loop, show_modal};
use super::view::instance;
use super::window::to_colorref;
use super::{chrome, uifont};

// ---------------------------------------------------------------------------
// 资源 255，逐字段
// ---------------------------------------------------------------------------

/// `0x90C808CC` 去掉 `WS_VISIBLE`：对话框先建好、摆好、再显示（`show_modal`），
/// 不让管理器那版字体推导出来的布局闪一下。剩下的位是
/// `WS_POPUP|WS_CAPTION|WS_SYSMENU|DS_MODALFRAME|DS_SETFONT|DS_FIXEDSYS|
/// DS_3DLOOK|DS_CENTER`。
const DLG_STYLE: u32 = 0x80C8_08CC;
const DLG_W: u16 = 213;
const DLG_H: u16 = 296;
/// `zh-CN.lng` 里 切换文件类型 框的 `0` 项（窗口标题）。
const DLG_TITLE: &str = "切换文件类型";
/// `zh-CN.lng` 里同一框的 `1` / `2` / `1201` 三项（按钮标题）。
const OK_TEXT: &str = "确定";
const CANCEL_TEXT: &str = "取消";
const CONFIG_TEXT: &str = "配置";

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const ID_LIST: i32 = 1200;
const ID_CONFIG: i32 = 1201;
const ID_EDIT: i32 = 1202;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_LISTBOX: u16 = 0x0083;

/// `LBN_DBLCLK`，`WM_COMMAND` 通知字的高位。
const LBN_DBLCLK: u16 = 2;

/// 「默认」 —— 语言字符串 15030，列表的第一项（没有语法）。
pub(super) const DEFAULT_LABEL: &str = "默认";

/// 五个子控件，按资源顺序；矩形和样式都是资源自己的字面量，标题取 zh-CN.lng。
const ITEMS: &[Item] = &[
    // WS_CHILD|WS_VISIBLE|WS_BORDER|WS_TABSTOP|ES_AUTOHSCROLL
    Item { id: ID_EDIT, class: Class::Atom(ATOM_EDIT), style: 0x5081_0080, x: 7, y: 7, w: 145, h: 14, label: "" },
    // WS_CHILD|WS_VISIBLE|WS_TABSTOP|BS_DEFPUSHBUTTON
    Item { id: IDOK, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0001, x: 157, y: 7, w: 50, h: 14, label: OK_TEXT },
    // WS_CHILD|WS_VISIBLE|WS_TABSTOP
    Item { id: IDCANCEL, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 157, y: 23, w: 50, h: 14, label: CANCEL_TEXT },
    // WS_CHILD|WS_VISIBLE|WS_VSCROLL|WS_BORDER|WS_TABSTOP|LBS_NOTIFY|LBS_SORT|
    // LBS_NOINTEGRALHEIGHT
    Item { id: ID_LIST, class: Class::Atom(ATOM_LISTBOX), style: 0x50A1_0103, x: 7, y: 24, w: 145, h: 265, label: "" },
    Item { id: ID_CONFIG, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 157, y: 39, w: 50, h: 14, label: CONFIG_TEXT },
];

/// 列表的一行：显示名 + 回到调用方的键。
///
/// `key == None` 是「默认」 —— 把文档的语法摘掉。其余是解析器包名
/// （`manifest.name`，`syntax_lang` 认它）。
pub(super) struct Entry {
    pub label: String,
    pub key: Option<String>,
}

/// 对话框的答案。`None`（[`show`] 的返回值）= 取消 / Esc / 标题栏 X。
pub(super) enum Outcome {
    /// 确定或双击。`None` = 选了「默认」。
    Pick(Option<String>),
    /// 配置：调用方接着开「语法着色」管理器（1459）。
    Config,
}

fn matches(label: &str, query: &str) -> bool {
    let mut hay = label.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|needle| hay.any(|c| c == needle))
}

/// DLGTEMPLATEEX，和 `encdlg` 同一套写法：EX 头、`DS_SETFONT` 字体块、
/// 然后每个控件「help id + ex style + style + rect + DWORD id + 0xFFFF
/// 类原子 + NUL 结尾标题 + extra count」。
fn build_template() -> Vec<u8> {
    dlgtpl::build_legacy(DLG_TITLE, DLG_STYLE, 0, DLG_W, DLG_H, 0, ITEMS)
}

// ---------------------------------------------------------------------------
// 对话框
// ---------------------------------------------------------------------------

struct Dlg {
    /// 行表，第 0 项是「默认」。列表控件按 `LBS_SORT` 自己排序，行号会变，
    /// 所以每行的 item data 存的是这里的下标。
    entries: Vec<Entry>,
    /// 当前文档的语法键（已经过 `resolve_grammar`），用来在重建后找回选中行。
    current: Option<String>,
    result: Option<Outcome>,
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
    font: HFONT,
    /// `WM_DESTROY` 置真：模态循环靠它区分「本框自己销毁」和「主窗收到
    /// WM_QUIT」。
    destroyed: bool,
}

unsafe fn state_mut(hwnd: HWND) -> &'static mut Dlg {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg)
}

/// 模态跑一遍。`None` = 取消；`Some(Pick(..))` = 确定；`Some(Config)` = 配置。
pub(super) fn show(
    frame: HWND,
    theme: &Theme,
    entries: &[Entry],
    current: Option<&str>,
) -> Option<Outcome> {
    let dpi = unsafe { GetDpiForWindow(frame).max(96) as i32 };
    let mut st = Dlg {
        entries: entries
            .iter()
            .map(|e| Entry {
                label: e.label.clone(),
                key: e.key.clone(),
            })
            .collect(),
        current: current.map(String::from),
        result: None,
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        ink: to_colorref(theme.toolbar_fg),
        face: to_colorref(theme.toolbar_bg),
        field_face: to_colorref(theme.toolbar_field),
        font: unsafe { uifont::create(dpi) },
        destroyed: false,
    };
    let param = LPARAM(&mut st as *mut Dlg as isize);
    let tmpl = build_template();
    let dlg = unsafe {
        CreateDialogIndirectParamW(
            Some(instance()),
            &*(tmpl.as_ptr() as *const DLGTEMPLATE),
            Some(frame),
            Some(dlg_proc),
            param,
        )
    };
    let Ok(dlg) = dlg else {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(st.bg.0));
            let _ = DeleteObject(HGDIOBJ(st.field.0));
            let _ = DeleteObject(HGDIOBJ(st.font.0));
        }
        return None;
    };
    unsafe {
        // 深色调色板，和 `encdlg` 同一套：控件和标题栏拿到暗色属性，对话框
        // 自己**不**送 `SetWindowTheme` —— 那会按字体重算 `#32770` 的边框，
        // 把右边的按钮列挤出客户区。
        let dark = chrome::theme_is_dark(theme);
        chrome::set_chrome_theme(theme);
        chrome::dark_theme_children(dlg, dark);
        chrome::title_bar(dlg, dark);
        // 资源没有 WS_CLIPCHILDREN：不补的话父窗口整块擦背景会盖掉子控件，
        // 只有变化过的那几个会重画。
        let style = GetWindowLongPtrW(dlg, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
        SetWindowLongPtrW(dlg, GWL_STYLE, style);
        show_modal(dlg, frame);
        run_modal_loop(dlg, frame, std::ptr::addr_of!(st.destroyed));
        let _ = DeleteObject(HGDIOBJ(st.bg.0));
        let _ = DeleteObject(HGDIOBJ(st.field.0));
        let _ = DeleteObject(HGDIOBJ(st.font.0));
    }
    st.result
}

/// `WM_INITDIALOG`：先换界面字体（模板那份 8 pt 太小），再填列表、选中当前
/// 语法、把光标放进过滤框。
unsafe fn init(dlg: HWND) -> isize {
    // 只换子控件的字，模板的 DS_SETFONT 不动 —— 它是 dialog unit 的基准，
    // 改了整版会重缩放（`encdlg` 的注释里量过）。
    let font = state_mut(dlg).font;
    let mut child = GetWindow(dlg, GW_CHILD).unwrap_or_default();
    while !child.0.is_null() {
        let _ = SendMessageW(child, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
    }
    refill(dlg);
    if let Ok(edit) = GetDlgItem(Some(dlg), ID_EDIT) {
        let _ = SetFocus(Some(edit));
    }
    1
}

/// 过滤框的当前文字。
unsafe fn edit_text(dlg: HWND) -> String {
    let Ok(edit) = GetDlgItem(Some(dlg), ID_EDIT) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(edit);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(edit, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 往列表里塞一行，item data 是 [`Dlg::entries`] 的下标。
unsafe fn add_row(list: HWND, label: &str, index: usize) -> i32 {
    let mut w: Vec<u16> = label.encode_utf16().chain([0]).collect();
    let idx = SendMessageW(
        list,
        LB_ADDSTRING,
        Some(WPARAM(0)),
        Some(LPARAM(w.as_mut_ptr() as isize)),
    )
    .0 as i32;
    if idx >= 0 {
        let _ = SendMessageW(
            list,
            LB_SETITEMDATA,
            Some(WPARAM(idx as usize)),
            Some(LPARAM(index as isize)),
        );
    }
    idx
}

/// 优先当前文档的语法，被过滤掉了就回落到第一条可见的行。
///
/// 单独拎出来是为了能测 —— 列表控件 `LBS_SORT` 会重排行号，真正的
/// 「条目 → 行号」映射只能在 `refill` 里拿到，但选谁不选谁是纯逻辑。
fn pick(entries: &[Entry], query: &str, current: Option<&str>) -> Option<usize> {
    let visible = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches(&e.label, query))
        .map(|(i, _)| i);
    let mut first = None;
    for i in visible {
        if entries[i].key.as_deref() == current {
            return Some(i);
        }
        first.get_or_insert(i);
    }
    first
}

///
/// 1. 清空；
/// 2. 逐条判断 `matches(label, query)`，命中的加进去；
/// 3. 选中「当前文档语法」那一行 —— 过滤把它滤掉了就选第 0 行。
///
/// 否则输入一个字符选中行就跳到别处了。
unsafe fn refill(dlg: HWND) {
    let st = state_mut(dlg);
    let Ok(list) = GetDlgItem(Some(dlg), ID_LIST) else {
        return;
    };
    let query = edit_text(dlg);
    let target = pick(&st.entries, &query, st.current.as_deref());
    let _ = SendMessageW(list, LB_RESETCONTENT, None, None);
    for (i, e) in st.entries.iter().enumerate() {
        if !matches(&e.label, &query) {
            continue;
        }
        add_row(list, &e.label, i);
    }
    // 行号**不能**用 `LB_ADDSTRING` 的返回值：`LBS_SORT` 会让后续插入把
    // 已插入的行整体挪位 —— 先加「默认」它返回 0，再加 Astro，「默认」就
    // 被挤到最后一行，那个 0 是过期的。填完按 item data 反查才准。
    let count = SendMessageW(list, LB_GETCOUNT, None, None).0 as i32;
    let mut row = -1;
    if let Some(want) = target {
        for i in 0..count {
            let data = SendMessageW(list, LB_GETITEMDATA, Some(WPARAM(i as usize)), None).0;
            if data as usize == want {
                row = i;
                break;
            }
        }
    }
    if row >= 0 {
        let _ = SendMessageW(list, LB_SETCURSEL, Some(WPARAM(row as usize)), None);
    }
}

/// 选中行对应的行表下标。`None` = 一行都没选。
unsafe fn selected(dlg: HWND) -> Option<usize> {
    let list = GetDlgItem(Some(dlg), ID_LIST).ok()?;
    let sel = SendMessageW(list, LB_GETCURSEL, None, None).0;
    if sel < 0 {
        return None;
    }
    let data = SendMessageW(list, LB_GETITEMDATA, Some(WPARAM(sel as usize)), None).0;
    let index = data as usize;
    (index < state_mut(dlg).entries.len()).then_some(index)
}

unsafe fn on_ok(dlg: HWND) {
    let Some(index) = selected(dlg) else {
        end(dlg);
        return;
    };
    let st = state_mut(dlg);
    st.result = Some(Outcome::Pick(st.entries[index].key.clone()));
    end(dlg);
}

unsafe fn on_config(dlg: HWND) {
    state_mut(dlg).result = Some(Outcome::Config);
    end(dlg);
}

unsafe fn end(dlg: HWND) {
    let _ = DestroyWindow(dlg);
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            crate::win32::lang::localize_dialog(hwnd, 255);
            init(hwnd)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let notif = (wparam.0 >> 16) as u16;
            match id {
                IDOK => {
                    on_ok(hwnd);
                    1
                }
                IDCANCEL => {
                    end(hwnd);
                    1
                }
                ID_CONFIG => {
                    on_config(hwnd);
                    1
                }
                ID_LIST if notif == LBN_DBLCLK => {
                    on_ok(hwnd);
                    1
                }
                // 过滤框每次变化都重建列表。
                ID_EDIT if notif == EN_CHANGE as u16 => {
                    refill(hwnd);
                    1
                }
                _ => 0,
            }
        }
        WM_ERASEBKGND => {
            let st = state_mut(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.field_face, st.field)
        }
        WM_CLOSE => {
            // 标题栏的 X 是取消：结果保持 None。
            end(hwnd);
            1
        }
        WM_DESTROY => {
            state_mut(hwnd).destroyed = true;
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam).0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模板必须是资源 255 的样子：五个控件、按钮列在右边、列表占满剩下的高度，
    /// 头是 213×296 的 `DS_MODALFRAME|DS_SETFONT`。
    #[test]
    fn template_matches_the_resource() {
        assert_eq!(ITEMS.len(), 5);
        assert_eq!(ITEMS[0].id, ID_EDIT);
        assert_eq!(ITEMS[0].class, Class::Atom(ATOM_EDIT));
        assert_eq!(ITEMS[0].style, 0x5081_0080);
        assert_eq!((ITEMS[0].x, ITEMS[0].y, ITEMS[0].w, ITEMS[0].h), (7, 7, 145, 14));
        assert_eq!(ITEMS[1].id, IDOK);
        assert_eq!(ITEMS[1].style, 0x5001_0001, "BS_DEFPUSHBUTTON");
        assert_eq!((ITEMS[1].x, ITEMS[1].y), (157, 7));
        assert_eq!(ITEMS[2].id, IDCANCEL);
        assert_eq!((ITEMS[2].x, ITEMS[2].y), (157, 23));
        assert_eq!(ITEMS[3].id, ID_LIST);
        assert_eq!(ITEMS[3].class, Class::Atom(ATOM_LISTBOX));
        assert_eq!(ITEMS[3].style, 0x50A1_0103, "LBS_SORT|LBS_NOTIFY|LBS_NOINTEGRALHEIGHT");
        assert_eq!((ITEMS[3].x, ITEMS[3].y, ITEMS[3].w, ITEMS[3].h), (7, 24, 145, 265));
        assert_eq!(ITEMS[4].id, ID_CONFIG);
        assert_eq!((ITEMS[4].x, ITEMS[4].y), (157, 39));
        assert_eq!(ITEMS[4].label, CONFIG_TEXT);

        let b = build_template();
        // dlgVer + signature, then the header.
        assert_eq!(&b[0..4], &[1, 0, 0xFF, 0xFF]);
        assert_eq!(&b[12..16], &DLG_STYLE.to_le_bytes());
        assert_eq!(u16::from_le_bytes([b[16], b[17]]), 5, "cdit");
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), DLG_W);
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), DLG_H);
        // The DS_SETFONT block stays the template's 8 pt / charset 1: the
        // manager turns that font into dialog units, so raising it here would
        // rescale the 213×296 layout instead of enlarging the text.
        // 位置由标题长度算出来，别写死：30 字节的头（menu/class 之后是标题） + 标题的 UTF-16 + NUL，才是
        // point size。
        let font = 30 + (DLG_TITLE.encode_utf16().count() + 1) * 2;
        assert_eq!(u16::from_le_bytes([b[font], b[font + 1]]), 8, "point size");
        assert_eq!(
            u16::from_le_bytes([b[font + 2], b[font + 3]]),
            0,
            "weight 0 = the face's own"
        );
        assert_eq!(b[font + 4], 0, "no italic");
        assert_eq!(b[font + 5], 1, "DEFAULT_CHARSET");
    }

    #[test]
    fn the_filter_is_an_ordered_subsequence() {
        assert!(matches("AutoIt", ""));
        assert!(matches("AutoIt", "ai"), "A…I 按顺序出现");
        assert!(matches("AutoIt", "AI"), "大小写不敏感");
        assert!(matches("CSharp", "cs"));
        assert!(matches("CSS Less", "csles"));
        assert!(!matches("AutoIt", "ia"), "顺序反了不算");
        assert!(!matches("Go", "goo"), "字符不够不算");
        assert!(matches("默认", "默"));
        assert!(matches("C++", "+"), "符号也能匹配");
    }

    /// 重建列表后选谁：当前语法优先，被滤掉就回落到第一条可见行，
    /// 一条都不剩就不选（`LB_SETCURSEL` 不发）。
    #[test]
    fn the_refill_reselects_the_documents_syntax() {
        let rows = [
            Entry { label: DEFAULT_LABEL.into(), key: None },
            Entry { label: "C++".into(), key: Some("cpp".into()) },
            Entry { label: "CSS".into(), key: Some("css".into()) },
            Entry { label: "Markdown".into(), key: Some("markdown".into()) },
        ];
        // 无过滤：选中的是文档当前语法那一行，不是第 0 行。
        assert_eq!(pick(&rows, "", Some("css")), Some(2));
        // 「默认」是 key 为 None 的那行，同样能被选中。
        assert_eq!(pick(&rows, "", None), Some(0));
        // 过滤把当前语法滤掉了 → 回落到第一条可见行。
        assert_eq!(pick(&rows, "mark", Some("css")), Some(3));
        // `c` 同时命中 C++ 和 CSS：当前语法是哪个就选哪个，而不是第一条。
        assert_eq!(pick(&rows, "c", Some("cpp")), Some(1));
        assert_eq!(pick(&rows, "c", Some("css")), Some(2));
        // 一条都不剩 → 不选。
        assert_eq!(pick(&rows, "zzz", Some("css")), None);
    }
}
