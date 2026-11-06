//! External tools — the `tool/*.tool` files and the runner's input side.
//!
//! Tools are grouped; each group is one INI file in `<exe_dir>	ool`, and the
//! group list lives in the main settings' `ToolGroupEx` key. A group file
//! holds its tools one section each: `[Tool]` for the first, `[Tool1]`, and
//! so on, with a name, command, arguments and working directory per tool.

use crate::ini::Ini;
use std::path::{Path, PathBuf};

/// What happens to the tool's output — the nine 输出模式 rows, keyed by
/// the id the tool file stores. This is the `ActionPattern` key;
/// `Capture` holds the regex capture groups instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// 1 无 — run, capture nothing.
    None,
    /// 2 捕获输出(隐藏窗口).
    CaptureHide,
    /// 9 捕获输出(显示窗口).
    CaptureShow,
    /// 8 捕获到Web预览.
    WebPreview,
    /// 3 插入.
    Insert,
    /// 4 从新行插入.
    InsertNewLine,
    /// 5 替换当前行.
    ReplaceLine,
    /// 6 替换文档.
    ReplaceDocument,
    /// 7 隐藏方式运行.
    RunHidden,
}

impl CaptureMode {
    /// The INI value for this mode.
    pub fn id(self) -> i32 {
        match self {
            CaptureMode::None => 1,
            CaptureMode::CaptureHide => 2,
            CaptureMode::CaptureShow => 9,
            CaptureMode::WebPreview => 8,
            CaptureMode::Insert => 3,
            CaptureMode::InsertNewLine => 4,
            CaptureMode::ReplaceLine => 5,
            CaptureMode::ReplaceDocument => 6,
            CaptureMode::RunHidden => 7,
        }
    }

    /// The mode an INI value names; an unknown value reads as `None`,
    /// the first of the 输出模式 rows (无).
    pub fn from_id(id: i32) -> CaptureMode {
        match id {
            2 => CaptureMode::CaptureHide,
            9 => CaptureMode::CaptureShow,
            8 => CaptureMode::WebPreview,
            3 => CaptureMode::Insert,
            4 => CaptureMode::InsertNewLine,
            5 => CaptureMode::ReplaceLine,
            6 => CaptureMode::ReplaceDocument,
            7 => CaptureMode::RunHidden,
            _ => CaptureMode::None,
        }
    }

    /// Whether the mode wants the child's
    /// output piped back at all. `None` and
    /// `RunHidden` fire and forget; the
    /// capture modes read the pipes; the
    /// document modes both pipe and edit.
    pub fn captures(self) -> bool {
        !matches!(self, CaptureMode::None | CaptureMode::RunHidden)
    }

    /// Whether the mode writes into the
    /// document rather than the output
    /// window.
    pub fn edits_document(self) -> bool {
        matches!(
            self,
            CaptureMode::Insert
                | CaptureMode::InsertNewLine
                | CaptureMode::ReplaceLine
                | CaptureMode::ReplaceDocument
        )
    }
}

/// What is saved before the tool runs — the three 保存模式 rows, keyed by
/// id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveMode {
    /// 1 无.
    None,
    /// 2 保存当前文档.
    Current,
    /// 3 保存全部文档.
    All,
}

impl SaveMode {
    pub fn id(self) -> i32 {
        match self {
            SaveMode::None => 1,
            SaveMode::Current => 2,
            SaveMode::All => 3,
        }
    }

    pub fn from_id(id: i32) -> SaveMode {
        match id {
            2 => SaveMode::Current,
            3 => SaveMode::All,
            _ => SaveMode::None,
        }
    }
}

/// The three capture-group combos of the
/// 高级 dialog, in the order the
/// dialog's three capture combos are laid out.
pub const CAPTURE_GROUPS: [&str; 3] = ["文件路径", "行号", "列号"];

/// [`Tool::capture`] slots, named for the
/// three combos that fill them.
pub const CAPTURE_FILE_PATH: usize = 0;
pub const CAPTURE_LINE: usize = 1;
pub const CAPTURE_COLUMN: usize = 2;

/// One configured tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub uuid: i32,
    pub title: String,
    pub icon: String,
    pub exe_path: String,
    pub args: String,
    pub init_dir: String,
    pub use_pattern: bool,
    pub input: bool,
    pub pattern: String,
    /// The capture group each of 文件路径 /
    /// 行号 / 列号 reads from the regex's
    /// match, 0 for 无. `Capture`.
    pub capture: [i32; 3],
    /// 动作模式 — `ActionPattern`.
    pub action_pattern: CaptureMode,
    /// 保存模式 — `SavePattern`.
    pub save_pattern: SaveMode,
    /// 输出编码 — `Encoding`, a code page;
    /// 0 is `Default (ANSI)`.
    pub encoding: i32,
}

impl Tool {
    /// A blank tool, the state the manager's 新工具 button starts from.
    ///
    /// The title carries no number: the 工具 menu numbers the rows itself
    /// (`%.2d. %s`), so a numbered title would come out as `01. 01. …`.
    pub fn blank() -> Tool {
        Tool {
            uuid: 0,
            title: String::from("新工具"),
            icon: String::new(),
            exe_path: String::new(),
            args: String::new(),
            init_dir: String::new(),
            use_pattern: false,
            input: false,
            pattern: String::new(),
            capture: [0; 3],
            action_pattern: CaptureMode::CaptureHide,
            save_pattern: SaveMode::None,
            encoding: 0,
        }
    }

    /// The first tool of a group is `[Tool]`, the rest `[Tool1]`,
    /// `[Tool2]`, … — the index the loader appends to the base name.
    pub fn to_ini_section(&self, index: usize) -> String {
        let mut out = if index == 0 {
            String::from("[Tool]\n")
        } else {
            format!("[Tool{index}]\n")
        };
        for (key, value) in self.ini_rows() {
            out.push_str(&key);
            out.push('=');
            out.push_str(&value);
            out.push('\n');
        }
        out
    }

    /// The section's rows in the loader's key order — the text spelling
    /// ([`Tool::to_ini_section`]) and the group-file writer share this, so
    /// both spell every key the same way.
    fn ini_rows(&self) -> Vec<(String, String)> {
        vec![
            ("UUID".to_string(), self.uuid.to_string()),
            ("Title".to_string(), self.title.clone()),
            ("Icon".to_string(), self.icon.clone()),
            ("ExePath".to_string(), prefixed(&self.exe_path)),
            ("Args".to_string(), prefixed(&self.args)),
            ("InitDir".to_string(), prefixed(&self.init_dir)),
            ("UsePattern".to_string(), yn(self.use_pattern).to_string()),
            ("Input".to_string(), yn(self.input).to_string()),
            ("Pattern".to_string(), prefixed(&self.pattern)),
            ("Capture".to_string(), capture_hex(self.capture)),
            (
                "ActionPattern".to_string(),
                self.action_pattern.id().to_string(),
            ),
            (
                "SavePattern".to_string(),
                self.save_pattern.id().to_string(),
            ),
            ("Encoding".to_string(), self.encoding.to_string()),
        ]
    }

    /// The tool's `[Tool]` section as
    /// text — the first-tool spelling.
    pub fn to_ini(&self) -> String {
        self.to_ini_section(0)
    }
}

/// The `Y`/`N` spelling the loader's bool helper compares against.
fn yn(v: bool) -> &'static str {
    if v { "Y" } else { "N" }
}

/// The `#` the saver puts in front of the four free-text values.
/// `GetPrivateProfileStringW` strips a value's leading blanks, so the
/// prefix is what keeps `Args=" $(FileName)"` from losing its space;
/// the loader drops it again.
fn prefixed(s: &str) -> String {
    format!("#{s}")
}

/// A value read back from a `#`-prefixed
/// key: the marker is the first character
/// or it is not there at all.
fn unprefixed(s: &str) -> &str {
    s.strip_prefix('#').unwrap_or(s)
}

/// The `Capture` value: the three ints as
/// the twelve little-endian bytes
/// `WritePrivateProfileStructW` would hex.
fn capture_hex(groups: [i32; 3]) -> String {
    let mut out = String::with_capacity(24);
    for g in groups {
        for b in g.to_le_bytes() {
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Reads a `Capture` value back. The
/// struct API appends a checksum byte, so
/// anything past the twelfth byte is
/// padding and is ignored; a
/// comma-separated triple is accepted too,
/// for a hand-edited file.
fn parse_capture(s: &str) -> [i32; 3] {
    let t = s.trim();
    if !t.is_empty() && t.len() >= 24 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut out = [0i32; 3];
        for (i, slot) in out.iter_mut().enumerate() {
            let mut word = [0u8; 4];
            for (j, b) in word.iter_mut().enumerate() {
                let at = i * 8 + j * 2;
                *b = u8::from_str_radix(&t[at..at + 2], 16).unwrap_or(0);
            }
            *slot = i32::from_le_bytes(word);
        }
        return out;
    }
    let mut out = [0i32; 3];
    let parts: Vec<&str> = t
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    for (slot, part) in out.iter_mut().zip(parts) {
        *slot = part.trim().parse().unwrap_or(0);
    }
    out
}

/// The `tool` directory next to the
/// scanner looks.
pub fn tool_dir() -> PathBuf {
    let mut p = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    p.push("tool");
    p
}

/// Reads every `*.tool` file in `dir`,
/// in filename order, flattening the
/// groups: the runner's and the menu's
/// view, which walks the same scan
/// and indexes the
/// tools by command id. Files that do
/// not parse are skipped.
pub fn load_dir(dir: &Path) -> Vec<Tool> {
    load_groups(dir)
        .into_iter()
        .flat_map(|g| g.tools)
        .collect()
}

/// One `*.tool` file: its group name
/// (the file's stem, which is what
/// `ToolGroupEx` lists and what the
/// manager's combo box shows) and its
/// tools in file order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolGroup {
    pub name: String,
    pub tools: Vec<Tool>,
}

/// Reads every `*.tool` file in `dir`
/// as a group, in filename order.
pub fn load_groups(dir: &Path) -> Vec<ToolGroup> {
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("tool"))
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    paths.sort();
    paths
        .iter()
        .filter_map(|p| {
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let tools = load_file(p);
            if tools.is_empty() {
                None
            } else {
                Some(ToolGroup { name, tools })
            }
        })
        .collect()
}

/// Reads one group file's tools:
/// `[Tool]`, `[Tool1]`, `[Tool2]`, …
/// in the order the sections appear.
pub fn load_file(path: &Path) -> Vec<Tool> {
    Ini::load(path).map(|ini| tools_of(&ini)).unwrap_or_default()
}

/// The tools of an already parsed group file: one section each, file order.
fn tools_of(ini: &Ini) -> Vec<Tool> {
    ini.sections()
        .into_iter()
        .filter(|(name, _)| is_tool_section(name))
        .filter_map(|(_, kv)| {
            let kv: Vec<(String, String)> = kv
                .into_iter()
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect();
            tool_from(&kv)
        })
        .collect()
}

/// Writes one group file: the tools
/// as `[Tool]`, `[Tool1]`, … in
/// order, each key in the loader's
/// order. An empty group writes an
/// empty file, which reads back as no
/// tools — the file still exists so
/// the group keeps its place.
pub fn save_file(path: &Path, tools: &[Tool]) -> std::io::Result<()> {
    let mut ini = Ini::new();
    for (i, tool) in tools.iter().enumerate() {
        let name = if i == 0 {
            "Tool".to_string()
        } else {
            format!("Tool{i}")
        };
        ini.replace_section(&name, &tool.ini_rows());
    }
    ini.save(path)
}

/// The group file's path inside `dir`.
pub fn group_path(dir: &Path, name: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    p.push(format!("{name}.tool"));
    p
}

/// A UUID not yet used by any tool:
/// one past the maximum. The key is
/// what the 外部工具条 buttons and
/// the manager's rows identify a tool
/// by, so a new tool has to be
/// distinguishable from the rest.
pub fn next_uuid<'a>(tools: impl IntoIterator<Item = &'a Tool>) -> i32 {
    tools
        .into_iter()
        .map(|t| t.uuid)
        .max()
        .map(|m| m + 1)
        .unwrap_or(1)
}

/// Parses a group file's tools: the
/// `[Tool]` section plus any `[ToolN]`
/// sections after it, in file order.
pub fn parse_sections(text: &str) -> Vec<Tool> {
    tools_of(&Ini::parse(text))
}

/// The section names that hold a tool: `Tool` for the first, then
/// `Tool1`, `Tool2`, ….
fn is_tool_section(name: &str) -> bool {
    if name == "Tool" {
        return true;
    }
    match name.strip_prefix("Tool") {
        Some(rest) => {
            !rest.is_empty()
                && rest.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// One tool from its section's key/value pairs.
fn tool_from(kv: &[(String, String)]) -> Option<Tool> {
    if kv.is_empty() {
        return None;
    }
    let s = |key: &str| -> String {
        kv.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    // The four `#`-prefixed values lose their
    // marker here, exactly where the loader
    // drops it.
    let h = |key: &str| -> String { unprefixed(&s(key)).to_string() };
    let b = |key: &str| -> bool {
        kv.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.eq_ignore_ascii_case("Y"))
            .unwrap_or(false)
    };
    let i = |key: &str| -> i32 {
        kv.iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.trim().parse::<i32>().ok())
            .unwrap_or(0)
    };
    Some(Tool {
        uuid: i("UUID"),
        title: s("Title"),
        icon: s("Icon"),
        exe_path: h("ExePath"),
        args: h("Args"),
        init_dir: h("InitDir"),
        use_pattern: b("UsePattern"),
        input: b("Input"),
        pattern: h("Pattern"),
        capture: parse_capture(&s("Capture")),
        action_pattern: CaptureMode::from_id(i("ActionPattern")),
        save_pattern: SaveMode::from_id(i("SavePattern")),
        encoding: i("Encoding"),
    })
}

/// Parses one `.tool` file's first
/// `[Tool]` section. `None` when the
/// section is absent
/// readers all take defaults, but a
/// file without the section is not a
/// tool.
pub fn parse_ini(text: &str) -> Option<Tool> {
    parse_sections(text).into_iter().next()
}

/// The values a tool's argument string can expand — the sixteen
/// `$(...)` variables the tool dialog's variable list offers. This
/// implementation fills what the active document knows.
#[derive(Clone, Debug, Default)]
pub struct ToolContext {
    /// Directory + name + extension.
    pub file_path: String,
    /// Directory only.
    pub file_dir: String,
    /// Name with extension.
    pub file_name: String,
    /// Name without extension.
    pub file_name_no_ext: String,
    /// Extension, without the dot.
    pub file_ext: String,
    /// 1-based caret line.
    pub cur_line: usize,
    /// 1-based caret column.
    pub cur_col: usize,
    /// The selection's text, empty when
    /// there is none.
    pub cur_sel: String,
    /// The word under the caret.
    pub cur_word: String,
    /// The caret line's text.
    pub cur_line_text: String,
    /// The executable's directory.
    pub app_path: String,
    /// The four user arguments.
    pub user_args: [String; 4],
}

/// Expands every `$(Name)` in `text`
/// from `ctx`. An unknown name is left
/// as written
/// copies the token through when no
/// table entry matches.
pub fn expand(text: &str, ctx: &ToolContext) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("$(") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(')') else {
            // Unterminated: the rest is literal.
            out.push_str(after);
            return out;
        };
        let name = &after[2..end];
        out.push_str(&value_of(name, ctx));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// One variable's value. The sixteen fields are the variable list's
/// own rows, and they are filled in that list's order rather than in
/// id order — 1, 15, 2, 3, 4, 16, 10, 5, 9, 6, 7, 8, 11-14 — so the
/// order below is data, not an accident.
fn value_of(name: &str, ctx: &ToolContext) -> String {
    match name {
        "FilePath" => ctx.file_path.clone(),
        "FilePathNoExt" => {
            let sep = std::path::MAIN_SEPARATOR;
            if ctx.file_dir.is_empty() {
                ctx.file_name_no_ext.clone()
            } else {
                format!(
                    "{}{}{}",
                    ctx.file_dir, sep, ctx.file_name_no_ext
                )
            }
        }
        "FileDir" => ctx.file_dir.clone(),
        "FileName" => ctx.file_name.clone(),
        "FileNameNoExt" => ctx.file_name_no_ext.clone(),
        "FileExt" => ctx.file_ext.clone(),
        "CurLine" => ctx.cur_line.to_string(),
        "CurCol" => ctx.cur_col.to_string(),
        "CurSel" => ctx.cur_sel.clone(),
        "CurWord" => ctx.cur_word.clone(),
        "CurLineText" => ctx.cur_line_text.clone(),
        "AppPath" => ctx.app_path.clone(),
        "UserArg1" => ctx.user_args[0].clone(),
        "UserArg2" => ctx.user_args[1].clone(),
        "UserArg3" => ctx.user_args[2].clone(),
        "UserArg4" => ctx.user_args[3].clone(),
        // Unknown: the token itself, minus
        // the `$(` and `)` the caller
        // already stripped.
        _ => format!("$({name})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> Tool {
        Tool {
            uuid: 7,
            title: "01. 新工具".into(),
            icon: String::new(),
            exe_path: "C:\\bin\\t.exe".into(),
            args: "$(FileName) $(CurLine)".into(),
            init_dir: "$(FileDir)".into(),
            use_pattern: true,
            input: false,
            pattern: "x".into(),
            capture: [1, 2, 0],
            action_pattern: CaptureMode::CaptureShow,
            save_pattern: SaveMode::Current,
            encoding: 65001,
        }
    }

    #[test]
    fn ini_round_trips_every_key() {
        let text = tool().to_ini();
        assert_eq!(parse_ini(&text), Some(tool()));
    }

    #[test]
    fn a_file_without_the_section_is_not_a_tool()
    {
        assert_eq!(parse_ini("[Other]\nTitle=x\n"), None);
        assert!(parse_sections("[Other]\nTitle=x\n").is_empty());
    }

    #[test]
    fn a_group_file_holds_one_section_per_tool() {
        // `[Tool]` first, then `[Tool1]`,
        // `[Tool2]` — the index appended
        // to the base name.
        let mut a = tool();
        a.uuid = 1;
        a.title = "first".into();
        let mut b = tool();
        b.uuid = 2;
        b.title = "second".into();
        let mut c = tool();
        c.uuid = 3;
        c.title = "third".into();
        let text = format!(
            "{}\n{}",
            a.to_ini_section(0),
            b.to_ini_section(1)
        ) + &c.to_ini_section(2);
        let tools = parse_sections(&text);
        assert_eq!(tools, vec![a, b, c]);
    }

    #[test]
    fn numbered_sections_round_trip_through_a_file()
    {
        let dir = std::env::temp_dir().join("dogedit_tools_test");
        std::fs::create_dir_all(&dir).ok();
        let path = group_path(&dir, "mygroup");
        let mut a = tool();
        a.uuid = 4;
        a.title = "one".into();
        let mut b = tool();
        b.uuid = 5;
        b.title = "two".into();
        save_file(&path, &[a.clone(), b.clone()]).unwrap();
        assert_eq!(load_file(&path), vec![a, b]);
        // the group reads back under the
        // file's stem
        let groups = load_groups(&dir);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "mygroup");
        assert_eq!(groups[0].tools.len(), 2);
        // and the flat view flattens the
        // groups, which is what the
        // runner and the menu index
        assert_eq!(load_dir(&dir).len(), 2);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn uuids_are_one_past_the_maximum() {
        let mut a = tool();
        a.uuid = 3;
        let mut b = tool();
        b.uuid = 9;
        assert_eq!(next_uuid(&[a.clone(), b.clone()]), 10);
        assert_eq!(next_uuid(&[] as &[Tool]), 1);
    }

    #[test]
    fn only_tool_sections_count() {
        // a neighbouring `[Settings]`
        // section is not a tool
        let text = "[Tool]\nTitle=a\n\n[Settings]\nX=1\n\n[Tool1]\nTitle=b\n";
        let tools = parse_sections(text);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].title, "a");
        assert_eq!(tools[1].title, "b");
        // a section that merely starts
        // with the letters is not one
        assert!(parse_sections("[Toolbox]\nTitle=x\n").is_empty());
    }

    #[test]
    fn bools_read_y_and_n() {
        let text = "[Tool]\nUsePattern=Y\nInput=n\n";
        let t = parse_ini(text).unwrap();
        assert!(t.use_pattern);
        assert!(!t.input);
    }

    #[test]
    fn capture_ids_match_menu317() {
        assert_eq!(
            CaptureMode::from_id(2),
            CaptureMode::CaptureHide
        );
        assert_eq!(
            CaptureMode::from_id(9),
            CaptureMode::CaptureShow
        );
        assert_eq!(
            CaptureMode::from_id(8),
            CaptureMode::WebPreview
        );
        assert_eq!(
            CaptureMode::from_id(7),
            CaptureMode::RunHidden
        );
        assert_eq!(CaptureMode::None.id(), 1);
        assert_eq!(CaptureMode::ReplaceDocument.id(), 6);
        // an unknown value reads as None
        assert_eq!(CaptureMode::from_id(42), CaptureMode::None);
    }

    /// The `Capture` key is the twelve-byte
    /// struct, not a mode: three ints, and a
    /// checksum byte from the struct API is
    /// padding rather than an error.
    #[test]
    fn capture_is_the_three_group_struct() {
        assert_eq!(capture_hex([0, 0, 0]), "000000000000000000000000");
        assert_eq!(capture_hex([1, 2, 9]), "010000000200000009000000");
        assert_eq!(
            parse_capture("010000000200000009000000"),
            [1, 2, 9]
        );
        // the trailing checksum byte is ignored
        assert_eq!(
            parse_capture("010000000200000009000000A5"),
            [1, 2, 9]
        );
        // a hand-edited triple reads too
        assert_eq!(parse_capture("1, 2, 3"), [1, 2, 3]);
        assert_eq!(parse_capture(""), [0, 0, 0]);
        assert_eq!(parse_capture("garbage"), [0, 0, 0]);
    }

    /// The four free-text values carry the
    /// `#` marker both ways, so a value that
    /// starts with a blank survives.
    #[test]
    fn free_text_values_keep_their_leading_blank() {
        let mut t = tool();
        t.args = " $(FileName)".into();
        let text = t.to_ini();
        assert!(text.contains("Args=# $(FileName)\n"));
        assert_eq!(parse_ini(&text), Some(t));
        // a file written without the marker
        // still reads
        let plain = "[Tool]\nArgs=$(CurLine)\n";
        assert_eq!(
            parse_ini(plain).unwrap().args,
            "$(CurLine)"
        );
    }

    #[test]
    fn save_ids_match_menu318() {
        assert_eq!(SaveMode::from_id(2), SaveMode::Current);
        assert_eq!(SaveMode::from_id(3), SaveMode::All);
        assert_eq!(SaveMode::None.id(), 1);
        assert_eq!(SaveMode::from_id(0), SaveMode::None);
    }

    /// The numbering lives in the menu caption (`%.2d. %s`), not in the
    /// title — a numbered title would render as `01. 01. 新工具`.
    #[test]
    fn new_tool_titles_are_plain() {
        assert_eq!(Tool::blank().title, "新工具");
    }

    #[test]
    fn every_variable_expands() {
        let ctx = ToolContext {
            file_path: "/a/b/c.txt".into(),
            file_dir: "/a/b".into(),
            file_name: "c.txt".into(),
            file_name_no_ext: "c".into(),
            file_ext: "txt".into(),
            cur_line: 12,
            cur_col: 3,
            cur_sel: "sel".into(),
            cur_word: "word".into(),
            cur_line_text: "line".into(),
            app_path: "/app".into(),
            user_args: [
                "1".into(),
                "2".into(),
                "3".into(),
                "4".into(),
            ],
        };
        assert_eq!(expand("$(FilePath)", &ctx), "/a/b/c.txt");
        assert_eq!(
            expand("$(FilePathNoExt)", &ctx),
            format!(
                "/a/b{}c",
                std::path::MAIN_SEPARATOR
            )
        );
        assert_eq!(expand("$(FileDir)", &ctx), "/a/b");
        assert_eq!(expand("$(FileName)", &ctx), "c.txt");
        assert_eq!(expand("$(FileNameNoExt)", &ctx), "c");
        assert_eq!(expand("$(FileExt)", &ctx), "txt");
        assert_eq!(expand("$(CurLine)", &ctx), "12");
        assert_eq!(expand("$(CurCol)", &ctx), "3");
        assert_eq!(expand("$(CurSel)", &ctx), "sel");
        assert_eq!(expand("$(CurWord)", &ctx), "word");
        assert_eq!(expand("$(CurLineText)", &ctx), "line");
        assert_eq!(expand("$(AppPath)", &ctx), "/app");
        assert_eq!(expand("$(UserArg4)", &ctx), "4");
        // several in one string, with literals around
        assert_eq!(
            expand("x $(FileName):$(CurLine) y", &ctx),
            "x c.txt:12 y"
        );
    }

    #[test]
    fn an_unknown_variable_survives() {
        let ctx = ToolContext::default();
        assert_eq!(expand("$(Nope)", &ctx), "$(Nope)");
        assert_eq!(expand("a $(Nope) b", &ctx), "a $(Nope) b");
    }

    #[test]
    fn an_unterminated_token_is_literal() {
        let ctx = ToolContext::default();
        assert_eq!(expand("a $(FilePath", &ctx), "a $(FilePath");
    }
}
