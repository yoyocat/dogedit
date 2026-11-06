//! 指针提示：悬停在这一格时，指针该长什么样、提示什么。
//!
//! 和 [`crate::State`] 一样，这里只放**词表**：`cursor` 是一串名字，具体加载哪个
//! `IDC_*` 是 `dg-app` 的事（这个 crate 不认识 windows）。
//!
//! 为什么是**声明式的一张表**而不是一个回调：悬停是每秒几十次的事情，每换一格进
//! 一次 Lua 就为了拿两个常量，代价和收益完全不成比例；而且回调能写死循环，指针卡
//! 住比画错更难受。真要按格分支，绘制档的 `ctx` 已经能拿到 `part`/`text`/`value`，
//! 提示这边只需要 `@text` 这种占位符就够了。
//!
//! 所以：
//!
//! ```lua
//! s:hint("toolbar.button", { cursor = "hand", tip = "@label" })
//! s:hint("status.pane",    { tip = "@text" })
//! ```
//!
//! 不写 `cursor` 就不动指针（保持内置的形状），不写 `tip` 就不弹提示。

use std::fmt;

/// 指针形状。名字是对外契约，改了就是破坏兼容。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    /// 普通箭头。
    Arrow,
    /// 文本 I 形。
    Ibeam,
    /// 手形：可以点。
    Hand,
    /// 十字：框选。
    Cross,
    /// 上下双向箭头：拖高度。
    SizeNs,
    /// 左右双向箭头：拖宽度。
    SizeWe,
    /// 左上-右下双向箭头。
    SizeNwse,
    /// 右上-左下双向箭头。
    SizeNesw,
    /// 四向箭头：拖位置。
    SizeAll,
    /// 沙漏：忙。
    Wait,
    /// 箭头带沙漏：后台在忙但还能操作。
    AppStarting,
    /// 禁止：这里不能放。
    No,
}

impl Cursor {
    /// 全部形状，按 [`Cursor::NAMES`] 的顺序。
    pub const ALL: [Cursor; 12] = [
        Cursor::Arrow,
        Cursor::Ibeam,
        Cursor::Hand,
        Cursor::Cross,
        Cursor::SizeNs,
        Cursor::SizeWe,
        Cursor::SizeNwse,
        Cursor::SizeNesw,
        Cursor::SizeAll,
        Cursor::Wait,
        Cursor::AppStarting,
        Cursor::No,
    ];

    /// 名字表，顺序与 [`Cursor::ALL`] 一致。**小写下划线**，和区域名一个风格。
    pub const NAMES: [&'static str; 12] = [
        "arrow",
        "ibeam",
        "hand",
        "cross",
        "size_ns",
        "size_we",
        "size_nwse",
        "size_nesw",
        "size_all",
        "wait",
        "appstarting",
        "no",
    ];

    /// 词表里的名字。
    pub const fn as_str(self) -> &'static str {
        match self {
            Cursor::Arrow => "arrow",
            Cursor::Ibeam => "ibeam",
            Cursor::Hand => "hand",
            Cursor::Cross => "cross",
            Cursor::SizeNs => "size_ns",
            Cursor::SizeWe => "size_we",
            Cursor::SizeNwse => "size_nwse",
            Cursor::SizeNesw => "size_nesw",
            Cursor::SizeAll => "size_all",
            Cursor::Wait => "wait",
            Cursor::AppStarting => "appstarting",
            Cursor::No => "no",
        }
    }

    /// 按名字找一个。大小写不敏感、忽略首尾空白；认不出来就报错，**不退回
    /// `arrow`** —— 皮肤里写错一个名字应该当场看见，而不是「指针怎么不动」。
    pub fn parse(name: &str) -> Result<Cursor, CursorParseError> {
        let want = name.trim();
        for (shape, known) in Cursor::ALL.into_iter().zip(Cursor::NAMES) {
            if known.eq_ignore_ascii_case(want) {
                return Ok(shape);
            }
        }
        Err(CursorParseError {
            name: want.to_string(),
        })
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 词表里没有这个指针名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorParseError {
    /// 用户写的那个名字（已 trim）。
    pub name: String,
}

impl fmt::Display for CursorParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown cursor {:?}: want one of {}",
            self.name,
            Cursor::NAMES.join(", ")
        )
    }
}

impl std::error::Error for CursorParseError {}

/// 一条悬停声明。两个字段都可以不写。
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct HintSpec {
    /// 指针形状；`None` = 不动指针。
    pub cursor: Option<Cursor>,
    /// 提示文本模板；`None` = 不弹提示。
    pub tip: Option<String>,
}

impl HintSpec {
    /// 这条声明有没有内容。全是 `None` 等于没写。
    pub fn is_empty(&self) -> bool {
        self.cursor.is_none() && self.tip.is_none()
    }

    /// 按 `@text` / `@label` / `@value` 展开提示模板。
    ///
    /// 三个占位符就是绘制档 `c:text(r, "@text", …)` 认的那三个，语义也一样：
    /// 这一格自己的标题、说明、值。模板里没有占位符就原样返回（一次分配都不做
    /// 是不可能的，但 `find('@')` 一次就短路了）。
    ///
    /// 没有 `tip` 时答 `None`。
    pub fn resolve_tip(&self, text: &str, label: &str, value: &str) -> Option<String> {
        let template = self.tip.as_deref()?;
        if !template.contains('@') {
            return Some(template.to_string());
        }
        let mut out = String::with_capacity(template.len() + 16);
        let mut rest = template;
        while let Some(at) = rest.find('@') {
            out.push_str(&rest[..at]);
            let tail = &rest[at + 1..];
            // 长名字先试，否则 `@text` 会被 `@t` 这种前缀吃掉（这里没有 `@t`，
            // 但顺序写对才不用每次改词表都回来想）。
            if let Some(s) = tail.strip_prefix("label") {
                out.push_str(label);
                rest = s;
            } else if let Some(s) = tail.strip_prefix("value") {
                out.push_str(value);
                rest = s;
            } else if let Some(s) = tail.strip_prefix("text") {
                out.push_str(text);
                rest = s;
            } else {
                // 不认识的占位符原样留下：皮肤里打错字，提示里就看得见。
                out.push('@');
                rest = tail;
            }
        }
        out.push_str(rest);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字表钉死：`skin.lua` 与文档用的就是这十二个词。
    #[test]
    fn the_twelve_cursor_names_are_the_contract() {
        assert_eq!(
            Cursor::NAMES.to_vec(),
            vec![
                "arrow",
                "ibeam",
                "hand",
                "cross",
                "size_ns",
                "size_we",
                "size_nwse",
                "size_nesw",
                "size_all",
                "wait",
                "appstarting",
                "no",
            ]
        );
        assert_eq!(Cursor::ALL.len(), Cursor::NAMES.len());
    }

    /// 每个名字都能解析回自己，`as_str` 与 `NAMES` 同序。
    #[test]
    fn every_cursor_name_round_trips() {
        for (shape, name) in Cursor::ALL.into_iter().zip(Cursor::NAMES) {
            assert_eq!(Cursor::parse(name), Ok(shape));
            assert_eq!(shape.as_str(), name);
            assert_eq!(shape.to_string(), name);
        }
    }

    /// 大小写与空白都不计较，写错了报错并列出全部合法名字。
    #[test]
    fn parsing_ignores_case_and_rejects_a_typo() {
        assert_eq!(Cursor::parse("  HAND "), Ok(Cursor::Hand));
        assert_eq!(Cursor::parse("Size_NS"), Ok(Cursor::SizeNs));
        let err = Cursor::parse("pointer").expect_err("typo");
        assert_eq!(err.name, "pointer");
        assert!(err.to_string().contains("ibeam"), "{err}");
    }

    /// 没有占位符就原样返回。
    #[test]
    fn a_plain_tip_is_returned_as_written() {
        let spec = HintSpec {
            cursor: None,
            tip: Some("保存".into()),
        };
        assert_eq!(spec.resolve_tip("x", "y", "z").as_deref(), Some("保存"));
    }

    /// 三个占位符各展开成自己那一份，可以混在一句里。
    #[test]
    fn the_placeholders_expand_from_the_cell() {
        let spec = HintSpec {
            cursor: None,
            tip: Some("@label (@text) = @value".into()),
        };
        assert_eq!(
            spec.resolve_tip("UTF-8", "编码", "65001").as_deref(),
            Some("编码 (UTF-8) = 65001")
        );
    }

    /// 不认识的占位符原样留着 —— 皮肤打错字要看得见，而不是悄悄消失。
    #[test]
    fn an_unknown_placeholder_survives_verbatim() {
        let spec = HintSpec {
            cursor: None,
            tip: Some("@nope/@text".into()),
        };
        assert_eq!(spec.resolve_tip("T", "", "").as_deref(), Some("@nope/T"));
    }

    /// 没写 `tip` 就不弹；`is_empty` 认得出来。
    #[test]
    fn an_empty_spec_says_nothing() {
        let spec = HintSpec::default();
        assert!(spec.is_empty());
        assert_eq!(spec.resolve_tip("a", "b", "c"), None);
        let only_cursor = HintSpec {
            cursor: Some(Cursor::Hand),
            tip: None,
        };
        assert!(!only_cursor.is_empty());
        assert_eq!(only_cursor.resolve_tip("a", "b", "c"), None);
    }
}
