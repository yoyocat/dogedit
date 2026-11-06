//! The status strip's model: the pane table a native `msctls_statusbar32` is
//! filled with.
//!
//! Painting, hit-testing and hover belong to the common control; the frame only
//! decides which panes exist, in which order, and what each one shows.
//! [`StatusField`] names a pane by the fact it displays, [`PANES`] is the default
//! left-to-right order, and [`StatusModel`] is the state the frame hands over.

use dg_render::theme::Theme;

/// Status bar height at 96 DPI.
pub const HEIGHT: f32 = 23.0;

/// Text measured by the status-pane width routine: the part is `extent + 4` wide.
///
/// 这是**默认值**（= 2 × `status.pad`）；皮肤改了度量就用 [`WidthRules`] 里的数。
pub const TEXT_EXTRA: i32 = 4;
/// A pane that can show an icon reserves `extent + 36` instead. Four panes can:
/// [`StatusField::Encoding`], [`StatusField::Eol`], [`StatusField::TabSize`] and
/// [`StatusField::Syntax`].
pub const ICON_EXTRA: i32 = 36;
/// The width routine never gives a part less than 45 px, so the two empty panes
/// stay on screen and the row does not reflow.
pub const MIN_WIDTH: i32 = 45;

/// Identifies a status bar pane. The declaration order is the default
/// left-to-right order, and [`StatusField::Message`] is the stretch pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusField {
    /// "Ready", or the transient message the last command left behind.
    Message,
    /// The file size: `%d B` / `%.1f KB` / `%.1f MB` / `%.1f G`.
    FileSize,
    /// 同步滚动, shown only while a synchronized view exists.
    Sync,
    /// The timed selection readout: how many characters and how many lines.
    Selection,
    /// The syntax display name, or 默认 when the document has none.
    Syntax,
    /// 制表符:N or 软空格:N, whichever mode the document is in.
    TabSize,
    /// 行 N, counting from 1.
    Ln,
    /// 列 N, counting from 0.
    Col,
    /// The character under the caret, as `U+%04X`.
    CharValue,
    /// A plain line count.
    LineCount,
    /// "CRLF" / "LF" / "CR".
    Eol,
    /// 插入 / 覆写 / 列模式 / 多选.
    EditMode,
    /// "ANSI" / "CP %d" / "UTF-8", with a trailing `+` when the file has a BOM.
    Encoding,
    /// 偏移: %08X, the hex view's caret byte. Only the hex pane table uses it.
    Offset,
}

impl StatusField {
    /// Stable lowercase name — the `part` a skin callback branches on, and
    /// the word the docs list. Never translated: it is an identifier, not a
    /// label.
    pub fn name(self) -> &'static str {
        match self {
            StatusField::Message => "message",
            StatusField::FileSize => "filesize",
            StatusField::Sync => "sync",
            StatusField::Selection => "selection",
            StatusField::Syntax => "syntax",
            StatusField::TabSize => "tabsize",
            StatusField::Ln => "ln",
            StatusField::Col => "col",
            StatusField::CharValue => "char",
            StatusField::LineCount => "lines",
            StatusField::Eol => "eol",
            StatusField::EditMode => "editmode",
            StatusField::Encoding => "encoding",
            StatusField::Offset => "offset",
        }
    }
}

/// The default order, left to right: the stretch pane, then file size, sync,
/// selection, syntax, tab size, line, column, character, line count, EOL, edit
/// mode and encoding. [`StatusField::Offset`] is not in it: only the hex
/// table below shows that pane.
pub const PANES: [StatusField; 14] = [
    StatusField::Message,
    StatusField::FileSize,
    StatusField::Sync,
    StatusField::Selection,
    StatusField::Syntax,
    StatusField::TabSize,
    StatusField::Ln,
    StatusField::Col,
    StatusField::CharValue,
    StatusField::LineCount,
    StatusField::Eol,
    StatusField::EditMode,
    StatusField::Encoding,
    StatusField::Offset,
];

impl StatusField {
    pub fn pane_id(self) -> u32 {
        match self {
            StatusField::Message => 0,
            StatusField::Ln => 1,
            StatusField::Col => 2,
            StatusField::CharValue => 3,
            StatusField::LineCount => 4,
            StatusField::Encoding => 5,
            StatusField::EditMode => 6,
            StatusField::Eol => 7,
            StatusField::TabSize => 8,
            StatusField::Syntax => 9,
            StatusField::Selection => 10,
            StatusField::Sync => 11,
            StatusField::FileSize => 12,
            StatusField::Offset => 14,
        }
    }

    /// The pane a `WM_COMMAND` notification refers to.
    pub fn from_pane_id(id: u32) -> Option<Self> {
        PANES.iter().copied().find(|f| f.pane_id() == id)
    }

    /// 按名字找回字段（`s:status` 的段名就是这个名字）。
    pub fn from_name(name: &str) -> Option<Self> {
        PANES.iter().copied().find(|f| f.name() == name)
    }

    pub fn reserves_icon(self) -> bool {
        matches!(
            self.pane_id(),
            5 | 7 | 8 | 9 | 13
        )
    }
}

/// Edit mode indicator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditMode {
    Insert,
    Overwrite,
    Column,
    Multi,
}

/// End-of-line mode. Defined in `dg-core` (the document owns it);
/// re-exported so the status model stays plain data.
pub use dg_core::Eol;

/// The selection pane's two numbers: how many characters, how many lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub chars: usize,
    pub lines: usize,
}

/// What the strip shows while a hex document is on screen: the hex pane
/// table replaces the text one, and the numbers come from the byte model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HexStatus {
    /// Selected bytes: the distance between the two anchors.
    pub selection: u32,
    /// The caret's byte offset, printed as 8 hex digits.
    pub offset: u32,
    /// Total bytes.
    pub size: u32,
    pub overwrite: bool,
}

/// Plain-data model the host fills from the editor state every paint (keeps
/// `dg-ui` free of any editor dependency).
#[derive(Clone, Debug, Default)]
pub struct StatusModel {
    /// The stretch pane's text; `None` shows 就绪.
    pub message: Option<String>,
    pub line: Option<usize>,
    /// 0-based
    pub col: Option<usize>,
    pub char_value: Option<u32>,
    pub line_count: Option<usize>,
    pub encoding: Option<String>,
    pub edit_mode: Option<EditMode>,
    pub eol: Option<Eol>,
    pub tab_size: Option<u32>,
    /// The soft-tab view option — 软空格 instead of 制表符.
    pub soft_tab: bool,
    pub syntax: Option<String>,
    /// `None` or a zero-length selection leaves the pane empty.
    pub selection: Option<Selection>,
    pub sync: bool,
    pub file_size: Option<u64>,
    /// `Some` while a binary document is active: the strip switches to the
    /// hex pane table and reads these numbers instead of the text fields.
    pub hex: Option<HexStatus>,
}

/// One pane as the control needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusPane {
    pub field: StatusField,
    /// Identifier the control reports back on click.
    pub id: u32,
    pub text: String,
    /// 这一格的名字：内置模式下就是 [`StatusField::name`]，皮肤模式下是
    /// `s:status` 里那个 `id`（也是绘制区域名 `status.<name>` 的后半截）。
    ///
    /// 两套都填，是因为**皮肤可以只声明一条整行里的几格**（`s:status_defaults()`
    /// 加一段），那些继承来的格还得按内置名字取文字、按内置规则留图标位。
    pub name: String,
    /// 皮肤模式下点这一格跑哪个命令。空 = 走内置那张表（[`StatusField`] →
    /// 菜单/命令）。内置模式下恒空。
    pub click: String,
}

/// 量一格宽度用的三个数，**设备像素**。
///
/// 内置那条就是 [`WidthRules::DEFAULT`]（三个常量原样）；皮肤改了
/// `[metrics]` 里的 `status.pad` / `status.icon_pad` / `status.min_width`，
/// 外壳就按皮肤的数来 —— 内边距是**画**和**量**共用的一把尺子，只改一边
/// 就会把文字量窄，最后被 `ellipsis` 吃掉半截。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WidthRules {
    /// 纯文字格左右**合计**留多少。默认 [`TEXT_EXTRA`]（= 2 × `status.pad`）。
    pub text_pad: i32,
    /// 带图标的格留多少（**替换** `text_pad`，不是相加）。默认 [`ICON_EXTRA`]。
    pub icon_pad: i32,
    /// 一格的下限。默认 [`MIN_WIDTH`]。
    pub min_width: i32,
}

impl WidthRules {
    /// 内置那条的三个数。
    pub const DEFAULT: WidthRules = WidthRules {
        text_pad: TEXT_EXTRA,
        icon_pad: ICON_EXTRA,
        min_width: MIN_WIDTH,
    };
}

/// Width the status-pane width routine gives a pane whose text measures `extent_cx`.
///
/// 文字用随 DPI 放大的系统 UI 字体画，所以只有 extent 跟着 DPI 长；边距和
/// 下限由 `rules` 给（外壳按 DPI 折算好了再传进来，见 [`WidthRules`]）。
pub fn pane_width(extent_cx: i32, id: u32, rules: WidthRules) -> i32 {
    let extra = if (9120u32 >> id.min(31)) & 1 != 0 {
        rules.icon_pad
    } else {
        rules.text_pad
    };
    (extent_cx + extra).max(rules.min_width)
}

/// The strip's left-to-right order when `[Setting] SBPane` is absent:
/// the stretch pane first
pub const DEFAULT_ORDER: [u32; 13] = [0, 12, 11, 10, 9, 8, 1, 2, 3, 4, 7, 6, 5];

/// The hex document's pane table, left to right: the stretch pane, the
/// selected byte count, the caret offset, the byte size and 插入/覆写.
pub const HEX_ORDER: [u32; 5] = [0, 10, 14, 12, 6];

/// The words every pane is built from. The defaults are this repository's
/// bundled Chinese table (`15038` 插入, `15229` 行, …); the Win32 backend replaces
/// them with the selected language's entries through
/// [`StatusBar::set_labels`].
#[derive(Clone, Debug)]
pub struct StatusLabels {
    pub ready: String,
    pub sync: String,
    pub default_syntax: String,
    pub soft_tab: String,
    pub tab: String,
    pub ln: String,
    pub col: String,
    pub insert: String,
    pub overwrite: String,
    pub column: String,
    pub multi: String,
    /// `15034`: two `%d` placeholders, chars then lines.
    pub selection: String,
    /// `15095`: one `%d`, the selected byte count.
    pub hex_selection: String,
    /// `15096`: a `%08X` conversion of the caret offset.
    pub hex_offset: String,
    /// `15097`: one `%d`, the byte count.
    pub hex_size: String,
}

impl Default for StatusLabels {
    fn default() -> Self {
        Self {
            ready: "就绪".to_string(),
            sync: "同步滚动".to_string(),
            default_syntax: "默认".to_string(),
            soft_tab: "软空格".to_string(),
            tab: "制表符".to_string(),
            ln: "行".to_string(),
            col: "列".to_string(),
            insert: "插入".to_string(),
            overwrite: "覆写".to_string(),
            column: "列模式".to_string(),
            multi: "多选".to_string(),
            selection: "%d字符,%d行".to_string(),
            hex_selection: "选区: %d".to_string(),
            hex_offset: "偏移: %08X".to_string(),
            hex_size: "大小: %d".to_string(),
        }
    }
}

/// The status strip's pane table.
pub struct StatusBar {
    /// `[Setting] SBPane`: the text document's table, kept so a tab switch
    /// back from a hex document restores it.
    configured: Vec<u32>,
    /// The table currently in the control.
    order: Vec<u32>,
    texts: Vec<String>,
    labels: StatusLabels,
    /// A binary document is on screen: [`HEX_ORDER`] is in the control.
    hex_mode: bool,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            configured: DEFAULT_ORDER.to_vec(),
            order: DEFAULT_ORDER.to_vec(),
            texts: vec![String::new(); DEFAULT_ORDER.len()],
            labels: StatusLabels::default(),
            hex_mode: false,
        }
    }

    /// Replaces the words the pane texts are built from (a language switch is
    /// a restart, so this is set once when the frame starts).
    pub fn set_labels(&mut self, labels: StatusLabels) {
        self.labels = labels;
    }

    /// Replace the pane ids after the stretch pane — `[Setting] SBPane`, in
    /// list order.
    pub fn set_order(&mut self, ids: &[u32]) -> bool {
        let mut order = vec![0u32];
        for id in ids {
            if *id != 0 && !order.contains(id) {
                order.push(*id);
            }
        }
        if order == self.configured {
            return false;
        }
        self.configured = order;
        if self.hex_mode {
            // The hex table is fixed; the new text table takes over when the
            // next text document shows.
            return false;
        }
        self.order = self.configured.clone();
        self.texts = vec![String::new(); self.order.len()];
        true
    }

    /// Swaps the pane table between the configured text one and the hex
    /// document's fixed four. Returns whether the table changed.
    pub fn set_hex_mode(&mut self, on: bool) -> bool {
        if on == self.hex_mode {
            return false;
        }
        self.hex_mode = on;
        self.order = if on {
            HEX_ORDER.to_vec()
        } else {
            self.configured.clone()
        };
        self.texts = vec![String::new(); self.order.len()];
        true
    }

    /// The order in the control, stretch pane included.
    pub fn order(&self) -> &[u32] {
        &self.order
    }

    /// Replace the model contents. Returns `true` when any text changed, which
    /// is when the host re-sends the pane table to the control.
    pub fn set_model(&mut self, model: &StatusModel) -> bool {
        let mut changed = self.set_hex_mode(model.hex.is_some());
        let texts = format_panes(model, &self.labels);
        for (id, slot) in self.order.iter().zip(self.texts.iter_mut()) {
            let text = PANES
                .iter()
                .position(|field| field.pane_id() == *id)
                .map(|index| texts[index].clone())
                .unwrap_or_default();
            if *slot != text {
                *slot = text;
                changed = true;
            }
        }
        changed
    }

    /// The pane table, left to right.
    pub fn panes(&self) -> Vec<StatusPane> {
        self.order
            .iter()
            .zip(self.texts.iter())
            .map(|(&id, text)| {
                let field = StatusField::from_pane_id(id).unwrap_or(StatusField::Message);
                StatusPane {
                    field,
                    id,
                    text: text.clone(),
                    name: field.name().to_string(),
                    click: String::new(),
                }
            })
            .collect()
    }

    /// 内置某一格现在的文字，按 [`StatusField::name`] 查。不是内置字段就是空串。
    ///
    /// 皮肤模式用它给**继承来的**格取文字：`就绪`、`4.2 KB`、`U+4E2D` 这些
    /// 模板表达不了（要查语言表、要格式化字节数），重写一遍必然和内置那条分叉。
    pub fn text_of(&self, name: &str) -> String {
        let Some(field) = PANES.iter().copied().find(|f| f.name() == name) else {
            return String::new();
        };
        self.order
            .iter()
            .position(|id| *id == field.pane_id())
            .and_then(|i| self.texts.get(i).cloned())
            .unwrap_or_default()
    }

    /// `name` 是不是一个内置字段名。
    pub fn has_field(name: &str) -> bool {
        PANES.iter().any(|f| f.name() == name)
    }

    /// 内置字段 `name` 要不要留图标位（编码/换行/制表符/语法四格）。
    pub fn field_reserves_icon(name: &str) -> bool {
        PANES
            .iter()
            .find(|f| f.name() == name)
            .is_some_and(|f| f.reserves_icon())
    }

    /// 内置字段 `name` 的基准样本文本（只给「每帧都在变」的那几格）。
    pub fn field_sample(&self, name: &str) -> Option<String> {
        PANES
            .iter()
            .find(|f| f.name() == name)
            .and_then(|f| self.default_text(f.pane_id()))
    }

    /// 编辑模式那个词（插入 / 覆写 / 列模式 / 多选）。模板的 `%M` 用它 ——
    /// 和「编辑模式」那一格**同一个词**，两处各写一份迟早会分叉。
    pub fn mode_word(&self, mode: EditMode) -> String {
        match mode {
            EditMode::Insert => self.labels.insert.clone(),
            EditMode::Overwrite => self.labels.overwrite.clone(),
            EditMode::Column => self.labels.column.clone(),
            EditMode::Multi => self.labels.multi.clone(),
        }
    }

    /// The base width pane `id` is measured against when its live text cannot
    /// settle: the panes that change on nearly every keystroke or caret move.
    /// A comfortably large sample, not the widest possible one, so the strip
    /// stays tight; the host still widens the part when live text outgrows the
    /// sample. Panes that change rarely return `None` and are measured from
    /// their own text.
    pub fn default_text(&self, id: u32) -> Option<String> {
        default_text(id, &self.labels, self.hex_mode)
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

/// The base text the panes that change constantly are measured against.
///
/// These are the panes a caret move or a keystroke rewrites: the line, the
/// column, the character under the caret, the line count and the size, plus
/// the hex view's selection and byte count. Each holds one digit of headroom
/// past what a document usually shows — enough that the common 行 9 → 行 100
/// does not move anything, small enough that an idle strip stays tight.
/// Everything else — the text selection, the tab width, the syntax, the
/// encoding, the EOL, the edit mode — changes too rarely, or spends most of
/// its life empty, and is measured from its own text.
const LINE_SAMPLE: &str = "999";
const COUNT_SAMPLE: &str = "9999";
const SELECTION_BYTES_SAMPLE: usize = 9_999_999;
const BYTES_SAMPLE: usize = 99_999_999;
/// Three digits and a decimal, then a two-letter unit: keeps the pane still
/// when the size crosses from bytes to KB to MB.
const SIZE_SAMPLE: &str = "999.9 MB";

fn default_text(id: u32, l: &StatusLabels, hex: bool) -> Option<String> {
    Some(match id {
        1 => format!("{} {LINE_SAMPLE}", l.ln),
        2 => format!("{} {LINE_SAMPLE}", l.col),
        3 => "U+10FFFF".to_string(),
        4 => COUNT_SAMPLE.to_string(),
        10 if hex => format_ints(&l.hex_selection, &[SELECTION_BYTES_SAMPLE]),
        12 if hex => format_ints(&l.hex_size, &[BYTES_SAMPLE]),
        12 => SIZE_SAMPLE.to_string(),
        _ => return None,
    })
}

/// The pane texts, in [`PANES`] order.
fn format_panes(m: &StatusModel, l: &StatusLabels) -> [String; PANES.len()] {
    let hex = m.hex;
    [
        m.message.clone().unwrap_or_else(|| l.ready.clone()),
        match hex {
            Some(h) => format_ints(&l.hex_size, &[h.size as usize]),
            None => m.file_size.map(format_size).unwrap_or_default(),
        },
        if m.sync { l.sync.clone() } else { String::new() },
        match hex {
            Some(h) => format_ints(&l.hex_selection, &[h.selection as usize]),
            None => match m.selection {
                Some(sel) if sel.chars > 0 => {
                    format_ints(&l.selection, &[sel.chars, sel.lines])
                }
                _ => String::new(),
            },
        },
        m.syntax.clone().unwrap_or_else(|| l.default_syntax.clone()),
        m.tab_size
            .map(|n| {
                format!(
                    "{}:{}",
                    if m.soft_tab { &l.soft_tab } else { &l.tab },
                    n
                )
            })
            .unwrap_or_default(),
        m.line.map(|n| format!("{} {n}", l.ln)).unwrap_or_default(),
        m.col.map(|n| format!("{} {n}", l.col)).unwrap_or_default(),
        m.char_value.map(|v| format!("U+{v:04X}")).unwrap_or_default(),
        m.line_count.map(|n| n.to_string()).unwrap_or_default(),
        match m.eol {
            Some(Eol::Crlf) => "CRLF".to_string(),
            Some(Eol::Lf) => "LF".to_string(),
            Some(Eol::Cr) => "CR".to_string(),
            None => String::new(),
        },
        match hex {
            Some(h) => {
                if h.overwrite {
                    l.overwrite.clone()
                } else {
                    l.insert.clone()
                }
            }
            None => match m.edit_mode {
                Some(EditMode::Insert) => l.insert.clone(),
                Some(EditMode::Overwrite) => l.overwrite.clone(),
                Some(EditMode::Column) => l.column.clone(),
                Some(EditMode::Multi) => l.multi.clone(),
                None => String::new(),
            },
        },
        m.encoding.clone().unwrap_or_default(),
        hex.map(|h| format_hex(&l.hex_offset, h.offset))
            .unwrap_or_default(),
    ]
}

/// Fills a `%0NX` / `%0Nx` conversion in `fmt` with `value` — the shape the
/// offset pane prints. Formats without one are left alone.
fn format_hex(fmt: &str, value: u32) -> String {
    let Some(start) = fmt.find('%') else {
        return fmt.to_string();
    };
    let rest = &fmt[start + 1..];
    let bytes = rest.as_bytes();
    let mut i = 0;
    if bytes.first() == Some(&b'0') {
        i = 1;
    }
    let mut width = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        width = width * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    let upper = match bytes.get(i) {
        Some(b'X') => true,
        Some(b'x') => false,
        _ => return fmt.to_string(),
    };
    let body = if upper {
        format!("{value:0width$X}")
    } else {
        format!("{value:0width$x}")
    };
    format!("{}{}{}", &fmt[..start], body, &rest[i + 1..])
}

/// Fills each `%d` in `fmt` with the next value
/// `wsprintf` does for the selection readout (`%d字符,%d行`).
fn format_ints(fmt: &str, values: &[usize]) -> String {
    let mut out = String::new();
    let mut rest = fmt;
    let mut values = values.iter();
    while let Some(pos) = rest.find("%d") {
        out.push_str(&rest[..pos]);
        if let Some(value) = values.next() {
            out.push_str(&value.to_string());
        } else {
            out.push_str("%d");
        }
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

/// File size exactly as the file-size updater formats it: B, then one decimal
/// in the largest unit, up to terabytes. The cutoffs are 1024 steps, not 1000,
/// and the divisions are done in `float`.
pub fn format_size(bytes: u64) -> String {
    if bytes <= 1024 {
        format!("{bytes} B")
    } else if bytes <= 0x100000 {
        format!("{:.1} KB", (bytes as f32) * 0.0009765625)
    } else if bytes <= 0x40000000 {
        format!("{:.1} MB", (bytes as f32) * 0.00000095367432)
    } else if bytes <= 0x10000000000 {
        format!("{:.1} GB", (bytes as f32) * 9.3132257e-10)
    } else {
        format!("{:.1} TB", (bytes as f32) * 9.094947e-13)
    }
}

/// The label [`StatusField::Encoding`] shows. `encoding_rs` exposes only
/// normative names, so the handful this implementation can produce are mapped to
/// the short form the strip prints; anything unlisted is shown as its own name.
/// Only the three UTF forms take the `+` that marks a BOM.
pub fn encoding_label(name: &str, has_bom: bool) -> String {
    let base = match name {
        "UTF-8" => "UTF-8",
        "UTF-16LE" => "UTF-16",
        "UTF-16BE" => "UTF-16 BE",
        "GB18030" => "GB18030",
        "gbk" => "CP 936",
        "windows-1252" => "ANSI",
        "windows-1251" => "CP 1251",
        "Big5" => "CP 950",
        "Shift_JIS" => "CP 932",
        "EUC-KR" => "CP 949",
        other => other,
    };
    let plus = has_bom && matches!(base, "UTF-8" | "UTF-16" | "UTF-16 BE");
    if plus {
        format!("{base}+")
    } else {
        base.to_string()
    }
}

/// The strip's colours, so the host's painter and the theme agree on one
/// source.
pub fn palette(theme: &Theme) -> (dg_render::Color, dg_render::Color, dg_render::Color) {
    (theme.statusbar_bg, theme.statusbar_fg, theme.statusbar_divider)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_model() -> StatusModel {
        StatusModel {
            message: None,
            line: Some(1),
            col: Some(0),
            char_value: Some(0x2F),
            line_count: Some(8576),
            encoding: Some("UTF-8".into()),
            edit_mode: Some(EditMode::Insert),
            eol: Some(Eol::Crlf),
            tab_size: Some(4),
            soft_tab: false,
            syntax: Some("Rust".into()),
            selection: None,
            sync: false,
            file_size: Some(297_696),
            hex: None,
        }
    }

    #[test]
    fn panes_come_out_in_the_models_declared_order() {
        let mut sb = StatusBar::new();
        assert!(sb.set_model(&full_model()));
        let panes = sb.panes();
        let texts: Vec<&str> = panes.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "就绪", "290.7 KB", "", "", "Rust", "制表符:4", "行 1", "列 0",
                "U+002F", "8576", "CRLF", "插入", "UTF-8"
            ]
        );
        let ids: Vec<u32> = panes.iter().map(|p| p.id).collect();
        assert_eq!(ids, [0, 12, 11, 10, 9, 8, 1, 2, 3, 4, 7, 6, 5]);
        assert_eq!(panes[0].field, StatusField::Message);
    }

    /// `[Setting] SBPane` decides which panes exist and their order; the
    /// message pane always leads and duplicate ids are dropped.
    #[test]
    fn configured_order_follows_sbpane() {
        let mut sb = StatusBar::new();
        assert!(sb.set_order(&[12, 9]));
        assert!(sb.set_model(&full_model()));
        let panes = sb.panes();
        let ids: Vec<u32> = panes.iter().map(|p| p.id).collect();
        let texts: Vec<&str> = panes.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(ids, [0, 12, 9]);
        assert_eq!(texts, ["就绪", "290.7 KB", "Rust"]);

        assert!(sb.set_order(&[0, 12, 12, 99]));
        assert_eq!(sb.order(), [0, 12, 99]);

        assert!(sb.set_order(&[]));
        assert_eq!(sb.order(), [0]);
        assert!(sb.set_model(&full_model()));
        assert_eq!(sb.panes().len(), 1);
    }

    /// 每一格都带着自己的名字 —— 皮肤模式按名字取文字、按名字找图标位。
    #[test]
    fn every_pane_carries_its_field_name() {
        let mut sb = StatusBar::new();
        sb.set_model(&full_model());
        let panes = sb.panes();
        let names: Vec<&str> = panes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "message", "filesize", "sync", "selection", "syntax", "tabsize",
                "ln", "col", "char", "lines", "eol", "editmode", "encoding"
            ]
        );
        // 内置模式下没有脚本命令：点击走 `StatusField` 那张表。
        assert!(panes.iter().all(|p| p.click.is_empty()));
    }

    /// 按名字取文字：内置字段给当前文字，别的名字给空串（皮肤自己加的那几段
    /// 没有内置文字可继承）。
    #[test]
    fn text_of_finds_a_builtin_field_by_name() {
        let mut sb = StatusBar::new();
        sb.set_model(&full_model());
        assert_eq!(sb.text_of("ln"), "行 1");
        assert_eq!(sb.text_of("encoding"), "UTF-8");
        assert_eq!(sb.text_of("message"), "就绪");
        assert_eq!(sb.text_of("clock"), "", "not a builtin field");
        assert!(StatusBar::has_field("eol"));
        assert!(!StatusBar::has_field("clock"));
        // 四格留图标位：编码、换行、制表符、语法。
        for name in ["encoding", "eol", "tabsize", "syntax"] {
            assert!(StatusBar::field_reserves_icon(name), "{name}");
        }
        assert!(!StatusBar::field_reserves_icon("ln"));
        // 每帧都在变的几格有基准样本，别的没有。
        assert_eq!(sb.field_sample("ln").as_deref(), Some("行 999"));
        assert_eq!(sb.field_sample("eol"), None);
    }

    /// `%M` 用的那个词和「编辑模式」那一格是同一个 —— 两处各写一份迟早分叉。
    #[test]
    fn the_mode_word_matches_the_edit_mode_pane() {
        let sb = StatusBar::new();
        assert_eq!(sb.mode_word(EditMode::Insert), "插入");
        assert_eq!(sb.mode_word(EditMode::Overwrite), "覆写");
        assert_eq!(sb.mode_word(EditMode::Column), "列模式");
        assert_eq!(sb.mode_word(EditMode::Multi), "多选");
    }

    #[test]
    fn format_size_steps_at_1024_and_keeps_one_decimal() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1024 B");
        assert_eq!(format_size(1025), "1.0 KB");
        assert_eq!(format_size(297_696), "290.7 KB");
        assert_eq!(format_size(0x100000), "1024.0 KB");
        assert_eq!(format_size(0x100001), "1.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(format_size(0x40000000), "1024.0 MB");
        assert_eq!(format_size(0x40000001), "1.0 GB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(format_size(0x10000000000), "1024.0 GB");
        assert_eq!(format_size(0x10000000001), "1.0 TB");
        assert_eq!(format_size(2 * 1024 * 1024 * 1024 * 1024), "2.0 TB");
    }

    /// `set_model` is what decides whether the control gets a new
    /// `SB_SETPARTS`, so a caret move must report a change and an idle repaint
    /// must not.
    #[test]
    fn set_model_reports_only_real_changes() {
        let mut sb = StatusBar::new();
        let m = full_model();
        assert!(sb.set_model(&m));
        assert!(!sb.set_model(&m));
        let mut moved = m.clone();
        moved.col = Some(4);
        assert!(sb.set_model(&moved));
    }

    #[test]
    fn pane_ids_round_trip() {
        for f in PANES {
            assert_eq!(StatusField::from_pane_id(f.pane_id()), Some(f));
        }
        assert_eq!(StatusField::from_pane_id(13), None);
    }

    /// A hex document swaps in its own four panes and their formats; the
    /// configured text table comes back when the next text tab shows.
    #[test]
    fn hex_model_uses_the_binary_panes() {
        let mut m = full_model();
        m.hex = Some(HexStatus {
            selection: 0,
            offset: 0x176,
            size: 58_927,
            overwrite: true,
        });
        let mut sb = StatusBar::new();
        assert!(sb.set_model(&m));
        let texts: Vec<String> = sb.panes().iter().map(|p| p.text.clone()).collect();
        assert_eq!(
            texts,
            ["就绪", "选区: 0", "偏移: 00000176", "大小: 58927", "覆写"]
        );
        let ids: Vec<u32> = sb.panes().iter().map(|p| p.id).collect();
        assert_eq!(ids, [0, 10, 14, 12, 6]);

        m.hex = Some(HexStatus {
            selection: 3,
            offset: 0x1f,
            size: 3,
            overwrite: false,
        });
        assert!(sb.set_model(&m));
        let texts: Vec<String> = sb.panes().iter().map(|p| p.text.clone()).collect();
        assert_eq!(
            texts,
            ["就绪", "选区: 3", "偏移: 0000001F", "大小: 3", "插入"]
        );

        m.hex = None;
        assert!(sb.set_model(&m));
        assert_eq!(sb.panes().len(), 13);
        assert_eq!(sb.order(), DEFAULT_ORDER);
    }

    #[test]
    fn hex_offset_follows_the_format() {
        assert_eq!(format_hex("偏移: %08X", 0x1f), "偏移: 0000001F");
        assert_eq!(format_hex("Offset: %08x", 0xab), "Offset: 000000ab");
        assert_eq!(format_hex("%X", 0xab), "AB");
        assert_eq!(format_hex("no conversion", 1), "no conversion");
    }

    /// The panes that change on every caret move or keystroke hold a base
    /// width; the rest are measured from their own text.
    #[test]
    fn frequent_panes_hold_a_base_width() {
        let sb = StatusBar::new();
        assert_eq!(sb.default_text(0), None);
        assert_eq!(sb.default_text(1).as_deref(), Some("行 999"));
        assert_eq!(sb.default_text(2).as_deref(), Some("列 999"));
        assert_eq!(sb.default_text(3).as_deref(), Some("U+10FFFF"));
        assert_eq!(sb.default_text(4).as_deref(), Some("9999"));
        assert_eq!(sb.default_text(5), None);
        assert_eq!(sb.default_text(6), None);
        assert_eq!(sb.default_text(7), None);
        assert_eq!(sb.default_text(8), None);
        assert_eq!(sb.default_text(9), None);
        assert_eq!(sb.default_text(10), None);
        assert_eq!(sb.default_text(11), None);
        assert_eq!(sb.default_text(12).as_deref(), Some("999.9 MB"));
        assert_eq!(sb.default_text(14), None);
    }

    #[test]
    fn hex_frequent_panes_hold_a_base_width() {
        let mut sb = StatusBar::new();
        assert!(sb.set_hex_mode(true));
        assert_eq!(sb.default_text(10).as_deref(), Some("选区: 9999999"));
        assert_eq!(sb.default_text(12).as_deref(), Some("大小: 99999999"));
        assert_eq!(sb.default_text(14), None);
    }

    /// the status-pane width routine: `extent + 4`, `extent + 36` for the icon panes,
    /// never below 45.
    #[test]
    fn pane_width_starts_at_45_and_gains_a_pixel_for_the_icon() {
        let d = WidthRules::DEFAULT;
        assert_eq!(pane_width(0, 0, d), 45);
        assert_eq!(pane_width(30, 0, d), 45);
        assert_eq!(pane_width(41, 1, d), 45);
        assert_eq!(pane_width(42, 1, d), 46);
        assert_eq!(pane_width(20, 6, d), 45);
        for id in [5, 7, 8, 9, 13] {
            assert_eq!(pane_width(10, id, d), 46, "pane {id} reserves icon space");
        }
        assert!(!StatusField::Ln.reserves_icon());
        assert!(StatusField::Eol.reserves_icon());
    }

    /// 边距和下限是皮肤能改的：`status.pad = 9` 的皮肤画文字时左缩 9 DIP，
    /// 量宽度就得按 18 留 —— 只改画不改量，文字会被 `ellipsis` 吃掉。
    #[test]
    fn width_rules_are_what_the_skin_says_they_are() {
        let wide = WidthRules {
            text_pad: 18,
            icon_pad: 40,
            min_width: 80,
        };
        assert_eq!(pane_width(10, 0, wide), 80, "min wins");
        assert_eq!(pane_width(100, 0, wide), 118);
        assert_eq!(pane_width(100, 7, wide), 140, "icon pad replaces text pad");
    }

    /// [`StatusField::Selection`] is empty when there is no selection and
    /// carries both numbers when there is; [`StatusField::TabSize`] swaps in
    /// "Soft Tab" from the soft-tab option.
    #[test]
    fn selection_and_soft_tab_shapes() {
        let mut m = full_model();
        m.selection = Some(Selection { chars: 7, lines: 2 });
        m.soft_tab = true;
        let mut sb = StatusBar::new();
        sb.set_model(&m);
        let panes = sb.panes();
        assert_eq!(panes[3].text, "7字符,2行");
        assert_eq!(panes[5].text, "软空格:4");
        m.selection = Some(Selection { chars: 0, lines: 0 });
        sb.set_model(&m);
        assert_eq!(sb.panes()[3].text, "");
    }

    #[test]
    fn edit_mode_and_eol_panes_use_the_language_table_words() {
        let mut m = full_model();
        m.edit_mode = Some(EditMode::Overwrite);
        m.eol = Some(Eol::Lf);
        let mut sb = StatusBar::new();
        sb.set_model(&m);
        assert_eq!(sb.panes()[11].text, "覆写");
        assert_eq!(sb.panes()[10].text, "LF");
        m.eol = Some(Eol::Cr);
        sb.set_model(&m);
        assert_eq!(sb.panes()[10].text, "CR");
        m.edit_mode = Some(EditMode::Column);
        sb.set_model(&m);
        assert_eq!(sb.panes()[11].text, "列模式");
        m.edit_mode = Some(EditMode::Multi);
        sb.set_model(&m);
        assert_eq!(sb.panes()[11].text, "多选");
    }

    /// 缩放归调用方：外壳按 DPI 把 `[metrics]` 折成像素再传进来，`pane_width`
    /// 只认传进来的三个数。这里钉住「同一份 rules 下 extent 是唯一的变量」，
    /// 以及 125% 那一档的三个数（4→5、36→45、45→56）。
    #[test]
    fn width_rules_carry_the_dpi_scaling() {
        let d = WidthRules::DEFAULT;
        assert_eq!(pane_width(0, 0, d), 45);
        assert_eq!(pane_width(10, 7, d), 46);
        assert_eq!(pane_width(52, 1, d), 56);
        let at120 = WidthRules {
            text_pad: 5,
            icon_pad: 45,
            min_width: 56,
        };
        assert_eq!(pane_width(0, 0, at120), 56);
        assert_eq!(pane_width(52, 7, at120), 97, "icon pane");
        assert_eq!(pane_width(52, 1, at120), 57, "text pane");
    }

    #[test]
    fn encoding_labels_use_the_code_page_words() {
        assert_eq!(encoding_label("windows-1252", false), "ANSI");
        assert_eq!(encoding_label("gbk", false), "CP 936");
        assert_eq!(encoding_label("windows-1251", false), "CP 1251");
        assert_eq!(encoding_label("Big5", false), "CP 950");
        assert_eq!(encoding_label("Shift_JIS", false), "CP 932");
        assert_eq!(encoding_label("EUC-KR", false), "CP 949");
        assert_eq!(encoding_label("GB18030", true), "GB18030");
        assert_eq!(encoding_label("UTF-8", true), "UTF-8+");
        assert_eq!(encoding_label("UTF-16LE", true), "UTF-16+");
        assert_eq!(encoding_label("UTF-16BE", true), "UTF-16 BE+");
    }
}
