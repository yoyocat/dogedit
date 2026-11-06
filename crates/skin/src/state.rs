//! 状态位：九个正交位，不是九个互斥的状态。
//!
//! 「一个页签被按下、并且是修改过的、并且有焦点」是同时成立的，所以这里是位集合
//! 而不是枚举。但挑颜色的时候总要选一个「主状态」，于是有 [`Primary`]：
//! `pressed > active > hot > normal`，只用于选主色。
//!
//! 其余六个（`disabled`/`checked`/`focus`/`selected`/`modified`/`alt`）是**叠加位**，
//! 不替换主状态 —— 配方里它们单独给覆盖项，例如 `states.modified.mark` 只改那条
//! 标记的颜色，其余照旧。

use std::fmt;

/// 一组状态位。空集合就是 `normal`。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct State(u16);

impl State {
    /// 指针在上面。
    pub const HOT: State = State(1 << 0);
    /// 是当前那一个（活动页签、活动文档）。
    pub const ACTIVE: State = State(1 << 1);
    /// 正在被按着。
    pub const PRESSED: State = State(1 << 2);
    /// 不可交互。**不是**主状态，而是叠加位：灰掉的按钮也可以是 hot。
    pub const DISABLED: State = State(1 << 3);
    /// 勾选/开启。
    pub const CHECKED: State = State(1 << 4);
    /// 有键盘焦点。
    pub const FOCUS: State = State(1 << 5);
    /// 被选中（列表行、文字选区所在的部件）。
    pub const SELECTED: State = State(1 << 6);
    /// 内容改过（文档页签的 `*`）。
    pub const MODIFIED: State = State(1 << 7);
    /// 交替行/交替格，斑马纹用。
    pub const ALT: State = State(1 << 8);

    /// 全部位，按 [`State::names`] 的顺序。
    pub const ALL: [State; 9] = [
        State::HOT,
        State::ACTIVE,
        State::PRESSED,
        State::DISABLED,
        State::CHECKED,
        State::FOCUS,
        State::SELECTED,
        State::MODIFIED,
        State::ALT,
    ];

    /// 名字表，顺序与 [`State::ALL`] 一致。
    pub const NAMES: [&'static str; 9] = [
        "hot",
        "active",
        "pressed",
        "disabled",
        "checked",
        "focus",
        "selected",
        "modified",
        "alt",
    ];

    /// 空集合：`normal`。
    pub const fn normal() -> State {
        State(0)
    }

    /// 位掩码。
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// 从裸位构造。只给回放层/序列化用，别拿它拼状态。
    pub const fn from_bits(bits: u16) -> State {
        State(bits & 0x1FF)
    }

    /// 是否含某个位。
    pub const fn contains(self, other: State) -> bool {
        self.0 & other.0 == other.0
    }

    /// 并上一个位。
    pub const fn with(self, other: State) -> State {
        State(self.0 | other.0)
    }

    /// 去掉一个位。
    pub const fn without(self, other: State) -> State {
        State(self.0 & !other.0)
    }

    /// 是否空集合。
    pub const fn is_normal(self) -> bool {
        self.0 == 0
    }

    /// 挑主状态：`pressed > active > hot > normal`。`disabled` 不参与 ——
    /// 灰掉的按钮仍然可以是 hot，配方里靠 `states.disabled` 覆盖。
    pub const fn primary(self) -> Primary {
        if self.contains(State::PRESSED) {
            Primary::Pressed
        } else if self.contains(State::ACTIVE) {
            Primary::Active
        } else if self.contains(State::HOT) {
            Primary::Hot
        } else {
            Primary::Normal
        }
    }

    /// 集合里的名字，按 [`State::NAMES`] 的顺序。
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        State::ALL
            .into_iter()
            .zip(State::NAMES)
            .filter(move |(bit, _)| self.contains(*bit))
            .map(|(_, name)| name)
    }

    /// 按名字拼一个集合。任何一个名字不认识就整体失败 —— 皮肤里写错一个
    /// 状态名应该报错，而不是悄悄少一个位。
    pub fn parse<I, S>(names: I) -> Result<State, StateParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = State::normal();
        for name in names {
            let want = name.as_ref().trim();
            if want.is_empty() || want.eq_ignore_ascii_case("normal") {
                continue;
            }
            let mut hit = None;
            for (bit, known) in State::ALL.into_iter().zip(State::NAMES) {
                if known.eq_ignore_ascii_case(want) {
                    hit = Some(bit);
                    break;
                }
            }
            match hit {
                Some(bit) => out = out.with(bit),
                None => {
                    return Err(StateParseError {
                        name: want.to_string(),
                    });
                }
            }
        }
        Ok(out)
    }
}

impl fmt::Display for State {
    /// `hot+modified`，空集合是 `normal`。只给日志和诊断输出用。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for name in self.names() {
            if !first {
                f.write_str("+")?;
            }
            f.write_str(name)?;
            first = false;
        }
        if first {
            f.write_str("normal")?;
        }
        Ok(())
    }
}

/// 挑主色用的那一个状态。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Primary {
    /// 什么都没有。
    Normal,
    /// 指针在上面。
    Hot,
    /// 是当前那一个。
    Active,
    /// 正在被按着。
    Pressed,
}

impl Primary {
    /// 名字，与 [`State::NAMES`] 的前四个一致。
    pub const fn as_str(self) -> &'static str {
        match self {
            Primary::Normal => "normal",
            Primary::Hot => "hot",
            Primary::Active => "active",
            Primary::Pressed => "pressed",
        }
    }
}

impl fmt::Display for Primary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 状态名不认识。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateParseError {
    /// 用户写的那个名字（已 trim）。
    pub name: String,
}

impl fmt::Display for StateParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown state {:?}", self.name)
    }
}

impl std::error::Error for StateParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字表钉死：主题文件与 Lua 用的就是这九个词。
    #[test]
    fn the_nine_names_are_the_contract() {
        assert_eq!(
            State::NAMES.to_vec(),
            vec![
                "hot", "active", "pressed", "disabled", "checked", "focus", "selected",
                "modified", "alt",
            ]
        );
        assert_eq!(State::ALL.len(), State::NAMES.len());
    }

    /// 每个位只占一位，且互不重叠 —— 重叠了 `contains` 就会误判。
    #[test]
    fn the_bits_are_orthogonal() {
        let mut seen = 0u16;
        for bit in State::ALL {
            assert_eq!(bit.bits().count_ones(), 1);
            assert_eq!(seen & bit.bits(), 0, "{bit} overlaps");
            seen |= bit.bits();
        }
        assert_eq!(seen, 0x1FF, "nine bits, no more");
    }

    /// 主状态只认四个，且按 pressed > active > hot 的优先级。
    #[test]
    fn the_primary_state_follows_the_priority() {
        assert_eq!(State::normal().primary(), Primary::Normal);
        assert_eq!(State::HOT.primary(), Primary::Hot);
        assert_eq!(State::HOT.with(State::ACTIVE).primary(), Primary::Active);
        assert_eq!(
            State::HOT.with(State::ACTIVE).with(State::PRESSED).primary(),
            Primary::Pressed
        );
    }

    /// `disabled` 是叠加位：灰掉的按钮照样可以是 hot，主色仍然按 hot 挑。
    #[test]
    fn disabled_is_an_overlay_not_a_primary() {
        let grey_hot = State::HOT.with(State::DISABLED);
        assert_eq!(grey_hot.primary(), Primary::Hot);
        assert!(grey_hot.contains(State::DISABLED));
    }

    /// 解析：`normal` 与空串都是空集合，名字大小写不敏感。
    #[test]
    fn parsing_accepts_normal_and_ignores_case() {
        assert_eq!(State::parse(["normal"]), Ok(State::normal()));
        assert_eq!(State::parse(Vec::<&str>::new()), Ok(State::normal()));
        assert_eq!(State::parse([" HOT ", "Modified"]), Ok(State::HOT.with(State::MODIFIED)));
    }

    /// 一个名字写错就整体失败，不静默少一位。
    #[test]
    fn parsing_rejects_a_typo() {
        assert_eq!(
            State::parse(["hot", "hott"]),
            Err(StateParseError { name: "hott".into() })
        );
    }

    /// 每个名字都能解析回自己，`Display` 也能读回来。
    #[test]
    fn every_name_round_trips() {
        for (bit, name) in State::ALL.into_iter().zip(State::NAMES) {
            assert_eq!(State::parse([name]), Ok(bit));
            assert_eq!(bit.to_string(), name);
        }
        assert_eq!(State::normal().to_string(), "normal");
        assert_eq!(
            State::HOT.with(State::MODIFIED).to_string(),
            "hot+modified"
        );
    }

    /// 裸位只留九位，高位脏数据不许漏进缓存键。
    #[test]
    fn from_bits_masks_the_top_bits() {
        assert_eq!(State::from_bits(0xFFFF).bits(), 0x1FF);
    }
}
