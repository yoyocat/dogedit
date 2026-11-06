//! 运行时外观：`App.ui.style`（写）和 `App.theme`（读）。
//!
//! 为什么必须有这一层：主题文件是**声明式**的，写不了「文件改了就把页签标记换成
//! 橙色、保存后换回来」。而这件事是编辑器该有的样子 —— 会变的那部分只能由脚本
//! 表达。优先级从低到高只有三层：出厂值 → `themes/<name>.ini` → 这里。没有第四
//! 层，因为包不再自带主题。
//!
//! **四段对四个动词**，和主题文件的四段一一对应：
//!
//! | 主题文件 | 这里 |
//! | --- | --- |
//! | `[Theme] tab.mark=#E67E22` | `style:set("tab.mark", "#E67E22")` |
//! | `[Metrics] status.height=26` | `style:metric("status.height", 26)` |
//! | `[Shape] tab.states.active.radius=0` | `style:shape("tab", "active", "radius", 0)` |
//! | `[Fonts] mono.face=Consolas` | `style:font("mono", { face = "Consolas" })` |
//!
//! **值传 `nil` 就是复位**（回到主题给的那个）。这条规则四段通用，所以不需要
//! 四套 `reset` 重载。
//!
//! **这一层不直接改 `Theme` / `Metrics` / `Recipe` / `Fonts`。** `dg-script` 不
//! 认识外壳，它只把「谁被改成了什么」收在一个 [`Overrides`] 里，外壳合成外观时
//! 最后一步盖上去。所以 `style:set` 之后是「下一次重画就对了」，不是「当场」——
//! 同一帧里连改十个 token 只重画一次，这正是想要的方向。
//!
//! 脏位（[`DIRTY_APPEARANCE`]）由外壳在每个入口返回后取一次（`take_dirty`），
//! 和 `bridge.edited` 同一个套路：脚本里改状态，外壳统一收尾。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dg_render::theme::Color;
use dg_skin::{FontSpec, FontToken, MetricId, ShapeKey, ShapeValue, State};
use mlua::{Lua, MultiValue, Table, UserData, UserDataMethods, Value};

/// 外观改了（有 token 被覆盖或复位）。外壳看到这一位就重算主题、重画。
pub const DIRTY_APPEARANCE: u8 = 1;

/// 当前主题名（`App.theme.name()`）。外壳在合成主题时推一次。
const REG_THEME_NAME: &str = "dogedit.theme_name";

/// 脏位。`Rc<Cell<u8>>` 而不是注册表里的整数：外壳要在**不借 Lua** 的地方读它
/// （每次入口返回后调一次 `take_dirty`）。
pub(crate) struct Dirty(pub Rc<Cell<u8>>);

// ---------------------------------------------------------------------------
// 覆盖表
// ---------------------------------------------------------------------------

/// 一条形状覆盖。区域名**已经展开过**：`style:shape("status", …)` 会变成两条
/// （`status.pane` / `status.grip`），和主题文件里那个族名的展开结果一致。
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeOverride {
    pub region: &'static str,
    pub state: Option<String>,
    pub key: ShapeKey,
    pub value: ShapeVal,
}

/// 形状值。`dg_skin::ShapeValue` 借用字符串，而这里要存下来，所以另起一个
/// 拥有的版本，盖上去的时候再借出去。
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeVal {
    Number(f32),
    Text(String),
}

impl ShapeVal {
    /// 借一份给 `dg_skin::Recipe::set` 用（它要的是借用的版本）。
    pub fn as_value(&self) -> ShapeValue<'_> {
        match self {
            ShapeVal::Number(v) => ShapeValue::Number(*v),
            ShapeVal::Text(s) => ShapeValue::Text(s),
        }
    }

    /// 给 `style:shapes()` 用：数字写成 `0`、`-1.5`，字符串原样。
    fn to_lua(&self, lua: &Lua) -> mlua::Result<Value> {
        Ok(match self {
            ShapeVal::Number(v) => Value::Number(*v as f64),
            ShapeVal::Text(s) => Value::String(lua.create_string(s)?),
        })
    }
}

/// 一个字体 token 上**被改过的那几个字段**。
///
/// 不是整份 [`FontSpec`]：`style:font("mono", { size = 12 })` 只该动字号，字面
/// 得留着主题给的那个。字段级的 `Option` 就是这个意思 —— `None` = 没动过。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontPatch {
    pub face: Option<String>,
    pub size: Option<f32>,
    pub weight: Option<u16>,
    pub italic: Option<bool>,
}

impl FontPatch {
    /// 空补丁（一个字段都没动）。
    fn is_empty(&self) -> bool {
        self.face.is_none() && self.size.is_none() && self.weight.is_none() && self.italic.is_none()
    }

    /// 把补丁盖到 `base` 上，夹一遍，得到能直接交给 `Fonts::set` 的整份配置。
    pub fn apply(&self, base: &FontSpec) -> FontSpec {
        let mut spec = base.clone();
        if let Some(face) = &self.face {
            spec.face = face.clone();
        }
        if let Some(size) = self.size {
            spec.size = size;
        }
        if let Some(weight) = self.weight {
            spec.weight = weight;
        }
        if let Some(italic) = self.italic {
            spec.italic = italic;
        }
        spec.clamp();
        spec
    }
}

/// 字体补丁的一个字段。`style:font("mono", "size", nil)` 要按名字清一个字段，
/// 字符串到字段的翻译只做一次。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FontField {
    Face,
    Size,
    Weight,
    Italic,
}

impl FontField {
    fn parse(name: &str) -> mlua::Result<FontField> {
        match name.trim().to_ascii_lowercase().as_str() {
            "face" => Ok(FontField::Face),
            "size" => Ok(FontField::Size),
            "weight" => Ok(FontField::Weight),
            "italic" => Ok(FontField::Italic),
            other => Err(mlua::Error::runtime(format!(
                "ui.style: {other:?} 不是字体字段；能写的有 face size weight italic"
            ))),
        }
    }
}

/// 全部运行时覆盖。**一张表装四段**，因为外壳只需要问一次「有没有人改过外观」。
///
/// 用 `Vec` 而不是 `BTreeMap`：条目个数是个位数，插入顺序比排序更有用
/// （`style:shapes()` 的输出顺序就是脚本改的顺序），而 `MetricId` / `FontToken`
/// 也不必为了一张表去实现 `Ord`。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overrides {
    colors: Vec<(String, Color)>,
    metrics: Vec<(MetricId, f32)>,
    shapes: Vec<ShapeOverride>,
    fonts: Vec<(FontToken, FontPatch)>,
}

impl Overrides {
    /// 被改过的颜色，按改的顺序。
    pub fn colors(&self) -> &[(String, Color)] {
        &self.colors
    }

    /// 被改过的度量。
    pub fn metrics(&self) -> &[(MetricId, f32)] {
        &self.metrics
    }

    /// 被改过的形状（族名已展开）。
    pub fn shapes(&self) -> &[ShapeOverride] {
        &self.shapes
    }

    /// 被改过的字体字段。
    pub fn fonts(&self) -> &[(FontToken, FontPatch)] {
        &self.fonts
    }

    /// 一条都没改。
    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
            && self.metrics.is_empty()
            && self.shapes.is_empty()
            && self.fonts.is_empty()
    }

    fn set_color(&mut self, token: &str, color: Option<Color>) {
        match color {
            Some(c) => match self.colors.iter_mut().find(|(n, _)| n == token) {
                Some(slot) => slot.1 = c,
                None => self.colors.push((token.to_string(), c)),
            },
            None => self.colors.retain(|(n, _)| n != token),
        }
    }

    fn color_of(&self, token: &str) -> Option<Color> {
        self.colors.iter().find(|(n, _)| n == token).map(|(_, c)| *c)
    }

    fn set_metric(&mut self, id: MetricId, value: Option<f32>) {
        match value {
            Some(v) => match self.metrics.iter_mut().find(|(m, _)| *m == id) {
                Some(slot) => slot.1 = v,
                None => self.metrics.push((id, v)),
            },
            None => self.metrics.retain(|(m, _)| *m != id),
        }
    }

    fn set_shape(&mut self, o: ShapeOverride) {
        match self
            .shapes
            .iter_mut()
            .find(|s| s.region == o.region && s.state == o.state && s.key == o.key)
        {
            Some(slot) => slot.value = o.value,
            None => self.shapes.push(o),
        }
    }

    fn clear_shape(&mut self, region: &str, state: Option<&str>, key: ShapeKey) {
        self.shapes
            .retain(|s| !(s.region == region && s.state.as_deref() == state && s.key == key));
    }

    fn set_font_field(&mut self, token: FontToken, field: FontField, value: FontValue) {
        let patch = match self.fonts.iter_mut().find(|(t, _)| *t == token) {
            Some((_, p)) => p,
            None => {
                self.fonts.push((token, FontPatch::default()));
                &mut self.fonts.last_mut().expect("刚 push 过").1
            }
        };
        match (field, value) {
            (FontField::Face, FontValue::Face(v)) => patch.face = v,
            (FontField::Size, FontValue::Size(v)) => patch.size = v,
            (FontField::Weight, FontValue::Weight(v)) => patch.weight = v,
            (FontField::Italic, FontValue::Italic(v)) => patch.italic = v,
            // 类型不匹配由调用方（`font_field_value`）先挡掉，这里只是兜底。
            _ => return,
        }
        // 字段全清空了就把这个 token 整条拿掉：留一条空补丁会让
        // `style:fonts()` 多出一个「改了但没改」的行。
        if patch.is_empty() {
            self.fonts.retain(|(t, _)| *t != token);
        }
    }
}

/// 一个字体字段的值。分类型而不是统一成字符串：`size` 要数字，`italic` 要布尔，
/// 从 Lua 收的时候就要判断，别等到盖上去才发现 `"big"` 转不成 f32。
#[derive(Clone, Debug, PartialEq)]
enum FontValue {
    Face(Option<String>),
    Size(Option<f32>),
    Weight(Option<u16>),
    Italic(Option<bool>),
}

// ---------------------------------------------------------------------------
// 装表 / 借表
// ---------------------------------------------------------------------------

/// 建表 + 把脏位挂进 `app_data`。`dirty` 由 `ScriptHost` 持有，这里只是挂一份
/// 克隆 —— 两处必须是**同一个** `Cell`，否则外壳永远读不到脚本置的位。
pub(crate) fn install(lua: &Lua, dirty: Rc<Cell<u8>>) -> mlua::Result<()> {
    lua.set_named_registry_value(REG_THEME_NAME, String::new())?;
    lua.set_app_data(RefCell::new(Overrides::default()));
    lua.set_app_data(Dirty(dirty));
    Ok(())
}

fn with<R>(lua: &Lua, f: impl FnOnce(&mut Overrides) -> R) -> mlua::Result<R> {
    let slot = lua
        .app_data_mut::<RefCell<Overrides>>()
        .ok_or_else(|| mlua::Error::runtime("ui.style: 覆盖表没装上"))?;
    Ok(f(&mut slot.borrow_mut()))
}

fn snapshot(lua: &Lua) -> Overrides {
    lua.app_data_ref::<RefCell<Overrides>>()
        .map(|slot| slot.borrow().clone())
        .unwrap_or_default()
}

fn palette(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>(crate::paint::REG_PALETTE)
}

/// 置脏位。`paint.rs` 的布局那一半也用它 —— 两个模块的位在同一个 `Cell` 里，
/// 各置各的位，外壳一次取走。
pub(crate) fn mark(lua: &Lua, bits: u8) {
    if let Some(d) = lua.app_data_ref::<Dirty>() {
        d.0.set(d.0.get() | bits);
    }
}

/// 当前生效的颜色：覆盖优先，其次调色板（也就是主题给的）。
fn effective(lua: &Lua, token: &str) -> mlua::Result<Option<String>> {
    let over = snapshot(lua);
    if let Some(c) = over.color_of(token) {
        return Ok(Some(hex(c)));
    }
    match palette(lua)?.get::<Value>(token) {
        Ok(Value::String(s)) => Ok(Some(s.to_string_lossy().to_string())),
        _ => Ok(None),
    }
}

fn hex(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

// ---------------------------------------------------------------------------
// 校验：写错当场报错，别静默收下
// ---------------------------------------------------------------------------

fn check_token(token: &str) -> mlua::Result<()> {
    if dg_render::tokens::TOKEN_NAMES.contains(&token) {
        return Ok(());
    }
    // 写错一个字母不报错、只是没反应，是这类接口最难查的一种病 —— 当场拒绝，
    // 并把「该长什么样」说出来。
    Err(mlua::Error::runtime(format!(
        "ui.style: {token:?} 不是 token 名（共 {} 个，形如 tab.mark / status.bg / scroll.thumb）",
        dg_render::tokens::TOKEN_NAMES.len()
    )))
}

fn check_metric(name: &str) -> mlua::Result<MetricId> {
    MetricId::parse(name).map_err(|_| {
        mlua::Error::runtime(format!(
            "ui.style: {name:?} 不是度量名；能写的有 {}",
            MetricId::ALL
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ))
    })
}

/// 区域名 → 要写的那几个区域。族名（`status` / `scroll` / `menu`）在这里展开，
/// 和主题文件走的是同一个 [`dg_skin::regions_for`] —— 同一个词在两边是同一件事。
fn check_region(name: &str) -> mlua::Result<Vec<&'static str>> {
    let regions = dg_skin::regions_for(name);
    if regions.is_empty() {
        return Err(mlua::Error::runtime(format!(
            "ui.style: {name:?} 不是区域；能写的有 {}",
            dg_skin::region_names()
        )));
    }
    Ok(regions)
}

fn check_state(name: &str) -> mlua::Result<String> {
    if !State::NAMES.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        return Err(mlua::Error::runtime(format!(
            "ui.style: 状态名 {name:?} 不认识；能写的有 {}",
            State::NAMES.join(" ")
        )));
    }
    // 统一成小写：`active` 和 `Active` 是同一层，不能各存一份。
    Ok(name.to_ascii_lowercase())
}

fn check_shape_key(name: &str) -> mlua::Result<ShapeKey> {
    ShapeKey::parse(name).ok_or_else(|| {
        mlua::Error::runtime(format!(
            "ui.style: {name:?} 不是形状键；能写的有 {}",
            ShapeKey::ALL
                .iter()
                .map(|k| k.name())
                .collect::<Vec<_>>()
                .join(" ")
        ))
    })
}

fn check_font_token(name: &str) -> mlua::Result<FontToken> {
    FontToken::parse(name).ok_or_else(|| {
        mlua::Error::runtime(format!(
            "ui.style: {name:?} 不是字体 token；能写的有 {}",
            FontToken::ALL
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>()
                .join(" ")
        ))
    })
}

/// Lua 的一个值 → 一个数。
///
/// 必须同时认 `Integer`：Lua 5.4 里 `26` 是整数、`26.0` 是浮点，而 mlua 把
/// 两者分给不同的变体。只认 `Number` 会让「写 26 报错、写 26.0 能过」这种
/// 见鬼的差异出现在脚本里。
fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) if n.is_finite() => Some(*n),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

/// Lua 的一个值 → 一个字体字段的值。`nil` = 清掉这个字段。
fn font_field_value(field: FontField, value: Value) -> mlua::Result<FontValue> {
    Ok(match field {
        FontField::Face => FontValue::Face(match value {
            Value::Nil => None,
            Value::String(s) => Some(s.to_string_lossy().to_string()),
            _ => return Err(bad_type("face", "字面名（字符串）")),
        }),
        FontField::Size => FontValue::Size(match as_number(&value) {
            _ if matches!(value, Value::Nil) => None,
            Some(n) => Some(n as f32),
            None => return Err(bad_type("size", "字号（数字，pt）")),
        }),
        FontField::Weight => FontValue::Weight(if matches!(value, Value::Nil) {
            None
        } else {
            Some(match value {
                Value::String(s) => match s.to_string_lossy().to_ascii_lowercase().as_str() {
                    "normal" => 400,
                    "bold" => 700,
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "ui.style: weight 要写 100–900 或 normal / bold，你写的是 {other:?}"
                        )));
                    }
                },
                ref other => match as_number(other) {
                    Some(n) => n.clamp(100.0, 900.0) as u16,
                    None => return Err(bad_type("weight", "字重（100–900 或 normal / bold）")),
                },
            })
        }),
        FontField::Italic => FontValue::Italic(match value {
            Value::Nil => None,
            Value::Boolean(b) => Some(b),
            _ => return Err(bad_type("italic", "true / false")),
        }),
    })
}

fn bad_type(field: &str, want: &str) -> mlua::Error {
    mlua::Error::runtime(format!("ui.style: {field} 要写{want}"))
}

fn want_str(value: Option<Value>, what: &str) -> mlua::Result<String> {
    match value {
        Some(Value::String(s)) => Ok(s.to_string_lossy().to_string()),
        _ => Err(mlua::Error::runtime(format!(
            "ui.style: 第一个参数要是{what}（字符串）"
        ))),
    }
}

fn opt_str(value: Option<Value>, what: &str) -> mlua::Result<Option<String>> {
    match value {
        Some(Value::String(s)) => Ok(Some(s.to_string_lossy().to_string())),
        Some(Value::Nil) | None => Ok(None),
        _ => Err(mlua::Error::runtime(format!(
            "ui.style: {what}要是字符串或 nil"
        ))),
    }
}

// ---------------------------------------------------------------------------
// App.ui.style
// ---------------------------------------------------------------------------

/// `App.ui.style`：运行时改外观。
///
/// 四段四组动词，写法和主题文件的四段一一对应；**值传 `nil` 就是复位**。
pub struct StyleApi;

impl UserData for StyleApi {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // ── 颜色 ──────────────────────────────────────────────────────
        // style:set("tab.mark", "#E67E22")；传 nil 等于 style:reset。
        methods.add_method("set", |lua, _, (token, color): (String, Option<String>)| {
            check_token(&token)?;
            let c = match color {
                Some(text) => Some(crate::paint::parse_color(&text)?),
                None => None,
            };
            with(lua, |o| o.set_color(&token, c))?;
            mark(lua, DIRTY_APPEARANCE);
            Ok(())
        });
        // style:reset("tab.mark") —— 回到主题给的那个值。
        methods.add_method("reset", |lua, _, token: String| {
            check_token(&token)?;
            with(lua, |o| o.set_color(&token, None))?;
            mark(lua, DIRTY_APPEARANCE);
            Ok(())
        });
        // style:get("tab.mark") -> "#RRGGBB"（含覆盖）。名字不认识返回 nil。
        methods.add_method("get", |lua, _, token: String| match effective(lua, &token)? {
            Some(s) => Ok(Value::String(lua.create_string(&s)?)),
            None => Ok(Value::Nil),
        });
        // style:list() -> { ["tab.mark"] = "#E67E22", … } 只列**被改过**的那几个。
        // 想知道全部生效值用 `App.theme.list()`。
        methods.add_method("list", |lua, _, ()| {
            let out = lua.create_table()?;
            for (token, color) in &snapshot(lua).colors {
                out.set(token.as_str(), hex(*color))?;
            }
            Ok(out)
        });

        // ── 度量 ──────────────────────────────────────────────────────
        // style:metric("status.height", 26)；传 nil 复位。
        methods.add_method("metric", |lua, _, (name, value): (String, Option<f64>)| {
            let id = check_metric(&name)?;
            with(lua, |o| o.set_metric(id, value.map(|v| v as f32)))?;
            mark(lua, DIRTY_APPEARANCE);
            Ok(())
        });
        // style:metrics() -> { ["status.height"] = 26 } 只列被改过的。
        // 这些行原样贴进主题文件的 `[Metrics]` 就变成永久的。
        methods.add_method("metrics", |lua, _, ()| {
            let out = lua.create_table()?;
            for (id, value) in &snapshot(lua).metrics {
                out.set(id.as_str(), *value as f64)?;
            }
            Ok(out)
        });

        // ── 形状 ──────────────────────────────────────────────────────
        // style:shape("tab", "active", "radius", 0)；state 可为 nil（常态层），
        // 值传 nil 复位。
        methods.add_method("shape", |lua, _, args: MultiValue| {
            let mut it = args.into_iter();
            let region = want_str(it.next(), "区域名")?;
            let state = opt_str(it.next(), "状态名")?;
            let key = check_shape_key(&want_str(it.next(), "形状键")?)?;
            let value = it.next().unwrap_or(Value::Nil);

            let regions = check_region(&region)?;
            let state = match state {
                Some(s) => Some(check_state(&s)?),
                None => None,
            };
            let value = match value {
                Value::Nil => None,
                ref other => match as_number(other) {
                    Some(n) => Some(ShapeVal::Number(n as f32)),
                    None => match other {
                        Value::String(s) if key == ShapeKey::Font => {
                            Some(ShapeVal::Text(s.to_string_lossy().to_string()))
                        }
                        Value::String(_) => {
                            return Err(mlua::Error::runtime(format!(
                                "ui.style: {} 要写数字，只有 font 收字符串",
                                key.name()
                            )));
                        }
                        _ => {
                            return Err(mlua::Error::runtime(format!(
                                "ui.style: {} 的值要是数字或 nil",
                                key.name()
                            )));
                        }
                    },
                },
            };

            with(lua, |o| {
                for r in &regions {
                    match &value {
                        Some(v) => o.set_shape(ShapeOverride {
                            region: r,
                            state: state.clone(),
                            key,
                            value: v.clone(),
                        }),
                        None => o.clear_shape(r, state.as_deref(), key),
                    }
                }
            })?;
            mark(lua, DIRTY_APPEARANCE);
            Ok(())
        });
        // style:shapes() -> { ["tab.states.active.radius"] = 0 }
        methods.add_method("shapes", |lua, _, ()| {
            let out = lua.create_table()?;
            for s in &snapshot(lua).shapes {
                let mut key = String::from(s.region);
                if let Some(state) = &s.state {
                    key.push_str(".states.");
                    key.push_str(state);
                }
                key.push('.');
                key.push_str(s.key.name());
                out.set(key, s.value.to_lua(lua)?)?;
            }
            Ok(out)
        });

        // ── 字体 ──────────────────────────────────────────────────────
        // style:font("mono", { face = "Consolas", size = 12 })：按字段改，
        // 没写的字段留着主题给的。传 nil 清掉整个 token。
        // style:font("mono", "size", 12)：单字段写法（清一个字段要用它：
        // 表里的 nil 等于「没写」）。
        methods.add_method("font", |lua, _, args: MultiValue| {
            let mut it = args.into_iter();
            let token = check_font_token(&want_str(it.next(), "字体 token")?)?;
            let first = it.next().unwrap_or(Value::Nil);
            let second = it.next();

            match (first, second) {
                // font(token, nil)：清掉整个 token。
                (Value::Nil, None) => {
                    with(lua, |o| o.fonts.retain(|(t, _)| *t != token))?;
                }
                // font(token, { … })：按字段改。
                (Value::Table(t), None) => {
                    let mut fields = Vec::new();
                    for pair in t.pairs::<String, Value>() {
                        let (name, value) = pair?;
                        let field = FontField::parse(&name)?;
                        fields.push((field, font_field_value(field, value)?));
                    }
                    if fields.is_empty() {
                        return Err(mlua::Error::runtime(
                            "ui.style: font 的表是空的 —— 想复位传 nil，想改字段至少写一个",
                        ));
                    }
                    with(lua, |o| {
                        for (field, value) in fields {
                            o.set_font_field(token, field, value);
                        }
                    })?;
                }
                // font(token, "size", 12)：单字段。
                (Value::String(name), value) => {
                    let field = FontField::parse(&name.to_string_lossy())?;
                    let value = font_field_value(field, value.unwrap_or(Value::Nil))?;
                    with(lua, |o| o.set_font_field(token, field, value))?;
                }
                (other, _) => {
                    return Err(mlua::Error::runtime(format!(
                        "ui.style: font 的第二个参数要是表、字段名或 nil，收到的是 {}",
                        other.type_name()
                    )));
                }
            }
            mark(lua, DIRTY_APPEARANCE);
            Ok(())
        });
        // style:fonts() -> { ["mono.face"] = "Consolas" }
        methods.add_method("fonts", |lua, _, ()| {
            let out = lua.create_table()?;
            for (token, patch) in &snapshot(lua).fonts {
                if let Some(face) = &patch.face {
                    out.set(format!("{}.face", token.name()), face.as_str())?;
                }
                if let Some(size) = patch.size {
                    out.set(format!("{}.size", token.name()), size as f64)?;
                }
                if let Some(weight) = patch.weight {
                    out.set(format!("{}.weight", token.name()), weight as f64)?;
                }
                if let Some(italic) = patch.italic {
                    out.set(format!("{}.italic", token.name()), italic)?;
                }
            }
            Ok(out)
        });
    }
}

/// `App.theme`：只读当前生效的主题。
///
/// 给「想跟着主题走、但不在绘制回调里」的脚本用。绘制回调里用 `ctx.theme` 更快
/// —— 那张表是同一个 `Theme` 的投影，读它不进 Lua 之外的路。
pub(crate) fn theme_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "name",
        lua.create_function(|lua, ()| {
            Ok(lua
                .named_registry_value::<String>(REG_THEME_NAME)
                .unwrap_or_default())
        })?,
    )?;
    table.set(
        "color",
        lua.create_function(|lua, token: String| match effective(lua, &token)? {
            Some(s) => Ok(Value::String(lua.create_string(&s)?)),
            None => Ok(Value::Nil),
        })?,
    )?;
    // `list()` 给的是**全部 89 个**生效值（覆盖已经盖上去），不是「被改过的」。
    table.set(
        "list",
        lua.create_function(|lua, ()| {
            let out = lua.create_table()?;
            for name in dg_render::tokens::TOKEN_NAMES {
                if let Some(s) = effective(lua, name)? {
                    out.set(*name, s)?;
                }
            }
            Ok(out)
        })?,
    )?;
    Ok(table)
}

/// 外壳合成主题时调：把当前主题名记下来（`App.theme.name()` 读它）。
pub(crate) fn set_theme_name(lua: &Lua, name: &str) -> mlua::Result<()> {
    lua.set_named_registry_value(REG_THEME_NAME, name.to_string())
}

/// 外壳合成主题时调：读一遍覆盖表。四段一起给，因为外壳只要问一次。
pub fn overrides_of(lua: &Lua) -> Overrides {
    snapshot(lua)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeadlessBridge, ScriptHost};

    fn host() -> ScriptHost {
        ScriptHost::new().expect("lua")
    }

    fn run(h: &ScriptHost, src: &str) {
        h.load("t", src, &mut HeadlessBridge).expect("loads");
    }

    fn fails(h: &ScriptHost, src: &str) -> String {
        let err = h
            .load("t", src, &mut HeadlessBridge)
            .expect_err("must fail");
        format!("{err}")
    }

    /// `set` 记下覆盖、`get` 读回来、脏位立起来 —— 三件事一起验，因为少任何
    /// 一件这个接口都是「写了没反应」。
    #[test]
    fn set_marks_dirty_and_get_reads_it_back() {
        let h = host();
        assert_eq!(h.take_dirty(), 0, "新建的宿主是干净的");
        run(
            &h,
            r##"
            App.ui.style:set("tab.mark", "#E67E22")
            assert(App.ui.style:get("tab.mark") == "#E67E22")
            "##,
        );
        assert_eq!(h.take_dirty() & DIRTY_APPEARANCE, DIRTY_APPEARANCE);
        assert_eq!(
            h.style_overrides().colors(),
            &[("tab.mark".to_string(), Color::rgb(0xE6, 0x7E, 0x22))]
        );
        // 取过一次就清零：外壳每个入口取一次，不能同一个改动重画十遍。
        assert_eq!(h.take_dirty(), 0);
    }

    /// `reset` 把覆盖拿掉，并且**照样**置脏位 —— 不然「保存后换回来」只会换不回来。
    ///
    /// 复位后 `get` 读到的是主题值而不是 nil（`get` 不区分来源），所以这里先给
    /// 一个能认出来的调色板，才分得清「拿掉了」和「读到了旧覆盖」。
    #[test]
    fn reset_drops_the_override_and_still_marks_dirty() {
        let h = host();
        let mut theme = dg_render::Theme::default();
        theme.tab_mark_fg = Color::rgb(0x30, 0x31, 0x32);
        h.set_palette(&theme);
        run(
            &h,
            r##"
            App.ui.style:set("tab.mark", "#E67E22")
            assert(App.ui.style:get("tab.mark") == "#E67E22")
            App.ui.style:reset("tab.mark")
            assert(App.ui.style:get("tab.mark") == "#303132", "覆盖已经拿掉了")
            "##,
        );
        assert!(h.style_overrides().is_empty());
        assert_eq!(h.take_dirty() & DIRTY_APPEARANCE, DIRTY_APPEARANCE);
    }

    /// `set(token, nil)` 和 `reset(token)` 是同一件事 —— 「值传 nil 就是复位」
    /// 这条规则四段通用，不能只有颜色这一格是例外。
    #[test]
    fn set_to_nil_is_reset() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:set("tab.mark", "#E67E22")
            App.ui.style:set("tab.mark", nil)
            "##,
        );
        assert!(h.style_overrides().is_empty());
    }

    /// 没覆盖时 `get` 读的是**调色板**（也就是主题给的），不是 nil ——
    /// 「想跟着主题走，但要在命令里问一句」是这个接口的常见用法。
    #[test]
    fn get_falls_back_to_the_theme_palette() {
        let h = host();
        let mut theme = dg_render::Theme::default();
        theme.statusbar_bg = Color::rgb(1, 2, 3);
        h.set_palette(&theme);
        run(
            &h,
            r##"
            assert(App.ui.style:get("status.bg") == "#010203")
            App.ui.style:set("status.bg", "#0A0B0C")
            assert(App.ui.style:get("status.bg") == "#0A0B0C", "覆盖优先")
            App.ui.style:reset("status.bg")
            assert(App.ui.style:get("status.bg") == "#010203", "复位回主题")
            "##,
        );
    }

    /// 名字写错当场报错。静默收下就是「这个颜色怎么没变」—— 那是最难查的一种。
    #[test]
    fn an_unknown_token_is_refused_at_the_call() {
        let h = host();
        let text = fails(&h, r##"App.ui.style:set("tab.mrak", "#E67E22")"##);
        assert!(text.contains("tab.mrak"), "{text}");
        assert!(text.contains("不是 token 名"), "{text}");
        assert!(h.style_overrides().is_empty(), "一个字段都不该动");
    }

    /// `App.theme` 是只读的那一份：名字由外壳推，颜色含覆盖，`list()` 是全部 89 个。
    #[test]
    fn the_theme_table_reads_name_colour_and_the_whole_list() {
        let h = host();
        h.set_palette(&dg_render::Theme::default());
        h.set_theme_name("Neon");
        run(
            &h,
            r##"
            assert(App.theme.name() == "Neon", App.theme.name())
            App.ui.style:set("tab.mark", "#E67E22")
            assert(App.theme.color("tab.mark") == "#E67E22", "覆盖要跟着")
            local n = 0
            for _ in pairs(App.theme.list()) do n = n + 1 end
            assert(n == 90, "全部 token，实际 " .. n)
            "##,
        );
    }

    // ── 度量 ──────────────────────────────────────────────────────────

    /// 度量覆盖收进同一张表，键就是 `[Metrics]` 里的那个词 —— 所以
    /// `style:metrics()` 的输出能原样贴回主题文件。
    #[test]
    fn metric_overrides_use_the_theme_key_names() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:metric("status.height", 26)
            App.ui.style:metric("tab.pad_x", 8)
            local m = App.ui.style:metrics()
            assert(m["status.height"] == 26, tostring(m["status.height"]))
            assert(m["tab.pad_x"] == 8)
            "##,
        );
        assert_eq!(
            h.style_overrides().metrics(),
            &[(MetricId::StatusHeight, 26.0), (MetricId::TabPadX, 8.0)]
        );
        assert_eq!(h.take_dirty() & DIRTY_APPEARANCE, DIRTY_APPEARANCE);
    }

    /// 传 nil 复位；度量名写错当场报错并列出全部。
    #[test]
    fn metric_resets_and_refuses_a_typo() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:metric("status.height", 26)
            App.ui.style:metric("status.height", nil)
            assert(next(App.ui.style:metrics()) == nil, "应该空了")
            "##,
        );
        assert!(h.style_overrides().is_empty());

        let text = fails(&h, r##"App.ui.style:metric("status.heigth", 26)"##);
        assert!(text.contains("status.heigth"), "{text}");
        assert!(text.contains("不是度量名"), "{text}");
        assert!(text.contains("status.height"), "要列出能写的：{text}");
    }

    // ── 形状 ──────────────────────────────────────────────────────────

    /// 区域族展开成各自的区域（和主题文件同一套规则），键的写法也照抄
    /// `[Shape]`：`区域[.states.状态].键`。
    #[test]
    fn shape_overrides_expand_families_and_write_theme_keys() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:shape("status", nil, "gap", 2)
            App.ui.style:shape("tab", "Active", "radius", 0)
            local s = App.ui.style:shapes()
            assert(s["status.pane.gap"] == 2, tostring(s["status.pane.gap"]))
            assert(s["status.grip.gap"] == 2)
            assert(s["tab.states.active.radius"] == 0, "状态名统一成小写")
            "##,
        );
        let over = h.style_overrides();
        assert_eq!(over.shapes().len(), 3, "{:?}", over.shapes());
        assert_eq!(over.shapes()[2].state.as_deref(), Some("active"));
    }

    /// `font` 是唯一的字符串键：数字和字符串不能混着收，写错当场报错。
    #[test]
    fn shape_accepts_a_font_name_but_not_a_number() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:shape("tab", nil, "font", "font.mono")
            assert(App.ui.style:shapes()["tab.font"] == "font.mono")
            "##,
        );
        let text = fails(&h, r##"App.ui.style:shape("tab", nil, "radius", "big")"##);
        assert!(text.contains("要写数字"), "{text}");
    }

    /// 区域名/状态名/键名三种写错各有各的说法，全都当场拒绝。
    #[test]
    fn shape_refuses_every_kind_of_typo() {
        let h = host();
        let text = fails(&h, r##"App.ui.style:shape("tabs", nil, "radius", 0)"##);
        assert!(text.contains("不是区域"), "{text}");
        assert!(text.contains("tab"), "要列出能写的：{text}");

        let text = fails(&h, r##"App.ui.style:shape("tab", "hover", "radius", 0)"##);
        assert!(text.contains("hover"), "{text}");
        assert!(text.contains("active"), "要列出能写的：{text}");

        let text = fails(&h, r##"App.ui.style:shape("tab", nil, "radios", 0)"##);
        assert!(text.contains("radios"), "{text}");
        assert!(text.contains("不是形状键"), "{text}");
        assert!(h.style_overrides().is_empty());
    }

    /// 复位按 (区域, 状态, 键) 三元组来：同一个键的常态层和 active 层互不影响。
    #[test]
    fn shape_reset_is_per_layer() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:shape("tab", nil, "radius", 2)
            App.ui.style:shape("tab", "active", "radius", 0)
            App.ui.style:shape("tab", "active", "radius", nil)
            local s = App.ui.style:shapes()
            assert(s["tab.radius"] == 2, "常态层还在")
            assert(s["tab.states.active.radius"] == nil, "active 层已经拿掉")
            "##,
        );
        assert_eq!(h.style_overrides().shapes().len(), 1);
    }

    // ── 字体 ──────────────────────────────────────────────────────────

    /// 表写法按字段改，没写的字段不动 —— 只改字号不该把字面换掉。
    #[test]
    fn font_patch_touches_only_the_written_fields() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:font("mono", { face = "Consolas", size = 12 })
            App.ui.style:font("mono", { size = 14 })
            local f = App.ui.style:fonts()
            assert(f["mono.face"] == "Consolas", "字面还在")
            assert(f["mono.size"] == 14, "字号被后一次盖掉")
            "##,
        );
        let over = h.style_overrides();
        let (token, patch) = &over.fonts()[0];
        assert_eq!(*token, FontToken::Mono);
        assert_eq!(patch.face.as_deref(), Some("Consolas"));
        assert_eq!(patch.size, Some(14.0));
    }

    /// 单字段写法：清一个字段要用它（表里的 nil 等于「没写」）。
    /// 字段全清空时整条 token 消失，不留一条空补丁。
    #[test]
    fn font_field_form_can_clear_one_field() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:font("mono", "face", "Consolas")
            App.ui.style:font("mono", "weight", "bold")
            App.ui.style:font("mono", "weight", nil)
            local f = App.ui.style:fonts()
            assert(f["mono.face"] == "Consolas")
            assert(f["mono.weight"] == nil, "bold 已经拿掉")
            App.ui.style:font("mono", nil)
            assert(next(App.ui.style:fonts()) == nil, "整条都拿掉了")
            "##,
        );
        assert!(h.style_overrides().is_empty());
    }

    /// 补丁是**盖在主题给的那份之上**的：主题写了字面，脚本只改字号，字面留着。
    #[test]
    fn a_patch_keeps_the_theme_face() {
        let mut base = FontSpec::default();
        base.face = "Consolas".to_string();
        base.size = 10.0;
        let patch = FontPatch {
            size: Some(18.0),
            ..Default::default()
        };
        let spec = patch.apply(&base);
        assert_eq!(spec.face, "Consolas", "字面是主题给的，脚本没动它");
        assert_eq!(spec.size, 18.0);
    }

    /// 字号/字重越界只夹不报（和主题文件同一条规则），但类型写错要当场报。
    #[test]
    fn font_clamps_numbers_and_refuses_wrong_types() {
        let h = host();
        run(
            &h,
            r##"
            App.ui.style:font("mono", { size = 999, weight = 2000, italic = true })
            "##,
        );
        let over = h.style_overrides();
        let (_, patch) = &over.fonts()[0];
        assert_eq!(patch.size, Some(999.0), "存的是写的值，夹取在盖上去时做");
        assert_eq!(patch.weight, Some(900), "字重收到就夹");
        assert_eq!(patch.italic, Some(true));

        let text = fails(&h, r##"App.ui.style:font("mono", { size = "big" })"##);
        assert!(text.contains("size"), "{text}");
        let text = fails(&h, r##"App.ui.style:font("mono", { sise = 12 })"##);
        assert!(text.contains("sise"), "{text}");
        let text = fails(&h, r##"App.ui.style:font("nope", { size = 12 })"##);
        assert!(text.contains("nope"), "{text}");
    }
}
