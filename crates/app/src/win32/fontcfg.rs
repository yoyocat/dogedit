//! `设置字体` 的配置模型：十个字体槽。
//!
//! 磁盘上是 `[Font0]` … `[Font9]` 十段：
//!
//! ```ini
//! [Font0]
//! Font=Consolas,10,0,1,0
//! UseCJK=N
//! CJKFont=Microsoft YaHei,10,0,1,0
//! Base0=0
//! Base1=0
//! [Font]
//! CurrentFont=0
//! ```
//!
//! `Font=` 是 `%s,%d,%d,%d,%d`：字面、字号(pt)、加粗、字体平滑字节、斜体字节。
//! 后两格存的是用户选的平滑方式与斜体标志（出厂 `1,0`），这里原样读写，
//! 不重新解释它们的取值 —— DirectWrite 那条绘制路自己决定抗锯齿，
//! 这一格只保留用户的选择，不悄悄改掉它。
//!
//! 读写一律经 [`crate::settings`]：这个文件不碰 `setting.ini` 的字节。键名解释
//! 放在这里，机械读写留在 `settings.rs`，〔设置字体〕对话框只跟本模块的结构体
//! 打交道。
//!
//! **没有配置时什么都不覆盖**：`Slot.font` 是 `None`，`gdi::EditorFace::current`
//! 仍旧按主题 `[Fonts] mono` / 内置 `Consolas` 走。这样出厂帧和这一版之前逐
//! 像素相同，手写 ini 的用户也不会因为多了个对话框而被换字体。
//!
//! **优先级**：主题 `[Fonts] mono` > 这里的 `[FontN] Font=` > 内置 `Consolas`。
//! 主题是作者给整套外观定的字体，比用户在对话框里挑的那只更具体（和
//! `uifont::create_for` 整条跳过 `[Setting] UIFont` 一个道理）。主题里一旦写了
//! `mono.*`，编辑视图这六个槽就不生效；输出 / 命令 / 打印 / HEX 槽没有对应的
//! 主题 token，不受影响。

use std::sync::{OnceLock, RwLock};

use super::gdi::{EditorFace, EDITOR_FACE, EDITOR_POINTS};

/// 十个槽，对应命令 1430 … 1439：默认字体、自定义 1–5、输出窗口、命令窗口、
/// 打印字体、HEX 字体。
pub const SLOT_IDS: [u32; 10] = [
    1430, 1431, 1432, 1433, 1434, 1435, 1436, 1437, 1438, 1439,
];
/// 只有前六个能「应用到当前编辑视图」。
pub const VIEW_SLOTS: usize = 6;
/// 打印字体槽。
pub const PRINT_SLOT: usize = 8;

/// `[FontN] Font=` / `CJKFont=` 的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSpec {
    pub face: String,
    /// pt，写成整数。
    pub points: i32,
    pub bold: bool,
    /// `Font=` 的第 4 格（出厂 1）。
    pub smooth: u8,
    /// `Font=` 的第 5 格（出厂 0）。
    pub italic: bool,
}

impl FontSpec {
    /// 出厂那一行：内置字面、内置字号、`0,1,0`。
    pub fn builtin() -> Self {
        Self {
            face: EDITOR_FACE.to_string(),
            points: EDITOR_POINTS as i32,
            bold: false,
            smooth: 1,
            italic: false,
        }
    }

    /// `%[^,],%d,%d,%d,%d` 的对面。字段数不对、字面为空、字号越界都算坏行 ——
    /// 与 `swscanf(...) == 5` 的判定一致，坏行当作「没配过」而不是半个字体。
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(5, ',');
        let face = parts.next()?.trim();
        if face.is_empty() {
            return None;
        }
        let points: i32 = parts.next()?.trim().parse().ok()?;
        if !(1..=200).contains(&points) {
            return None;
        }
        let bold = int_bool(parts.next()?)?;
        let smooth: u8 = parts.next()?.trim().parse().ok()?;
        let italic = int_bool(parts.next()?)?;
        Some(Self {
            face: face.to_string(),
            points,
            bold,
            smooth,
            italic,
        })
    }

    /// `%s,%d,%d,%d,%d`。
    pub fn format(&self) -> String {
        format!(
            "{},{},{},{},{}",
            self.face.trim(),
            self.points,
            u8::from(self.bold),
            self.smooth,
            u8::from(self.italic)
        )
    }

    /// 变成渲染器那只描述。字号取传进来的数（文档的缩放），字面/字重/斜体
    /// 取这一行配置。`EditorFace::current` 只在主题没声明 `[Fonts] mono` 时
    /// 才走到这里 —— 两层谁优先是那边的事，这一层不参与判断。
    pub fn into_face(&self, points: f32) -> EditorFace {
        EditorFace {
            name: self.face.clone(),
            points,
            weight: if self.bold { 700 } else { 400 },
            italic: self.italic,
        }
    }
}

fn int_bool(s: &str) -> Option<bool> {
    Some(s.trim() != "0")
}

fn ini_int(text: &str, section: &str, key: &str, default: i32) -> i32 {
    crate::settings::section_in(text, section, key)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn ini_flag(text: &str, section: &str, key: &str) -> bool {
    crate::settings::section_in(text, section, key)
        .is_some_and(|v| matches!(v.trim(), "1" | "Y" | "y" | "yes" | "true"))
}

/// 一个字体槽。`font` / `cjk` 为 `None` 表示 ini 里没写这一行，对话框仍会用
/// 出厂值展示，但保存时只写用户真正改过的槽。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub font: Option<FontSpec>,
    pub use_cjk: bool,
    pub cjk: Option<FontSpec>,
    pub base0: i32,
    pub base1: i32,
    /// 对话框里的改动标记，不落盘。
    pub dirty: bool,
}

impl Slot {
    fn empty() -> Self {
        Self {
            font: None,
            use_cjk: false,
            cjk: None,
            base0: 0,
            base1: 0,
            dirty: false,
        }
    }

    /// 展示用：没配过就拿出厂那一行。
    pub fn font_or_default(&self) -> FontSpec {
        self.font.clone().unwrap_or_else(FontSpec::builtin)
    }

    pub fn cjk_or_default(&self) -> FontSpec {
        self.cjk.clone().unwrap_or_else(FontSpec::builtin)
    }
}

/// 十槽 + 当前编辑视图用的是哪一槽。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub slots: Vec<Slot>,
    /// `[Font] CurrentFont`，只可能是 0..5。
    pub active: usize,
}

impl Config {
    /// 一次读完 `setting.ini`，再用纯文本查询逐个段取值 —— 十个段 × 五个键
    /// 不该把同一个文件打开五十遍。没有 ini 时是一份全空的出厂配置。
    pub fn load() -> Self {
        let text = crate::settings::path()
            .and_then(|p| crate::settings::text(&p))
            .unwrap_or_default();
        let mut slots = Vec::with_capacity(SLOT_IDS.len());
        for i in 0..SLOT_IDS.len() {
            let section = format!("Font{i}");
            let font = crate::settings::section_in(&text, &section, "Font")
                .and_then(|v| FontSpec::parse(&v));
            let cjk = crate::settings::section_in(&text, &section, "CJKFont")
                .and_then(|v| FontSpec::parse(&v));
            let mut slot = Slot::empty();
            slot.font = font;
            slot.cjk = cjk;
            slot.use_cjk = ini_flag(&text, &section, "UseCJK");
            slot.base0 = ini_int(&text, &section, "Base0", 0);
            slot.base1 = ini_int(&text, &section, "Base1", 0);
            slots.push(slot);
        }
        let active = crate::settings::section_in(&text, "Font", "CurrentFont")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0)
            .min(VIEW_SLOTS - 1);
        Self { slots, active }
    }

    /// 只写改过的槽：逐槽看 dirty 位，没碰过的段不凭空写进
    /// 用户的 ini。`--safe-mode` 一个字节都不落盘。
    pub fn save(&self) {
        if crate::safety::on() {
            return;
        }
        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.dirty {
                continue;
            }
            let section = format!("Font{i}");
            if let Some(font) = &slot.font {
                crate::settings::write_section(&section, "Font", &font.format());
            }
            crate::settings::write_section(
                &section,
                "UseCJK",
                if slot.use_cjk { "Y" } else { "N" },
            );
            if slot.use_cjk && let Some(cjk) = &slot.cjk {
                crate::settings::write_section(&section, "CJKFont", &cjk.format());
            }
            crate::settings::write_section(&section, "Base0", &slot.base0.to_string());
            crate::settings::write_section(&section, "Base1", &slot.base1.to_string());
        }
        crate::settings::write_section("Font", "CurrentFont", &self.active.to_string());
    }

    pub fn view_spec(&self) -> Option<FontSpec> {
        self.slots.get(self.active).and_then(|s| s.font.clone())
    }

    pub fn print_spec(&self) -> Option<FontSpec> {
        self.slots.get(PRINT_SLOT).and_then(|s| s.font.clone())
    }

    /// 「对中日韩字符使用单独的字体」勾上时的第二只字体：当前槽 `UseCJK=Y`
    /// 且 `CJKFont=` 有行才生效 —— 勾了但没写字面行视为没配，回落到单字体。
    pub fn view_cjk(&self) -> Option<FontSpec> {
        let slot = self.slots.get(self.active)?;
        if !slot.use_cjk {
            return None;
        }
        slot.cjk.clone()
    }
}

static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();

fn config() -> &'static RwLock<Config> {
    CONFIG.get_or_init(|| RwLock::new(Config::load()))
}

fn read<T>(f: impl FnOnce(&Config) -> T) -> T {
    let guard = config().read().unwrap_or_else(|e| e.into_inner());
    f(&guard)
}

/// 重新读盘。〔应用〕写完之后调，窗口那边的 `refresh_metrics_and_fonts` 才量到
/// 新字面。
pub fn reload() {
    *config().write().unwrap_or_else(|e| e.into_inner()) = Config::load();
}

/// 直接换掉内存里的配置：安全模式、以及 ini 不存在时用 —— 那些情况下 `reload`
/// 会把刚改的东西读丢。
pub fn replace(next: Config) {
    *config().write().unwrap_or_else(|e| e.into_inner()) = next;
}

/// 落盘 + 让进程内立刻看到。返回值只是给对话框一个「写没写」的回执。
pub fn commit(cfg: &Config) {
    let had_file = crate::settings::path().is_some();
    cfg.save();
    if had_file && !crate::safety::on() {
        reload();
    } else {
        replace(cfg.clone());
    }
}

/// 当前视图那只字体；没配过是 `None`，`gdi` 就继续走主题 / 内置值。
/// 这是**低优先级**的一层：主题声明了 `[Fonts] mono` 时它不会被采纳。
pub fn view_spec() -> Option<FontSpec> {
    read(|c| c.view_spec())
}

/// 当前视图的「对中日韩字符使用单独的字体」第二只；`Config::view_cjk` 的
/// 进程级入口。没勾、没写行都返回 `None`。
pub fn view_cjk() -> Option<FontSpec> {
    read(|c| c.view_cjk())
}

/// 打印字体槽；`printing.rs` 的「不镜像编辑器字体」分支用它。主题没有打印
/// 字体 token，这个槽不受主题优先级影响。
pub fn print_spec() -> Option<FontSpec> {
    read(|c| c.print_spec())
}

/// 编辑器的基准字号 —— 只在主题没声明 `[Fonts] mono` 时生效，与
/// [`view_spec`] 同一层。
pub fn base_points() -> Option<f32> {
    view_spec().map(|s| s.points as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_round_trips_all_five_fields() {
        let spec = FontSpec {
            face: "Microsoft YaHei".to_string(),
            points: 12,
            bold: true,
            smooth: 1,
            italic: false,
        };
        let line = spec.format();
        assert_eq!(line, "Microsoft YaHei,12,1,1,0");
        assert_eq!(FontSpec::parse(&line), Some(spec));
    }

    #[test]
    fn factory_spec_is_the_builtin_default() {
        assert_eq!(FontSpec::builtin().format(), "Consolas,10,0,1,0");
        assert_eq!(FontSpec::parse("Courier New,10,0,1,0").unwrap().face, "Courier New");
    }

    #[test]
    fn a_broken_spec_is_not_a_font() {
        assert!(FontSpec::parse("").is_none());
        assert!(FontSpec::parse("Consolas,10,0,1").is_none());
        assert!(FontSpec::parse(",10,0,1,0").is_none());
        assert!(FontSpec::parse("Consolas,x,0,1,0").is_none());
        assert!(FontSpec::parse("Consolas,0,0,1,0").is_none());
    }

    #[test]
    fn a_bold_spec_becomes_weight_700() {
        let mut spec = FontSpec::builtin();
        spec.bold = true;
        spec.italic = true;
        let face = spec.into_face(11.0);
        assert_eq!(face.weight, 700);
        assert!(face.italic);
        assert_eq!(face.points, 11.0);
        assert_eq!(face.name, EDITOR_FACE);
    }

    #[test]
    fn a_missing_ini_leaves_every_slot_unconfigured() {
        let text = "[Edit]\r\nAutoPair=Y\r\n";
        // section_in 的口径：段不存在就是没配过。
        assert!(crate::settings::section_in(text, "Font0", "Font").is_none());
        let c = Config {
            slots: (0..10).map(|_| Slot::empty()).collect(),
            active: 0,
        };
        assert!(c.view_spec().is_none());
        assert_eq!(c.slots[0].font_or_default(), FontSpec::builtin());
    }

    #[test]
    fn the_active_slot_feeds_the_view_and_slot_eight_feeds_printing() {
        let mut c = Config {
            slots: (0..SLOT_IDS.len()).map(|_| Slot::empty()).collect(),
            active: 3,
        };
        c.slots[3].font = Some(FontSpec {
            face: "Georgia".to_string(),
            points: 13,
            bold: true,
            smooth: 1,
            italic: false,
        });
        c.slots[PRINT_SLOT].font = Some(FontSpec {
            face: "Courier New".to_string(),
            points: 9,
            bold: false,
            smooth: 1,
            italic: false,
        });
        assert_eq!(c.view_spec().unwrap().face, "Georgia");
        assert_eq!(c.view_spec().unwrap().points, 13);
        assert_eq!(c.print_spec().unwrap().face, "Courier New");
        assert!(c.slots[0].font.is_none(), "未配过的槽不能凭空冒出一行");
    }

    #[test]
    fn the_cjk_second_font_needs_both_the_flag_and_the_line() {
        let cjk = FontSpec {
            face: "Microsoft YaHei".to_string(),
            points: 12,
            bold: false,
            smooth: 1,
            italic: false,
        };
        let mut c = Config {
            slots: (0..SLOT_IDS.len()).map(|_| Slot::empty()).collect(),
            active: 1,
        };
        // 没勾：写了 CJKFont= 也不生效。
        c.slots[1].cjk = Some(cjk.clone());
        assert!(c.view_cjk().is_none());
        // 勾了但没写字面行：同样不生效（ini 里 UseCJK=Y 可以没有 CJKFont=）。
        c.slots[1].use_cjk = true;
        c.slots[1].cjk = None;
        assert!(c.view_cjk().is_none());
        // 勾了且写了：第二只字体整行生效。
        c.slots[1].cjk = Some(cjk);
        let got = c.view_cjk().unwrap();
        assert_eq!(got.face, "Microsoft YaHei");
        assert_eq!(got.points, 12);
        assert!(!got.bold);
        // 其它槽不受影响。
        c.active = 2;
        assert!(c.view_cjk().is_none());
    }

    #[test]
    fn ini_keys_are_read_by_their_written_names() {
        let text = "[Font0]\r\nFont=Consolas,12,1,1,0\r\nUseCJK=Y\r\n"
            .to_owned()
            + "CJKFont=Microsoft YaHei,12,0,1,0\r\nBase0=2\r\nBase1=-3\r\n"
            + "[Font]\r\nCurrentFont=3\r\n";
        assert_eq!(
            crate::settings::section_in(&text, "Font0", "Font").as_deref(),
            Some("Consolas,12,1,1,0")
        );
        assert_eq!(ini_int(&text, "Font0", "Base0", 0), 2);
        assert_eq!(ini_int(&text, "Font0", "Base1", 0), -3);
        assert!(ini_flag(&text, "Font0", "UseCJK"));
        assert!(!ini_flag(&text, "Font0", "Base0"));
        let spec = FontSpec::parse(
            &crate::settings::section_in(&text, "Font0", "Font").unwrap(),
        )
        .unwrap();
        assert_eq!(spec.points, 12);
        assert!(spec.bold);
    }
}
