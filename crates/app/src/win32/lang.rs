//! UI language tables.  Every visible word lives in `<exe>\lang\<name>.lng`
//! (`en-US.lng`, `zh-CN.lng`, …), an INI with three kinds of section: `[MENUxxx]` keyed
//! by command id (`P<id>` for a popup's own caption), `[DLGxxx]` keyed by control id (0
//! is the dialog caption, 1/2 the OK/Cancel pair), and `[STRING]` keyed by a caption
//! id. The bar, the context menus, the dialogs and the status strip all ask that table
//! rather than embedding text, which is what makes a language switch a file swap
//! instead of a rebuild.  This module is that table. The two languages this
//! implementation ships with (English and Simplified Chinese) are compiled in as a
//! fallback, so a fresh build runs without any files next to it; a
//! `<exe>\lang\<name>.lng` file wins when present, and dropping another one there is
//! all a new language needs. A table named by its locale id (a Windows LCID such as
//! `1041`) is accepted the same way. `setting.ini`'s `[Setting] Language` picks the
//! file (an empty value means "follow the system UI language"), and a change takes
//! effect on the next start — the same 需重启 contract the options page states.  The
//! Chinese table is also loaded as the canonical one: every label the code carries is
//! Chinese, so a popup's `P` id and a hand-written message box text are resolved
//! through the canonical value before the selected table's value is returned. That is
//! what lets a menu built from `ROWS` be localized without giving every row a second
//! id.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use dg_ui::menu::MenuItem;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Globalization::{
    GetLocaleInfoW, GetUserDefaultUILanguage, LOCALE_SLOCALIZEDLANGUAGENAME,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetDlgCtrlID, SetWindowTextW,
};
use windows::core::{BOOL, PCWSTR};

const EN_US: u32 = 1033;
const ZH_CN: u32 = 2052;

const EN_TABLE: &[u8] = include_bytes!("../../../../lang/en-US.lng");
const ZH_TABLE: &[u8] = include_bytes!("../../../../lang/zh-CN.lng");

#[derive(Default, Clone)]
struct Table {
    menu: HashMap<(u32, u32), String>,
    popup: HashMap<(u32, u32), String>,
    dlg: HashMap<(u32, u32), String>,
    string: HashMap<u32, String>,
    author: String,
    home: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct LangInfo {
    pub(super) code: u32,
    pub(super) name: String,
    pub(super) author: String,
    pub(super) home: String,
}

struct Locale {
    code: u32,
    current: Table,
    canonical: Table,
    /// Canonical (Chinese) value → the selected table's value, for text the
    /// code carries as a literal. Chinese is indexed too, so a label already in
    /// the selected language maps to itself.
    reverse: HashMap<String, String>,
    /// Popup caption → its `P<id>` key, in the canonical or selected table.
    popup_index: HashMap<String, (u32, u32)>,
}

static LOCALE: OnceLock<Locale> = OnceLock::new();

/// Reads the language table once, before the frame creates any window. Later
/// calls are no-ops, which is what the restart-to-apply contract relies on.
pub(super) fn init() {
    let _ = LOCALE.set(Locale::load());
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Locale {
    fn load() -> Self {
        let dir = exe_dir().join("lang");
        let wanted = configured_language().unwrap_or_else(system_language);
        let (code, current) = load_language(&dir, wanted);
        // The canonical table is always Chinese: it is the language every
        // caption in this tree was written in, so it is the key space the
        // reverse lookups use regardless of the selected language.
        let (_, canonical) = if code == ZH_CN {
            (code, current.clone())
        } else {
            load_language(&dir, ZH_CN)
        };
        let mut locale = Self {
            code,
            current,
            canonical,
            reverse: HashMap::new(),
            popup_index: HashMap::new(),
        };
        locale.build_indexes();
        locale
    }

    fn build_indexes(&mut self) {
        // Deterministic order: the global caption section first, then
        // the rest by section/id, so a label that lives in two sections always
        // resolves the same way.
        let mut menu_keys: Vec<&(u32, u32)> = self.canonical.menu.keys().collect();
        menu_keys.sort_unstable();
        for key in menu_keys {
            if let Some(cur) = self.current.menu.get(key) {
                self.reverse
                    .entry(self.canonical.menu[key].clone())
                    .or_insert_with(|| cur.clone());
            }
        }
        let mut pop_keys: Vec<&(u32, u32)> = self.canonical.popup.keys().collect();
        pop_keys.sort_unstable();
        for key in pop_keys {
            if let Some(cur) = self.current.popup.get(key) {
                self.reverse
                    .entry(self.canonical.popup[key].clone())
                    .or_insert_with(|| cur.clone());
            }
        }
        let mut dlg_keys: Vec<&(u32, u32)> = self.canonical.dlg.keys().collect();
        dlg_keys.sort_unstable();
        for key in dlg_keys {
            if let Some(cur) = self.current.dlg.get(key) {
                self.reverse
                    .entry(self.canonical.dlg[key].clone())
                    .or_insert_with(|| cur.clone());
            }
        }
        let mut str_keys: Vec<&u32> = self.canonical.string.keys().collect();
        str_keys.sort_unstable();
        for key in str_keys {
            if let Some(cur) = self.current.string.get(key) {
                self.reverse
                    .entry(self.canonical.string[key].clone())
                    .or_insert_with(|| cur.clone());
            }
        }

        // Popup captions: the bar puts the caption in the `P` namespace, and
        // its row id is 0 because a Win32 popup has no command id of its own.
        // The caption is therefore the only handle a `MenuItem` has on the
        // key, which is why it is indexed here.
        let mut order: Vec<(bool, u32, u32, String)> = Vec::new();
        for (&(sec, id), text) in &self.canonical.popup {
            order.push((sec != 128, sec, id, text.clone()));
        }
        for (&(sec, id), text) in &self.current.popup {
            order.push((sec != 128, sec, id, text.clone()));
        }
        order.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        for (_, sec, id, text) in order {
            self.popup_index.entry(text).or_insert((sec, id));
        }

        // Everything in the selected table maps to itself, so a label that is
        // already in the target language is not translated through a
        // same-looking canonical entry from another section.
        for text in self
            .current
            .menu
            .values()
            .chain(self.current.popup.values())
            .chain(self.current.dlg.values())
            .chain(self.current.string.values())
        {
            self.reverse.entry(text.clone()).or_insert_with(|| text.clone());
        }
    }

    fn menu_item_text(
        &self,
        section: u32,
        id: u32,
        popup: bool,
        fallback: &str,
    ) -> Option<String> {
        let (base, accel) = match fallback.split_once('\t') {
            Some((base, accel)) => (base, Some(accel)),
            None => (fallback, None),
        };
        let text = if popup {
            self.popup_index
                .get(base)
                .and_then(|&(sec, pid)| {
                    self.popup_value(sec, pid)
                })
                .or_else(|| {
                    (id != 0)
                        .then(|| self.popup_value(section, id))
                        .flatten()
                })
        } else {
            self.current
                .menu
                .get(&(section, id))
                .or_else(|| self.current.menu.get(&(128, id)))
        };
        text.map(|text| match accel {
            Some(accel) => format!("{text}\t{accel}"),
            None => text.clone(),
        })
    }

    fn popup_value(&self, section: u32, id: u32) -> Option<&String> {
        if let Some(text) = self.current.popup.get(&(section, id)) {
            return Some(text);
        }
        // Two popups are keyed under different ids in the two tables
        // (72 vs 81, 1336 vs 336), so they are aliased here.
        let alias = match (section, id) {
            (128, 72) => Some((128, 81)),
            (128, 1336) => Some((128, 336)),
            _ => None,
        };
        if let Some((sec, pid)) = alias {
            if let Some(text) = self.current.popup.get(&(sec, pid)) {
                return Some(text);
            }
        }
        self.canonical.popup.get(&(section, id))
    }
}

/// `setting.ini` 的 `[Setting] Language`：写的值是语言表文件名（`zh-CN` 或
/// (locale id)`2052`），空着就是「跟系统」。读的就是 [`crate::settings`] 那条路，
/// 别再有一份自己的读取器。
fn configured_language() -> Option<u32> {
    let value = crate::settings::section("Setting", "Language")?;
    let stem = value
        .strip_suffix(".lng")
        .or_else(|| value.strip_suffix(".LNG"))
        .unwrap_or(&value);
    code_from_stem(stem)
}

fn system_language() -> u32 {
    let lcid = unsafe { GetUserDefaultUILanguage() } as u32;
    // zh-TW (1028) and zh-HK (3076) have no table in this build; they read
    // Simplified Chinese rather than falling all the way back to English.
    if lcid & 0x3FF == 0x04 { ZH_CN } else { EN_US }
}

fn language_file(dir: &Path, code: u32) -> Option<PathBuf> {
    let mut names: Vec<String> = Vec::new();
    match code {
        EN_US => names.push("en-US".to_string()),
        ZH_CN => names.push("zh-CN".to_string()),
        _ => {}
    }
    names.push(code.to_string());
    names
        .into_iter()
        .map(|stem| dir.join(format!("{stem}.lng")))
        .find(|path| path.is_file())
}

fn builtin(code: u32) -> Option<&'static [u8]> {
    match code {
        EN_US => Some(EN_TABLE),
        ZH_CN => Some(ZH_TABLE),
        _ => None,
    }
}

fn load_language(dir: &Path, wanted: u32) -> (u32, Table) {
    if let Some(table) = language_file(dir, wanted).and_then(|p| std::fs::read(p).ok()) {
        let parsed = table_from(&table);
        if !parsed.menu.is_empty() || !parsed.dlg.is_empty() || !parsed.string.is_empty() {
            return (wanted, parsed);
        }
    }
    if let Some(bytes) = builtin(wanted) {
        return (wanted, table_from(bytes));
    }
    let fallback = if wanted & 0x3FF == 0x04 { ZH_CN } else { EN_US };
    if let Some(table) = language_file(dir, fallback).and_then(|p| std::fs::read(p).ok()) {
        let parsed = table_from(&table);
        if !parsed.menu.is_empty() {
            return (fallback, parsed);
        }
    }
    if let Some(bytes) = builtin(fallback) {
        return (fallback, table_from(bytes));
    }
    (fallback, Table::default())
}

/// 解一份语言表字节再进 [`parse`]。语言表多认一种没有 BOM 的 UTF-16LE
/// （[`dg_core::ini::decode_with`] 的猜测）；解不出来就当一张空表，
/// 让加载链落到下一份。
fn table_from(bytes: &[u8]) -> Table {
    match dg_core::ini::decode_with(bytes, true) {
        Some(text) => parse(&text),
        None => Table::default(),
    }
}

/// 行的切分、段名大小写、注释（`;` 与 `//`）都在 [`dg_core::ini`]；
/// 这里只认四种段、把值收进各自的表。重复键后者胜 —— HashMap 的写法正好如此。
fn parse(text: &str) -> Table {
    let mut table = Table::default();
    for (name, kv) in dg_core::ini::Ini::parse(text).sections() {
        let upper = name.to_ascii_uppercase();
        if upper == "INFO" {
            for (key, value) in kv {
                let value = value.trim().to_string();
                if key.trim().eq_ignore_ascii_case("Author") {
                    table.author = value;
                } else if key.trim().eq_ignore_ascii_case("Home") {
                    table.home = value;
                }
            }
            continue;
        }
        if upper == "STRING" {
            for (key, value) in kv {
                if let Ok(id) = key.trim().parse::<u32>() {
                    table.string.insert(id, value.trim().to_string());
                }
            }
            continue;
        }
        if let Some(rest) = upper.strip_prefix("MENU") {
            let Ok(number) = rest.parse::<u32>() else {
                continue;
            };
            for (key, value) in kv {
                let value = value.trim().to_string();
                let key = key.trim();
                if let Some(rest) = key.strip_prefix('P') {
                    if let Ok(id) = rest.parse::<u32>() {
                        table.popup.insert((number, id), value);
                    }
                } else if let Ok(id) = key.parse::<u32>() {
                    table.menu.insert((number, id), value);
                }
            }
        } else if let Some(rest) = upper.strip_prefix("DLG") {
            let Ok(number) = rest.parse::<u32>() else {
                continue;
            };
            for (key, value) in kv {
                if let Ok(id) = key.trim().parse::<u32>() {
                    table.dlg.insert((number, id), value.trim().to_string());
                }
            }
        }
    }
    table
}

/// The id of the table in force, for the options page.
pub(super) fn current_code() -> Option<u32> {
    LOCALE.get().map(|l| l.code)
}

/// The `MENU` caption for a command id, with `[MENU128]` as the shared
/// fallback every other menu draws from.
pub(super) fn menu(section: u32, id: u32) -> Option<&'static str> {
    let loc = LOCALE.get()?;
    loc.current
        .menu
        .get(&(section, id))
        .or_else(|| loc.current.menu.get(&(128, id)))
        .map(|s| s.as_str())
}

/// A `DLG` caption by control id.
pub(super) fn dlg(dialog: u32, id: u32) -> Option<&'static str> {
    LOCALE
        .get()?
        .current
        .dlg
        .get(&(dialog, id))
        .map(|s| s.as_str())
}

/// A `STRING` entry by caption id.
pub(super) fn string(id: u32) -> Option<&'static str> {
    let loc = LOCALE.get()?;
    loc.current
        .string
        .get(&id)
        .or_else(|| loc.canonical.string.get(&id))
        .map(|s| s.as_str())
}

/// Translates a caption the code carries as a Chinese literal. Returns `None`
/// when the table has no entry, so the caller keeps its own text.
pub(super) fn tr(text: &str) -> Option<&'static str> {
    LOCALE.get()?.reverse.get(text).map(|s| s.as_str())
}

/// [`tr`] with the input as the fallback.
pub(super) fn trs(text: &str) -> String {
    tr(text).unwrap_or(text).to_string()
}

/// A `STRING` entry with its `%d`/`%s` placeholders filled in order, the way the
/// `wsprintf` reads them.
pub(super) fn format(id: u32, args: &[&str]) -> Option<String> {
    let fmt = string(id)?;
    let mut out = String::new();
    let mut rest = fmt;
    let mut args = args.iter();
    while let Some(pos) = rest.find('%') {
        out.push_str(&rest[..pos]);
        let mut chars = rest[pos + 1..].chars();
        match chars.next() {
            Some(kind @ ('d' | 's')) => {
                if let Some(value) = args.next() {
                    out.push_str(value);
                } else {
                    out.push('%');
                    out.push(kind);
                }
                rest = &rest[pos + 1 + kind.len_utf8()..];
            }
            _ => {
                out.push('%');
                rest = &rest[pos + 1..];
            }
        }
    }
    out.push_str(rest);
    Some(out)
}

/// Localizes a menu tree in place. `section` is the caption-table id the frame keys
/// the menu on (`MENU128` for the bar, `MENU304` for the editor menu, …); rows without
/// an entry keep their captions, which is how this implementation's unported
/// placeholders stay readable.
pub(super) fn localize_menu(section: u32, items: &mut [MenuItem]) {
    let Some(loc) = LOCALE.get() else {
        return;
    };
    for item in items.iter_mut() {
        if item.separator {
            continue;
        }
        if let Some(text) =
            loc.menu_item_text(section, item.id, !item.children.is_empty(), &item.label)
        {
            item.label = text;
        }
        localize_menu(section, &mut item.children);
    }
}

/// Localizes one row by caption-id, for menus the frame does not own as a
/// `MenuItem` tree (the toolbar's row model, the print-token popups).
pub(super) fn menu_text(section: u32, id: u32, popup: bool, fallback: &str) -> String {
    LOCALE
        .get()
        .and_then(|loc| loc.menu_item_text(section, id, popup, fallback))
        .unwrap_or_else(|| fallback.to_string())
}

/// Localizes a dialog's title and every Button/Static child that has a `DLG`
/// entry, by control id.
///
/// Call this before the dialog's own init fills values: the table only names
/// captions, but an edit or list control that shares an id with one would
/// otherwise be blanked.
pub(super) unsafe fn localize_dialog(hwnd: HWND, dialog: u32) {
    unsafe {
        let Some(loc) = LOCALE.get() else {
            return;
        };
        if let Some(text) = loc.current.dlg.get(&(dialog, 0)) {
            set_text(hwnd, text);
        }
        let mut children: Vec<HWND> = Vec::new();
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(collect_child),
            LPARAM(&mut children as *mut Vec<HWND> as isize),
        );
        for child in children {
            let id = GetDlgCtrlID(child);
            if id <= 0 {
                continue;
            }
            let Some(text) = loc.current.dlg.get(&(dialog, id as u32)) else {
                continue;
            };
            let mut class = [0u16; 32];
            let len = GetClassNameW(child, &mut class);
            let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
            if class.eq_ignore_ascii_case("button") || class.eq_ignore_ascii_case("static") {
                set_text(child, text);
            }
        }
    }
}

unsafe extern "system" fn collect_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        (*(lparam.0 as *mut Vec<HWND>)).push(hwnd);
    }
    BOOL(1)
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(wide.as_ptr())) };
}

/// The `STRING` command pair behind the shortcut manager: the table stores
/// `说明\n命令` (a literal backslash-n), the list wants the two halves.
pub(super) fn command_text(id: u32) -> Option<(String, String)> {
    let raw = string(id)?;
    Some(match raw.split_once("\\n") {
        Some((desc, name)) => (name.trim().to_string(), desc.trim().to_string()),
        None => (raw.to_string(), raw.to_string()),
    })
}

/// The caption of a docked pane: the command's own string with the accelerator
/// taken off. A strip cell is a name, not a menu row — `文件列表(&O)` would put
/// the `&O` in the cell.
pub(super) fn pane_title(id: u32, fallback: &str) -> String {
    // The command's string is the description and the name on two lines; the
    // cell wants the name.
    let text = match command_text(id) {
        Some((name, _)) => name,
        None => fallback.to_string(),
    };
    match text.find("(&") {
        Some(at) => text[..at].trim_end().to_string(),
        None => text,
    }
}

/// The caption an untouched document goes by before its number.
pub(super) fn untitled_caption() -> String {
    string(15021).unwrap_or("无标题").to_string()
}

/// The number in an untouched document's generated name. `None` for the bare
/// caption and for any other name.
pub(super) fn untitled_number(name: &str) -> Option<u32> {
    let caption = untitled_caption();
    let digits = name.strip_prefix(caption.as_str())?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

/// The tab/window name for a document: the 无标题 caption plus its number when
/// it has neither a path nor a user-supplied title.
pub(super) fn editor_name(editor: &crate::app::Editor) -> String {
    if editor.path.is_none() && editor.tab_title.is_none() {
        let caption = untitled_caption();
        return match editor.untitled_no {
            Some(n) => format!("{caption}{n}"),
            None => caption,
        };
    }
    editor.display_name()
}

/// The words the status strip composes its panes from (`STRING` ids the
/// format path expands).
pub(super) fn status_labels() -> dg_ui::statusbar::StatusLabels {
    let word = |id, fallback: &str| string(id).unwrap_or(fallback).to_string();
    dg_ui::statusbar::StatusLabels {
        ready: word(57345, "就绪"),
        sync: word(15233, "同步滚动"),
        default_syntax: word(15030, "默认"),
        soft_tab: word(15036, "软空格"),
        tab: word(15037, "制表符"),
        ln: word(15229, "行"),
        col: word(15230, "列"),
        insert: word(15038, "插入"),
        overwrite: word(15039, "覆写"),
        column: word(15040, "列模式"),
        multi: word(15247, "多选"),
        selection: word(15034, "%d字符,%d行"),
        hex_selection: word(15095, "选区: %d"),
        hex_offset: word(15096, "偏移: %08X"),
        hex_size: word(15097, "大小: %d"),
    }
}

/// Every language the options page can offer: the two compiled-in tables plus
/// anything in `<exe>\lang`.
pub(super) fn available() -> Vec<LangInfo> {
    let mut list: Vec<LangInfo> = Vec::new();
    let mut add = |code: u32, author: String, home: String| {
        if list.iter().any(|item| item.code == code) {
            return;
        }
        let mut name = locale_name(code);
        if name.is_empty() {
            name = format!("Language {code}");
        }
        list.push(LangInfo {
            code,
            name,
            author,
            home,
        });
    };
    for (code, bytes) in [(EN_US, EN_TABLE), (ZH_CN, ZH_TABLE)] {
        let table = table_from(bytes);
        add(code, table.author, table.home);
    }
    if let Ok(dir) = std::fs::read_dir(exe_dir().join("lang")) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|s| s.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("lng"))
            {
                continue;
            }
            let Some(code) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(code_from_stem)
            else {
                continue;
            };
            let table = std::fs::read(&path)
                .map(|bytes| table_from(&bytes))
                .unwrap_or_default();
            add(code, table.author, table.home);
        }
    }
    list.sort_by_key(|item| item.code);
    list
}

/// The language code a table's stem names: a locale id (`2052`) or a language
/// name (`zh-CN`, `ja`, `pt-BR`, …).
fn code_from_stem(stem: &str) -> Option<u32> {
    let stem = stem.trim();
    if stem.is_empty() {
        return None;
    }
    if let Ok(code) = stem.parse::<u32>() {
        return Some(code);
    }
    let name = stem.to_ascii_lowercase().replace('_', "-");
    Some(match name.as_str() {
        "en" | "en-us" | "english" => EN_US,
        "zh" | "zh-cn" | "zh-hans" | "chs" | "chinese" => ZH_CN,
        "zh-tw" | "zh-hk" | "zh-mo" | "cht" => 1028,
        "ja" | "ja-jp" => 1041,
        "ko" | "ko-kr" => 1042,
        "de" | "de-de" => 1031,
        "fr" | "fr-fr" => 1036,
        "es" | "es-es" => 3082,
        "ru" | "ru-ru" => 1049,
        "pt" | "pt-br" => 1046,
        "it" | "it-it" => 1040,
        _ => return None,
    })
}

/// The Windows display name of an LCID, in the current UI language.
pub(super) fn locale_name(lcid: u32) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetLocaleInfoW(lcid, LOCALE_SLOCALIZEDLANGUAGENAME, Some(&mut buf)) };
    if n > 1 {
        return String::from_utf16_lossy(&buf[..(n - 1) as usize]);
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(code: u32) -> Table {
        table_from(builtin(code).unwrap())
    }

    #[test]
    fn both_builtin_tables_parse_their_sections() {
        for code in [EN_US, ZH_CN] {
            let t = table(code);
            assert_eq!(t.menu.get(&(128, 57601)).map(String::as_str), Some(if code == EN_US {
                "&Open…"
            } else {
                "打开(&O)..."
            }));
            assert_eq!(t.dlg.get(&(202, 0)).map(String::as_str), Some(if code == EN_US {
                "Encoding"
            } else {
                "编码"
            }));
            assert!(t.popup.contains_key(&(128, 57601)));
            assert!(t.string.contains_key(&15021));
        }
    }

    #[test]
    fn canonical_values_map_to_the_selected_language() {
        let mut loc = Locale {
            code: EN_US,
            current: table(EN_US),
            canonical: table(ZH_CN),
            reverse: HashMap::new(),
            popup_index: HashMap::new(),
        };
        loc.build_indexes();
        assert_eq!(loc.reverse.get("文件(&F)").map(String::as_str), Some("&File"));
        assert_eq!(
            loc.reverse.get("无标题").map(String::as_str),
            Some("Unnamed")
        );
        // The edit menu's own section overrides the shared `MENU128` row.
        assert_eq!(
            loc.popup_index.get("更多粘贴(&M)..."),
            Some(&(304, 91))
        );
    }

    #[test]
    fn untitled_names_carry_their_number() {
        let caption = untitled_caption();
        let mut ed = crate::app::Editor::new();
        ed.untitled_no = Some(3);
        assert_eq!(editor_name(&ed), format!("{caption}3"));
        assert_eq!(untitled_number(&format!("{caption}3")), Some(3));
        assert_eq!(untitled_number(&caption), None);
        ed.untitled_no = None;
        assert_eq!(editor_name(&ed), caption);
        ed.path = Some("C:\\x\\a.txt".to_string());
        assert_eq!(editor_name(&ed), "a.txt");
        ed.path = None;
        ed.tab_title = Some("查找结果".to_string());
        assert_eq!(editor_name(&ed), "查找结果");
    }

    #[test]
    fn menu_text_keeps_the_shortcut_column() {
        let mut loc = Locale {
            code: EN_US,
            current: table(EN_US),
            canonical: table(ZH_CN),
            reverse: HashMap::new(),
            popup_index: HashMap::new(),
        };
        loc.build_indexes();
        let text = loc
            .menu_item_text(304, 57635, false, "剪切(&T)\tCtrl+X")
            .unwrap();
        assert_eq!(text, "Cu&t\tCtrl+X");
        let popup = loc
            .menu_item_text(304, 0, true, "更多编辑(&O)...")
            .unwrap();
        // The English table keys this popup as `P81=&More…`.
        assert_eq!(popup, "&More…");
    }

    /// `[MENU349]` 的值自带制表符和快捷键，所以调它的时候兜底文案不能再带一个：
    /// `menu_item_text` 会把兜底的那一列再拼一遍，菜单上就会出现两个 `Ctrl+D`。
    #[test]
    fn quick_dir_menu_carries_one_shortcut_column() {
        let mut loc = Locale {
            code: EN_US,
            current: table(EN_US),
            canonical: table(ZH_CN),
            reverse: HashMap::new(),
            popup_index: HashMap::new(),
        };
        loc.build_indexes();
        assert_eq!(
            loc.menu_item_text(349, 1206, false, "Add Folder").unwrap(),
            "Add folder\tCtrl+D"
        );
        assert_eq!(
            loc.menu_item_text(349, 1208, false, "Remove from List").unwrap(),
            "Remove from list\tDel"
        );
    }

    #[test]
    fn every_bar_popup_caption_resolves() {
        let mut loc = Locale {
            code: EN_US,
            current: table(EN_US),
            canonical: table(ZH_CN),
            reverse: HashMap::new(),
            popup_index: HashMap::new(),
        };
        loc.build_indexes();
        for label in [
            "文件(&F)",
            "编辑(&E)",
            "查看(&V)",
            "查找(&S)",
            "工具(&T)",
            "扩展(&A)",
            "窗口(&I)",
            "帮助(&H)",
            "会话",
            "最近的文件(&R)",
        ] {
            assert!(
                loc.popup_index.contains_key(label),
                "popup caption {label} has no P key"
            );
        }
    }
}
