//! 字体表：`[fonts]` 里的具名字体，和配方里 `font = "font.ui"` 指向的那些 token。
//!
//! 六个 token，一个都不能多：`ui` / `mono` / `tab` / `status` / `menu` / `pane`。
//! 写错名字当场报错并列出六个 —— 和样式键、度量名同一个态度，「写了没反应」
//! 比「写了报错」难查得多。
//!
//! ## 没写的 token 用内置字体
//!
//! `Fonts::get` 回 `Option`，`None` 就是「用这个区域本来那只字体」（系统
//! `NONCLIENTMETRICS` 的 UI 字体，或者编辑器自己那套等宽字体）。所以空皮肤
//! 逐像素等于没这张表 —— 这条不变式每个模块都要守住。
//!
//! 也正因为内置字体是**系统给的**、不是一个固定的 `face`，这里没有「默认
//! `FontSpec`」这种东西：默认不是一份配置，是「不覆盖」。
//!
//! ## 夹住不报错
//!
//! 字号夹到 6–48 pt、字重夹到 100–900。和度量表同一个态度：`size = 200` 的
//! 意思是「大一点」，不是「我打错了」，没必要开不了窗。
//!
//! ## 只按系统已安装字面解析
//!
//! 打包字体不做（授权 + 加载器）。字面名写错时 GDI 会自己挑一个替代字体，
//! 这里不拦 —— 拦不住，也无从知道系统装了哪些。

use std::fmt;

use dg_render::theme::ThemeFile;

/// 一个具名字体。六个，顺序就是名字表顺序。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FontToken {
    /// 界面正文（`font.ui`）。
    Ui,
    /// 编辑器字面（`font.mono`）。
    Mono,
    /// 文档页签（`font.tab`）。
    Tab,
    /// 状态栏（`font.status`）。
    Status,
    /// 菜单（`font.menu`）。
    Menu,
    /// 停靠窗格（`font.pane`）。
    Pane,
}

/// 字体 token 总数，也是 [`Fonts`] 数组的长度。
pub const FONT_COUNT: usize = 6;

/// 已经有绘制器读的 token。
///
/// 列表之外的只解析、不生效，加载时补一条警告并按名字列出来 —— 和配方的
/// [`WIRED_REGIONS`](crate::recipe::WIRED_REGIONS) 同一个态度：「写了没反应」
/// 比「写了报错」难查得多。
///
/// 接线的代价不一样：`mono` 换字体要连带重算单元格、行高、缩进线和光标
/// （`remeasure_at`）；`tab` 要重算页签宽度（`build_fonts` 跑在
/// `Metrics::at` 之前，所以它是顺带的）；`ui` / `menu` / `status` / `pane` 最省
/// —— 它们共用 `uifont` 那条路（`create_for`），换掉就是换掉，高度自己会长：
/// 菜单那一行由字体量出来，状态栏在 `WM_SETFONT` 之后由 comctl32 按字体重新
/// 报自然高度（实测 9 pt → 29 px、14 pt → 41 px、20 pt → 57 px，@120 DPI）。
/// 想让状态栏不跟着长就写 `[metrics] status.height`。
///
/// `pane` 只覆盖**脚本窗格**（`s:pane`，见 [`crate::FontToken`] 的用处）：内置的
/// 停靠窗格画字用的还是编辑器字体（`outputwin` 拿的是框架传下来的那只），所以
/// 写 `pane` 不会让目录树变大。
pub const WIRED_FONTS: [FontToken; 6] = [
    FontToken::Mono,
    FontToken::Tab,
    FontToken::Ui,
    FontToken::Menu,
    FontToken::Status,
    FontToken::Pane,
];

impl FontToken {
    /// 全部 token，顺序即 [`FontToken::NAMES`] 的顺序。
    pub const ALL: [FontToken; FONT_COUNT] = [
        FontToken::Ui,
        FontToken::Mono,
        FontToken::Tab,
        FontToken::Status,
        FontToken::Menu,
        FontToken::Pane,
    ];

    /// 主题文件 `[Fonts]` 段里的 token 名。
    pub const NAMES: [&'static str; FONT_COUNT] =
        ["ui", "mono", "tab", "status", "menu", "pane"];

    /// `[fonts]` 里的键名（`ui`）。
    pub const fn name(self) -> &'static str {
        match self {
            FontToken::Ui => "ui",
            FontToken::Mono => "mono",
            FontToken::Tab => "tab",
            FontToken::Status => "status",
            FontToken::Menu => "menu",
            FontToken::Pane => "pane",
        }
    }

    /// 配方里写的那个 token 名（`font.ui`）。Lua 的 `c:text(..., "font.ui", ...)`
    /// 也是这一个词。
    pub const fn token(self) -> &'static str {
        match self {
            FontToken::Ui => "font.ui",
            FontToken::Mono => "font.mono",
            FontToken::Tab => "font.tab",
            FontToken::Status => "font.status",
            FontToken::Menu => "font.menu",
            FontToken::Pane => "font.pane",
        }
    }

    /// 数组下标。
    pub const fn index(self) -> usize {
        match self {
            FontToken::Ui => 0,
            FontToken::Mono => 1,
            FontToken::Tab => 2,
            FontToken::Status => 3,
            FontToken::Menu => 4,
            FontToken::Pane => 5,
        }
    }

    /// 认一个名字。`[fonts]` 的键名（`ui`）和配方里的 token 名（`font.ui`）
    /// 都认，大小写不敏感 —— 同一个东西在两张表里写法不同，别让人记两遍。
    pub fn parse(name: &str) -> Option<FontToken> {
        let want = name.trim();
        let bare = want
            .strip_prefix("font.")
            .or_else(|| want.strip_prefix("Font."))
            .unwrap_or(want);
        FontToken::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(bare))
    }
}

impl fmt::Display for FontToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// 一份字体配置。字段全部是**已经夹好**的值。
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// 字面名（`"Segoe UI"`）。空串 = 系统默认字面。
    pub face: String,
    /// 字号，**pt**。夹到 6–48。
    pub size: f32,
    /// 字重，100–900。夹到区间。
    pub weight: u16,
    /// 斜体。
    pub italic: bool,
}

impl Default for FontSpec {
    fn default() -> Self {
        FontSpec {
            face: String::new(),
            size: 9.0,
            weight: 400,
            italic: false,
        }
    }
}

impl FontSpec {
    /// 把越界的字段夹回区间。构造之后调一次，之后随便读。
    pub fn clamp(&mut self) {
        if !self.size.is_finite() {
            self.size = 9.0;
        }
        self.size = self.size.clamp(6.0, 48.0);
        self.weight = self.weight.clamp(100, 900);
        self.face = self.face.trim().to_string();
    }

    /// 人话形式，给主题体检用：`Segoe UI 9pt`。
    pub fn describe(&self) -> String {
        let face = if self.face.is_empty() {
            "（系统默认）"
        } else {
            &self.face
        };
        let mut out = format!("{face} {}pt", trim_pt(self.size));
        if self.weight >= 600 {
            out.push_str(" bold");
        }
        if self.italic {
            out.push_str(" italic");
        }
        out
    }
}

/// `9.0` 写成 `9`，`10.5` 照旧。字号里那一位小数只在真写了小数时才有意义。
fn trim_pt(size: f32) -> String {
    if (size - size.round()).abs() < f32::EPSILON {
        format!("{}", size.round() as i32)
    } else {
        format!("{size}")
    }
}

/// 字体表：六个槽，没写的留 `None`。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fonts {
    slots: [Option<FontSpec>; FONT_COUNT],
}

impl Fonts {
    /// 空表：六个 token 全走内置字体。
    ///
    /// `const` 是为了 `static FONTS: RwLock<Fonts> = RwLock::new(Fonts::new())`
    /// —— `Default::default()` 不是 `const`。
    pub const fn new() -> Fonts {
        Fonts {
            // `[None; N]` 要求 `Option<FontSpec>: Copy`，而 `FontSpec` 里有 `String`。
            // 内联 const 让每一项各自求值一次，不用给 `FontSpec` 加个假的 `Copy`。
            slots: [const { None }; FONT_COUNT],
        }
    }

    /// 一个 token 的配置；没写过就是 `None`（= 用内置字体）。
    pub fn get(&self, token: FontToken) -> Option<&FontSpec> {
        self.slots[token.index()].as_ref()
    }

    /// 按名字取。名字不认识回 `None` —— 调用方要区分「不认识」和「没写」时
    /// 先用 [`FontToken::parse`]。
    pub fn get_by_name(&self, name: &str) -> Option<&FontSpec> {
        FontToken::parse(name).and_then(|t| self.get(t))
    }

    /// 写一个 token。值会被 [`FontSpec::clamp`] 夹一遍。
    pub fn set(&mut self, token: FontToken, mut spec: FontSpec) {
        spec.clamp();
        self.slots[token.index()] = Some(spec);
    }

    /// 一条都没写？
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// 写了几条。
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// 写过的条目，按 [`FontToken::ALL`] 的顺序。
    pub fn iter(&self) -> impl Iterator<Item = (FontToken, &FontSpec)> {
        FontToken::ALL
            .into_iter()
            .filter_map(|t| self.get(t).map(|s| (t, s)))
    }

    /// 从主题文件的 `[Fonts]` 段建字体表。
    ///
    /// 键的写法是 `token.字段`：`mono.face=Consolas`、`mono.size=11`。和把一组
    /// 字段写成 `mono = { face = "Consolas", size = 11 }` 是同一件事，只是压成
    /// 一行 —— 好处是一个 token 的字段可以分散在**两个**文件里
    /// （包给的 `theme.ini` 写字面，用户的 `themes/<name>.ini` 调字号），
    /// 而 `merge` 是按行合并的，压平之后天然就支持了。
    ///
    /// 回 `(字体表, 问题清单)`，不在这里失败 —— 理由同
    /// [`crate::Recipe::from_theme_file`]：主题文件是共用的，一条写错不该让整套
    /// 外观消失。
    pub fn from_theme_file(file: &ThemeFile) -> (Fonts, Vec<String>) {
        // 一个 token 的字段可能散在若干行，所以先攒齐再 `set`：`Fonts::set` 会
        // 把整份 spec 夹一遍，逐字段设的话最后那一次会覆盖掉前面夹好的结果。
        let mut specs: Vec<(FontToken, FontSpec)> = Vec::new();
        let mut problems = Vec::new();
        for (key, value) in &file.fonts {
            let Some((name, field)) = key.rsplit_once('.') else {
                problems.push(format!("[Fonts] {key}：要写成「token.字段」，比如 mono.face"));
                continue;
            };
            let Some(token) = FontToken::parse(name) else {
                problems.push(format!(
                    "[Fonts] {key}：{name:?} 不是字体 token；能写的有 {}",
                    FontToken::ALL
                        .iter()
                        .map(|t| t.name())
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                continue;
            };
            let value = value.trim();
            // 先把值收成一条**已校验**的字段，再动 slot。反过来的话，
            // `mono.size=huge` 会留下一个全是默认值的 `mono`，把内置字体
            // 悄悄换成 9pt —— 写错一个字段反而让整个 token 生效，比不生效更难查。
            let field = match field {
                "face" => Field::Face(value.to_string()),
                "size" => match value.parse::<f32>() {
                    Ok(v) if v.is_finite() => Field::Size(v),
                    _ => {
                        problems.push(format!(
                            "[Fonts] {key}：字号要写数字（pt），你写的是 {value:?}"
                        ));
                        continue;
                    }
                },
                "weight" => match parse_weight(value) {
                    Some(w) => Field::Weight(w),
                    None => {
                        problems.push(format!(
                            "[Fonts] {key}：字重要写 100–900 或 normal / bold，你写的是 {value:?}"
                        ));
                        continue;
                    }
                },
                "italic" => match parse_switch(value) {
                    Some(b) => Field::Italic(b),
                    None => {
                        problems.push(format!(
                            "[Fonts] {key}：要写 true / false，你写的是 {value:?}"
                        ));
                        continue;
                    }
                },
                other => {
                    problems.push(format!(
                        "[Fonts] {key}：不认识的字段 {other:?}（认识的有 face / size / weight / italic）"
                    ));
                    continue;
                }
            };
            let slot = match specs.iter().position(|(t, _)| *t == token) {
                Some(i) => &mut specs[i].1,
                None => {
                    specs.push((token, FontSpec::default()));
                    let last = specs.len() - 1;
                    &mut specs[last].1
                }
            };
            match field {
                Field::Face(face) => slot.face = face,
                Field::Size(size) => slot.size = size,
                Field::Weight(weight) => slot.weight = weight,
                Field::Italic(italic) => slot.italic = italic,
            }
        }
        let mut fonts = Fonts::default();
        for (token, spec) in specs {
            fonts.set(token, spec);
        }
        (fonts, problems)
    }
}

/// 一个已经校验过的 `[Fonts]` 字段。
///
/// 单独一个枚举是为了让「校验」和「写进 slot」分成两步 —— 见
/// [`Fonts::from_theme_file`] 里那句注释：顺序反了会让写错的字段反而生效。
enum Field {
    Face(String),
    Size(f32),
    Weight(u16),
    Italic(bool),
}

/// 字重：数字，或者 `normal` / `regular` / `bold`。
///
/// 越界的数字**夹住**而不是拒绝 —— 和度量表一个态度，`weight = 1000` 的意图
/// 很明确，没必要让人回去改。
fn parse_weight(text: &str) -> Option<u16> {
    if let Ok(n) = text.parse::<i64>() {
        return Some(n.clamp(100, 900) as u16);
    }
    match text.to_ascii_lowercase().as_str() {
        "normal" | "regular" => Some(400),
        "bold" => Some(700),
        _ => None,
    }
}

/// `true` / `false` / `yes` / `no` / `1` / `0`。ini 没有布尔类型，只能自己认。
fn parse_switch(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// 字体表里的一个字段不认识。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFieldError {
    /// `[fonts]` 下的那个键名。
    pub token: String,
    /// 那个字段名。
    pub field: String,
}

impl fmt::Display for FontFieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[fonts.{}] 里不认识的字段 {:?}（认识的有 face / size / weight / italic）",
            self.token, self.field
        )
    }
}

impl std::error::Error for FontFieldError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 六个名字是契约：主题文件的键、配方里的 token、Lua 的字符串都用它。
    #[test]
    fn the_six_tokens_are_the_contract() {
        assert_eq!(
            FontToken::ALL.map(FontToken::name).to_vec(),
            vec!["ui", "mono", "tab", "status", "menu", "pane"]
        );
        assert_eq!(
            FontToken::ALL.map(FontToken::token).to_vec(),
            vec![
                "font.ui",
                "font.mono",
                "font.tab",
                "font.status",
                "font.menu",
                "font.pane"
            ]
        );
        // 下标必须一一对应，否则 `slots[token.index()]` 会串格。
        for (i, t) in FontToken::ALL.into_iter().enumerate() {
            assert_eq!(t.index(), i);
        }
    }

    /// 键名和 token 名都认，大小写不敏感 —— 同一个东西两张表里写法不同。
    #[test]
    fn both_spellings_parse() {
        for t in FontToken::ALL {
            assert_eq!(FontToken::parse(t.name()), Some(t));
            assert_eq!(FontToken::parse(t.token()), Some(t));
            assert_eq!(FontToken::parse(&t.name().to_uppercase()), Some(t));
            assert_eq!(FontToken::parse(&format!(" font.{} ", t.name())), Some(t));
        }
        assert_eq!(FontToken::parse("ui2"), None);
        assert_eq!(FontToken::parse(""), None);
        assert_eq!(FontToken::parse("font."), None);
    }

    /// 空表 = 全走内置字体。这是「空皮肤 0 像素」在字体这一层的落点。
    #[test]
    fn an_empty_table_means_the_built_in_fonts() {
        let fonts = Fonts::new();
        assert!(fonts.is_empty());
        assert_eq!(fonts.len(), 0);
        for t in FontToken::ALL {
            assert!(fonts.get(t).is_none(), "{t}");
        }
        assert_eq!(fonts.iter().count(), 0);
    }

    /// 写一条只动那一格，别的照旧空着。
    #[test]
    fn writing_one_token_leaves_the_others_alone() {
        let mut fonts = Fonts::new();
        fonts.set(
            FontToken::Mono,
            FontSpec {
                face: " JetBrains Mono ".into(),
                size: 11.0,
                ..FontSpec::default()
            },
        );
        assert_eq!(fonts.len(), 1);
        let mono = fonts.get(FontToken::Mono).expect("written");
        assert_eq!(mono.face, "JetBrains Mono", "两头空白被剪掉");
        assert_eq!(mono.size, 11.0);
        assert!(fonts.get(FontToken::Ui).is_none());
        assert_eq!(fonts.get_by_name("font.mono"), Some(mono));
    }

    /// 越界**夹住不报错**：`size = 200` 的意思是「大一点」。
    #[test]
    fn out_of_range_values_are_clamped() {
        let mut fonts = Fonts::new();
        fonts.set(
            FontToken::Ui,
            FontSpec {
                face: "Segoe UI".into(),
                size: 200.0,
                weight: 5000,
                italic: true,
            },
        );
        let ui = fonts.get(FontToken::Ui).expect("written");
        assert_eq!(ui.size, 48.0);
        assert_eq!(ui.weight, 900);

        fonts.set(
            FontToken::Ui,
            FontSpec {
                size: 0.5,
                weight: 10,
                ..FontSpec::default()
            },
        );
        let ui = fonts.get(FontToken::Ui).expect("written");
        assert_eq!(ui.size, 6.0);
        assert_eq!(ui.weight, 100);
    }

    /// NaN 不许漏进来（`size = nan` 在 toml 里写不出来，但 Lua 能算出来）。
    #[test]
    fn a_nan_size_falls_back_to_the_default() {
        let mut spec = FontSpec {
            size: f32::NAN,
            ..FontSpec::default()
        };
        spec.clamp();
        assert_eq!(spec.size, 9.0);
    }

    /// 诊断输出那一行：整数不带小数点，真写了小数才带。
    #[test]
    fn the_description_reads_like_a_person_wrote_it() {
        let spec = FontSpec {
            face: "Segoe UI".into(),
            size: 9.0,
            ..FontSpec::default()
        };
        assert_eq!(spec.describe(), "Segoe UI 9pt");
        let bold = FontSpec {
            face: String::new(),
            size: 10.5,
            weight: 700,
            italic: true,
        };
        assert_eq!(bold.describe(), "（系统默认） 10.5pt bold italic");
    }

    /// 接线表只列已经生效的几个，且必须是 `ALL` 里的真名字 —— 打错一个字
    /// 就等于那个 token 静默失效（警告也会跟着漏报）。
    #[test]
    fn the_wired_list_names_real_tokens() {
        for token in WIRED_FONTS {
            assert!(FontToken::ALL.contains(&token), "{token}");
        }
        assert_eq!(
            WIRED_FONTS.map(FontToken::name).to_vec(),
            vec!["mono", "tab", "ui", "menu", "status", "pane"]
        );
    }

    // ── `[Fonts]`（主题文件）──────────────────────────────────────────────

    /// 四个字段各落一格 —— 少一个就是「写了没反应」，而那正是最难查的一种。
    #[test]
    fn all_four_font_fields_land() {
        let file = dg_render::parse_theme_ini(
            "[Fonts]
mono.face=Consolas
mono.size=11
mono.weight=700
mono.italic=true
",
        );
        let (fonts, problems) = Fonts::from_theme_file(&file);
        assert!(problems.is_empty(), "{problems:?}");
        let spec = fonts.get(FontToken::Mono).expect("mono");
        assert_eq!(spec.face, "Consolas");
        assert_eq!(spec.size, 11.0);
        assert_eq!(spec.weight, 700);
        assert!(spec.italic);
    }

    /// 一个 token 的字段可以散在两个文件里 —— `merge` 是按行合并的，压平之后
    /// 天然支持「包写字面、用户调字号」。
    #[test]
    fn fields_of_one_token_can_come_from_two_files() {
        let mut base = dg_render::parse_theme_ini("[Fonts]\nmono.face=Consolas\n");
        base.merge(&dg_render::parse_theme_ini("[Fonts]\nmono.size=13\n"));
        let (fonts, problems) = Fonts::from_theme_file(&base);
        assert!(problems.is_empty(), "{problems:?}");
        let spec = fonts.get(FontToken::Mono).expect("mono");
        assert_eq!(spec.face, "Consolas");
        assert_eq!(spec.size, 13.0);
    }

    /// 字重和斜体认人话，越界的数字夹住 —— 和度量表一个态度。
    #[test]
    fn weight_and_italic_accept_the_words_people_write() {
        let file = dg_render::parse_theme_ini(
            "[Fonts]
ui.weight=bold
menu.weight=normal
status.italic=yes
pane.italic=0
tab.weight=5000
",
        );
        let (fonts, problems) = Fonts::from_theme_file(&file);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(fonts.get(FontToken::Ui).unwrap().weight, 700);
        assert_eq!(fonts.get(FontToken::Menu).unwrap().weight, 400);
        assert!(fonts.get(FontToken::Status).unwrap().italic);
        assert!(!fonts.get(FontToken::Pane).unwrap().italic);
        assert_eq!(fonts.get(FontToken::Tab).unwrap().weight, 900, "夹到 900");
    }

    /// 一条写错不牵连其他条。
    #[test]
    fn a_bad_entry_is_reported_and_the_rest_still_applies() {
        let file = dg_render::parse_theme_ini(
            "[Fonts]
mono.size=11
mono.face=Consolas
mono.sizee=12
nosuch.face=X
mono.italic=maybe
ui.size=huge
",
        );
        let (fonts, problems) = Fonts::from_theme_file(&file);
        let mono = fonts.get(FontToken::Mono).expect("mono 还是建起来了");
        assert_eq!(mono.size, 11.0);
        assert_eq!(mono.face, "Consolas");
        assert!(fonts.get(FontToken::Ui).is_none(), "字号写错就不该建这一条");
        assert_eq!(problems.len(), 4, "{problems:?}");
        let text = problems.join("\n");
        assert!(text.contains("mono.sizee") && text.contains("不认识的字段"), "{text}");
        assert!(text.contains("nosuch") && text.contains("不是字体 token"), "{text}");
        assert!(text.contains("mono.italic") && text.contains("true / false"), "{text}");
        assert!(text.contains("ui.size") && text.contains("数字"), "{text}");
    }

    /// 没有 `[Fonts]` 段时是空表，调用方读到的就是「全内置字体」。
    #[test]
    fn a_theme_without_fonts_says_nothing() {
        let (fonts, problems) =
            Fonts::from_theme_file(&dg_render::parse_theme_ini("[Theme]\nCaret=#FF0000\n"));
        assert!(problems.is_empty());
        assert!(fonts.is_empty());
    }
}
