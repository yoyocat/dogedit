//! 包：脚本的容器。
//!
//! 一个包 = 一个目录 + 一份可选的 `package.toml`。清单只回答五件事：
//! **我是谁**（`id` / `name` / `version`）、**我要什么版本**（`api`）、
//! **我依赖谁**（`requires`）、**按什么顺序跑**（`scripts` / `order`）、
//! **什么时候跑**（`launch`：写了就是点菜单才启动的按需包）。
//!
//! ## 这里没有的东西
//!
//! 没有 `kind`，没有 `[facets]`，没有 `[permissions]`。理由很简单：
//! 一张「哪几类包能用哪些 API」的表，每加一个 API 就要扩一行，而大部分格子的
//! 正确答案是「都能用」。真正的约束在**回调签名**上（绘制回调必须纯、元素事件
//! 回调返回意图），那两条跟包是谁无关。
//!
//! 所以清单里出现 `kind` / `facets` / `permissions` 会**报错**而不是忽略 ——
//! 从旧示例抄过来的作者需要知道这几段已经没了，静默忽略会让他以为分级还在。
//!
//! ## 这个模块不碰 Lua
//!
//! 纯数据：读文件、排顺序、写报告。不建窗口、不跑脚本、不碰注册表。
//! 所以它能在开窗之前跑完（`--packages` 就是这么用的），也能拿一段字符串单独测。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// 本实现认识的 `package.toml` 版本。
///
/// 只有这一个值：清单格式还在改，没有历史版本要兼容。api 对不上就是拒绝，
/// 而不是「尽力读读看」—— 读一半的清单比不读更难查。
pub const API_VERSION: u32 = 1;

/// 宿主版本。`requires = ["dogedit >= 0.9"]` 拿它比。
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 清单里那几段**已经删掉**的键。写出来是为了给一句人话，不是为了兼容。
const GONE_KEYS: [(&str, &str); 3] = [
    (
        "kind",
        "包不再分级：没有 skin / widget / extension / bundle 之分，什么脚本都能建元素、画画、改文档",
    ),
    (
        "facets",
        "不再按面声明文件：目录里的每个 *.lua 都会跑，要顺序就写 scripts = [\"a.lua\", \"b.lua\"]",
    ),
    (
        "permissions",
        "不再限制包能做什么；真正的约束在回调签名上（画法必须纯、元素事件返回意图）",
    ),
];

// ---------------------------------------------------------------------------
// 依赖
// ---------------------------------------------------------------------------

/// 一条依赖：`acme.base >= 1.2` 或 `dogedit >= 0.9`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Require {
    /// 被依赖的 id；`dogedit` 是宿主本身。
    pub id: String,
    /// 最低版本，`None` 表示不挑版本。
    pub min: Option<String>,
}

impl Require {
    /// 解析 `"acme.base >= 1.2"`。只认 `>=`，别的比较符一概不收 ——
    /// 多支持一个符号就多一种「为什么这个包装不上」的解释。
    pub fn parse(text: &str) -> Result<Require, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("空字符串".to_string());
        }
        let (id, min) = match text.split_once(">=") {
            Some((id, min)) => {
                let min = min.trim();
                if min.is_empty() {
                    return Err("`>=` 后面没写版本".to_string());
                }
                (id.trim(), Some(min.to_string()))
            }
            None => (text, None),
        };
        if id.is_empty() {
            return Err("没写包名".to_string());
        }
        if id.contains(char::is_whitespace) {
            return Err(format!("{id:?} 里有空格；写法是 `包名 >= 版本`"));
        }
        Ok(Require {
            id: id.to_string(),
            min,
        })
    }

    /// 宿主/被依赖方的版本够不够。
    pub fn satisfied_by(&self, version: &str) -> bool {
        match &self.min {
            None => true,
            Some(min) => version_ge(version, min),
        }
    }
}

/// `a >= b`？按点分段比数字，缺的段按 0 算。
///
/// 不引 semver：这里只有 `1.2` 和 `0.9.3` 这种，多一个依赖不值得。
pub fn version_ge(a: &str, b: &str) -> bool {
    let (Some(pa), Some(pb)) = (version_parts(a), version_parts(b)) else {
        // 有一边不是版本号：比不了就不拦，让包自己跑起来报错更容易查。
        return true;
    };
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    true
}

fn version_parts(text: &str) -> Option<Vec<u32>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for seg in text.split('.') {
        // 允许 `1.2.0-beta`：只取数字前缀，后缀忽略。
        let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        out.push(digits.parse().ok()?);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// 清单
// ---------------------------------------------------------------------------

/// 一份读懂的 `package.toml`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageManifest {
    pub api: u32,
    pub id: String,
    /// 显示名；空则用 `id`。
    pub name: String,
    pub version: String,
    pub description: String,
    pub requires: Vec<Require>,
    /// 同依赖层内的装载顺序，小的先装。
    pub order: i64,
    /// 要跑的脚本，**相对包目录**。空 = 目录下所有 `*.lua`，按文件名排序。
    ///
    /// 解析期就定下来（不写的话在这里扫目录填好），所以下游拿到的永远是一份
    /// 具体清单 —— 「哪些文件会跑」是个能打印出来的事实，不是一条隐式规则。
    pub scripts: Vec<String>,
    /// 非空 = **按需启动包**：开机不跑脚本，只在「扩展」菜单放一行，文字就是
    /// 这个值；点它才装载，关掉它带的窗格就整包卸载。空 = 传统的开机装载包。
    ///
    /// 用一行文字而不是 `true`：菜单总得有个标题，多一个 `menu = "..."` 字段
    /// 只会多出「写了 menu 忘了写 on_demand」这种组合。
    pub launch: String,
}

impl PackageManifest {
    /// 显示名，没写就是 id。
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// 开机要不要跑这个包的脚本。
    pub fn is_on_demand(&self) -> bool {
        !self.launch.is_empty()
    }

    /// 「扩展」菜单里那一行的文字。没写 `launch` 的包不上这张菜单，这里退回
    /// 显示名只是让调用方不必再判一次空。
    pub fn menu_label(&self) -> &str {
        if self.launch.is_empty() {
            self.display_name()
        } else {
            &self.launch
        }
    }
}

/// 清单读不通的原因。每种都有话直说 —— 作者是拿记事本手写的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageError {
    /// 文件读不出来。
    Io { path: String, message: String },
    /// TOML 本身不合法。
    Toml { line: Option<usize>, message: String },
    /// `id` 不合规。
    BadId { id: String, why: String },
    /// `api` 不是本实现认识的那个。
    Api { found: i64, want: u32 },
    /// 依赖项写不通。
    BadRequire { text: String, why: String },
    /// 某个键的类型不对。
    BadType { key: String, want: &'static str },
    /// 声明了但磁盘上没有。
    MissingScript { name: String },
    /// 一个脚本都没有。
    NoScripts,
    /// 写了已经删掉的段。
    Gone { key: &'static str, why: &'static str },
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageError::Io { path, message } => write!(f, "读不了 {path}：{message}"),
            PackageError::Toml { line, message } => match line {
                Some(n) => write!(f, "第 {n} 行不是合法 TOML：{message}"),
                None => write!(f, "不是合法 TOML：{message}"),
            },
            PackageError::BadId { id, why } => write!(f, "包 id {id:?} 不合法：{why}"),
            PackageError::Api { found, want } => write!(
                f,
                "清单写的是 api = {found}，这个版本只认识 api = {want}"
            ),
            PackageError::BadRequire { text, why } => {
                write!(f, "依赖项 {text:?} 不合法：{why}")
            }
            PackageError::BadType { key, want } => write!(f, "{key} 要写 {want}"),
            PackageError::MissingScript { name } => {
                write!(f, "声明了脚本 {name}，但包目录里没有这个文件")
            }
            PackageError::NoScripts => write!(
                f,
                "这个包里一个 .lua 都没有 —— 包就是脚本的容器，没有脚本就没有包"
            ),
            PackageError::Gone { key, why } => {
                write!(f, "`{key}` 已经删掉了：{why}")
            }
        }
    }
}

impl std::error::Error for PackageError {}

/// `id` 的规则：小写字母或数字开头，只含 `[a-z0-9._-]`，≤64 字符，不含 `..`。
///
/// 它会变成目录名、命令前缀、storage 目录名 —— 所以不能有惊喜（大写、空格、
/// 路径分隔符都会在不同的地方咬人）。
fn check_id(id: &str) -> Result<(), PackageError> {
    let bad = |why: &str| PackageError::BadId {
        id: id.to_string(),
        why: why.to_string(),
    };
    if id.is_empty() {
        return Err(bad("不能是空的"));
    }
    if id.len() > 64 {
        return Err(bad("超过 64 个字符"));
    }
    if id.contains("..") {
        return Err(bad("不能含 `..`（它会变成目录名）"));
    }
    let mut chars = id.chars();
    let first = chars.next().expect("非空");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(bad("首字符要小写字母或数字"));
    }
    for c in id.chars() {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')) {
            return Err(bad("只能用 a-z 0-9 . _ -"));
        }
    }
    Ok(())
}

/// 脚本名必须是包目录里的相对路径，不许跳出去。
fn check_script(name: &str) -> Result<(), PackageError> {
    let bad = |why: &'static str| PackageError::BadType {
        key: format!("scripts 里的 {name:?}"),
        want: why,
    };
    if name.is_empty() {
        return Err(bad("不能是空字符串"));
    }
    if name.contains("..") {
        return Err(bad("不能含 `..`"));
    }
    if name.contains('\\') {
        return Err(bad("用 `/` 分隔，不要用 `\\`"));
    }
    if name.starts_with('/') || name.contains(':') {
        return Err(bad("要是相对包目录的路径"));
    }
    Ok(())
}

/// 读一份清单。
///
/// `dir` 用来做两件事：把相对脚本名解析成真路径（好检查存在），以及
/// 「没写 `scripts` 时扫目录里的 `*.lua`」。
pub fn parse_manifest(text: &str, dir: &Path) -> Result<PackageManifest, PackageError> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| PackageError::Toml {
        line: e.span().map(|s| text[..s.start].lines().count()),
        message: e.message().to_string(),
    })?;

    // 先认「已经删掉的段」：从旧示例抄过来的作者最需要这句。
    for (key, why) in GONE_KEYS {
        if table.contains_key(key) {
            return Err(PackageError::Gone { key, why });
        }
    }

    let api = match table.get("api") {
        None => API_VERSION as i64,
        Some(v) => match v.as_integer() {
            Some(n) => n,
            None => {
                return Err(PackageError::BadType {
                    key: "api".to_string(),
                    want: "整数",
                });
            }
        },
    };
    if api != API_VERSION as i64 {
        return Err(PackageError::Api {
            found: api,
            want: API_VERSION,
        });
    }

    let id = match table.get("id") {
        Some(v) => match v.as_str() {
            Some(s) => s.to_string(),
            None => {
                return Err(PackageError::BadType {
                    key: "id".to_string(),
                    want: "字符串",
                });
            }
        },
        None => {
            return Err(PackageError::BadType {
                key: "id".to_string(),
                want: "字符串（必填）",
            });
        }
    };
    check_id(&id)?;

    let string_of = |key: &str| -> Result<String, PackageError> {
        match table.get(key) {
            None => Ok(String::new()),
            Some(v) => v.as_str().map(str::to_string).ok_or(PackageError::BadType {
                key: key.to_string(),
                want: "字符串",
            }),
        }
    };
    let name = string_of("name")?;
    let version = string_of("version")?;
    let description = string_of("description")?;
    // 写了 `launch` 就是按需包。空串/纯空白报错而不是当成「不按需」：
    // 一行没有文字的菜单项点不着，而静默退回开机装载更糟 —— 作者以为自己
    // 关掉了自动装载，结果脚本照跑。
    let launch = string_of("launch")?;
    if table.contains_key("launch") && launch.trim().is_empty() {
        return Err(PackageError::BadType {
            key: "launch".to_string(),
            want: "非空的菜单行文字（不写这一行就是开机装载）",
        });
    }

    let order = match table.get("order") {
        None => 0,
        Some(v) => v.as_integer().ok_or(PackageError::BadType {
            key: "order".to_string(),
            want: "整数",
        })?,
    };

    let mut requires = Vec::new();
    match table.get("requires") {
        None => {}
        Some(v) => {
            let Some(list) = v.as_array() else {
                return Err(PackageError::BadType {
                    key: "requires".to_string(),
                    want: "字符串数组，比如 [\"acme.base >= 1.2\"]",
                });
            };
            for item in list {
                let Some(s) = item.as_str() else {
                    return Err(PackageError::BadType {
                        key: "requires 里的项".to_string(),
                        want: "字符串",
                    });
                };
                let req = Require::parse(s).map_err(|why| PackageError::BadRequire {
                    text: s.to_string(),
                    why,
                })?;
                if req.id == id {
                    return Err(PackageError::BadRequire {
                        text: s.to_string(),
                        why: "不能依赖自己".to_string(),
                    });
                }
                requires.push(req);
            }
        }
    }

    // 脚本：写了就按写的顺序，没写就扫目录。两条都要落到一份**具体清单**上。
    let mut scripts: Vec<String> = match table.get("scripts") {
        None => scan_lua(dir),
        Some(v) => {
            let Some(list) = v.as_array() else {
                return Err(PackageError::BadType {
                    key: "scripts".to_string(),
                    want: "字符串数组，比如 [\"init.lua\"]",
                });
            };
            let mut out = Vec::new();
            for item in list {
                let Some(s) = item.as_str() else {
                    return Err(PackageError::BadType {
                        key: "scripts 里的项".to_string(),
                        want: "字符串",
                    });
                };
                check_script(s)?;
                out.push(s.to_string());
            }
            out
        }
    };
    if scripts.is_empty() {
        return Err(PackageError::NoScripts);
    }
    // 声明的必须真在：硬引用，不猜。「少了一个文件」比「少跑一段」好查得多。
    for s in &scripts {
        if !dir.join(s).is_file() {
            return Err(PackageError::MissingScript { name: s.clone() });
        }
    }
    // 扫出来的列表已经是排好序的；写出来的保持作者顺序，只去掉重复。
    if table.get("scripts").is_some() {
        let mut seen = BTreeSet::new();
        scripts.retain(|s| seen.insert(s.clone()));
    }

    Ok(PackageManifest {
        api: API_VERSION,
        id,
        name,
        version,
        description,
        requires,
        order,
        scripts,
        launch,
    })
}

/// 目录里的 `*.lua`，按文件名排序。目录不存在就是空。
fn scan_lua(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "lua") {
                Some(path.file_name()?.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// 状态与实例
// ---------------------------------------------------------------------------

/// 一个包走到哪一步了。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// 扫到了，还没解析。
    Discovered,
    /// 清单读通了、依赖排得开。
    Validated,
    /// 脚本跑完了。
    Loaded,
    /// 正在工作。
    Active,
    /// 用户关掉了。
    Disabled,
    /// 装载失败，已回滚。
    Failed(String),
    /// 根本没装：清单/依赖/版本不合格。
    Rejected(String),
    /// 同 id 被后面的根目录覆盖了。
    Ignored,
}

impl State {
    pub fn as_str(&self) -> &str {
        match self {
            State::Discovered => "discovered",
            State::Validated => "validated",
            State::Loaded => "loaded",
            State::Active => "active",
            State::Disabled => "disabled",
            State::Failed(_) => "failed",
            State::Rejected(_) => "rejected",
            State::Ignored => "ignored",
        }
    }

    /// 这个状态是不是「不干活」。
    pub fn is_idle(&self) -> bool {
        matches!(
            self,
            State::Disabled | State::Failed(_) | State::Rejected(_) | State::Ignored
        )
    }
}

/// 包从哪来的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// `<exe>/packages/`：随程序发布，只读。
    Bundled,
    /// `%LOCALAPPDATA%\DogEdit\packages\`：用户装的，可写。
    User,
    /// 工作目录的 `packages/`：正在开发的那一份（仓库里 `cargo run` 就是它）。
    /// 排在最后，所以它覆盖上面两个 —— 正在改的应该赢。
    Development,
    /// `<exe>/scripts/*.lua`：还没打包的散装脚本。
    Loose,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Bundled => "bundled",
            Origin::User => "user",
            Origin::Development => "dev",
            Origin::Loose => "loose",
        }
    }
}

/// 一个装上的包。
#[derive(Clone, Debug)]
pub struct Package {
    /// 包目录；散装脚本是它所在的目录。
    pub dir: PathBuf,
    pub manifest: PackageManifest,
    pub state: State,
    pub origin: Origin,
}

impl Package {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// 开机不装载，等用户点菜单。
    pub fn is_on_demand(&self) -> bool {
        self.manifest.is_on_demand()
    }

    /// 「扩展」菜单里那一行的文字。
    pub fn menu_label(&self) -> &str {
        self.manifest.menu_label()
    }

    /// 要跑的脚本，已经是绝对路径。
    pub fn script_paths(&self) -> Vec<PathBuf> {
        self.manifest
            .scripts
            .iter()
            .map(|s| self.dir.join(s))
            .collect()
    }
}

/// 扫出来的一条：成了就是 `Package`，没成就是一句话。
#[derive(Clone, Debug)]
pub struct Scanned {
    pub id: String,
    pub dir: PathBuf,
    pub origin: Origin,
    pub result: Result<Package, String>,
}

// ---------------------------------------------------------------------------
// 发现
// ---------------------------------------------------------------------------

/// 包的三个根目录，**后面的覆盖前面的**。
///
/// 第三个是工作目录：仓库里 `cargo run` 时 exe 在 `target/debug/`，随包发布的
/// 那份 `packages/` 不在它旁边 —— 和 `themes/` 同一个口径（exe 旁优先，再是
/// 工作目录）。放最后是因为「正在改的那一份应该赢」。
pub fn roots() -> Vec<(PathBuf, Origin)> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push((dir.join("packages"), Origin::Bundled));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push((
            PathBuf::from(local).join("DogEdit").join("packages"),
            Origin::User,
        ));
    }
    out.push((PathBuf::from("packages"), Origin::Development));
    out
}

/// 扫一个根目录下的所有包。每个子目录里要有 `package.toml` 才算候选。
pub fn scan_root(root: &Path, origin: Origin) -> Vec<Scanned> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest = dir.join("package.toml");
        if !manifest.is_file() {
            continue;
        }
        let fallback = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&manifest) {
            Ok(t) => t,
            Err(e) => {
                out.push(Scanned {
                    id: fallback,
                    dir,
                    origin,
                    result: Err(
                        PackageError::Io {
                            path: manifest.display().to_string(),
                            message: e.to_string(),
                        }
                        .to_string(),
                    ),
                });
                continue;
            }
        };
        let result = parse_manifest(&text, &dir)
            .map(|m| Package {
                dir: dir.clone(),
                manifest: m,
                state: State::Validated,
                origin,
            })
            .map_err(|e| e.to_string());
        let id = match &result {
            Ok(pkg) => pkg.id().to_string(),
            Err(_) => fallback,
        };
        out.push(Scanned {
            id,
            dir,
            origin,
            result,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 扫全部根目录，按 id 去重（**后出现的根胜**）。
///
/// 被覆盖的那份不会消失，它变成 `State::Ignored` —— 管理器要能回答「我改的
/// 为什么没生效」，答案就是这里。
pub fn discover(roots: &[(PathBuf, Origin)]) -> Vec<Scanned> {    let mut all: Vec<Scanned> = Vec::new();
    for (root, origin) in roots {
        all.extend(scan_root(root, *origin));
    }
    // 每个 id 只留**最后一个**成功的那份（后出现的根胜）。
    let mut winner: BTreeMap<String, usize> = BTreeMap::new();
    for (i, item) in all.iter().enumerate() {
        if item.result.is_ok() {
            winner.insert(item.id.clone(), i);
        }
    }
    for (i, item) in all.iter_mut().enumerate() {
        if item.result.is_ok()
            && winner.get(&item.id).copied() != Some(i)
            && let Ok(pkg) = &mut item.result
        {
            pkg.state = State::Ignored;
        }
    }
    // 生效的排在前面，同 id 的「被覆盖」那份紧跟其后。
    all.sort_by(|a, b| {
        a.id
            .cmp(&b.id)
            .then(b.result.is_ok().cmp(&a.result.is_ok()))
    });
    all
}

/// 这台机器上能看到的全部包和散装脚本，按 id 去重。
///
/// 三个调用方（启动装载、`--ui-doctor`、管理器）问的都是同一个问题，所以「扫哪
/// 几个根、按什么顺序」只写一遍 —— 三份拷贝迟早有一份忘了加 `loose_roots`，
/// 然后「诊断里有、管理器里没有」这种分歧就出现了。
///
/// 散装脚本也在这份里（`Origin::Loose`），要分开看的调用方按 origin 筛。
pub fn scan_all() -> Vec<Scanned> {
    let mut all = discover(&roots());
    for root in loose_roots() {
        all.extend(scan_loose(&root));
    }
    all.sort_by(|a, b| {
        a.id
            .cmp(&b.id)
            .then(b.result.is_ok().cmp(&a.result.is_ok()))
    });
    all
}

// ---------------------------------------------------------------------------
// 排序
// ---------------------------------------------------------------------------

/// 把包按依赖排成一条装载顺序，返回排好的包和被拒的包。
///
/// 规则：
///
/// 1. 依赖不存在的包 → 拒绝；
/// 2. 有环的包 → **整环**拒绝，报出环上的 id；
/// 3. 其余按「依赖层 + `order` + id」排。
///
/// 层用最长路径算，不用深度优先的完成序 —— 后者对 `a → b, c → b` 这种菱形
/// 会给出一个看着对、换台机器就变的顺序。
///
/// 没有停用名单的版本，见 [`sort_with`]。
pub fn sort(packages: Vec<Package>, host: &str) -> (Vec<Package>, Vec<Package>) {
    let (live, _, bad) = sort_with(packages, host, &BTreeSet::new());
    (live, bad)
}

/// 和 [`sort`] 一样，但先把用户停用的包摘出去。
///
/// 停用的包**不是坏包**：它们不进 `bad`（那会被报成装载失败，用户以为自己写错
/// 了什么），单独返回一份，状态是 [`State::Disabled`]。被它连累的包仍然进
/// `bad`，但原因写「依赖的 `x` 已停用」—— 说「没装成」会让人去翻清单。
///
/// `off` 只按 id 比：包目录可以有好几个根，同一个 id 的哪一份都算停用。
pub fn sort_with(
    packages: Vec<Package>,
    host: &str,
    off: &BTreeSet<String>,
) -> (Vec<Package>, Vec<Package>, Vec<Package>) {
    let mut bad: Vec<Package> = Vec::new();
    let mut off_list: Vec<Package> = Vec::new();

    // 第一轮：停用与宿主版本。缺依赖不在这里判 —— 被依赖的那个可能自己也出局，
    // 那时这个包的报错该指向真正的原因，而不是「找不到 a.b」。
    let mut pool: Vec<Package> = Vec::new();
    for mut pkg in packages {
        if off.contains(pkg.id()) {
            pkg.state = State::Disabled;
            off_list.push(pkg);
            continue;
        }
        if let Some(req) = pkg
            .manifest
            .requires
            .iter()
            .find(|r| r.id != "dogedit" && off.contains(&r.id))
        {
            pkg.state = State::Rejected(format!("依赖的 `{}` 已停用", req.id));
            bad.push(pkg);
            continue;
        }
        if let Some(req) = pkg.manifest.requires.iter().find(|r| r.id == "dogedit")
            && !req.satisfied_by(host)
        {
            pkg.state = State::Rejected(format!(
                "需要 dogedit >= {}，当前是 {host}",
                req.min.as_deref().unwrap_or("?")
            ));
            bad.push(pkg);
            continue;
        }
        pool.push(pkg);
    }

    // 第二轮：建图。`deps[x]` = x 依赖的包，`edges[x]` = 依赖 x 的包；
    // 两者都只算池子里有的 id（宿主 `dogedit` 不参与拓扑）。
    let present: BTreeSet<String> = pool.iter().map(|p| p.id().to_string()).collect();
    let mut indeg: BTreeMap<String, usize> = BTreeMap::new();
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pkg in &pool {
        let id = pkg.id().to_string();
        indeg.entry(id.clone()).or_insert(0);
        edges.entry(id.clone()).or_default();
        let mut mine = Vec::new();
        for req in &pkg.manifest.requires {
            if req.id == "dogedit" || !present.contains(&req.id) {
                continue;
            }
            edges
                .entry(req.id.clone())
                .or_default()
                .push(id.clone());
            *indeg.entry(id.clone()).or_insert(0) += 1;
            mine.push(req.id.clone());
        }
        deps.insert(id, mine);
    }

    let mut ready: Vec<String> = indeg
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| id.clone())
        .collect();
    ready.sort();
    let mut order: Vec<String> = Vec::new();
    while !ready.is_empty() {
        // 从队头取，让同一层的包按 id 序出列 —— 顺序稳定，测试才写得准。
        let id = ready.remove(0);
        order.push(id.clone());
        let mut next: Vec<String> = edges.get(&id).cloned().unwrap_or_default();
        next.sort();
        for child in next {
            if let Some(n) = indeg.get_mut(&child) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    ready.push(child);
                }
            }
        }
        ready.sort();
    }

    // 第三轮：分类。Kahn 之后还有入度的，要么自己在环上，要么依赖了环上的包。
    // 两者要分开报 —— 「你依赖的 a.b 没装成」和「a.b 与 a.c 互相依赖」是两种
    // 完全不同的修法。
    let stuck: Vec<String> = present
        .iter()
        .filter(|id| !order.contains(id))
        .cloned()
        .collect();
    let mut live: Vec<Package> = Vec::new();
    for mut pkg in pool {
        let id = pkg.id().to_string();
        if !stuck.contains(&id) {
            live.push(pkg);
            continue;
        }
        if on_cycle(&id, &edges, &stuck) {
            let ring = describe_cycle(&id, &edges, &stuck);
            pkg.state = State::Rejected(format!("循环依赖：{ring}"));
        } else {
            let dep = deps
                .get(&id)
                .and_then(|d| d.iter().find(|d| stuck.contains(d)).cloned());
            pkg.state = State::Rejected(match dep {
                Some(dep) => format!("依赖的 `{dep}` 没装成"),
                None => "依赖关系没排出来".to_string(),
            });
        }
        bad.push(pkg);
    }

    // 第四轮：按依赖层排。层 = 从「没有依赖的包」往下数的最长链。用最长链而
    // 不是深度优先的完成序：菱形依赖（a → b、a → c、b/c → d）下完成序会随
    // 遍历方向变，最长链不会。
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    for id in &order {
        let d = deps
            .get(id)
            .map(|ds| {
                ds.iter()
                    .filter_map(|d| depth.get(d).copied())
                    .max()
                    .unwrap_or(0)
                    + 1
            })
            .unwrap_or(0);
        depth.insert(id.clone(), d);
    }
    live.sort_by(|a, b| {
        let da = depth.get(a.id()).copied().unwrap_or(0);
        let db = depth.get(b.id()).copied().unwrap_or(0);
        da.cmp(&db)
            .then(a.manifest.order.cmp(&b.manifest.order))
            .then(a.id().cmp(b.id()))
    });
    // 排进来的就是校验过的：清单读过、依赖排得开。再往后是装载。
    for pkg in &mut live {
        if pkg.state == State::Discovered {
            pkg.state = State::Validated;
        }
    }
    (live, off_list, bad)
}

/// `start` 在不在环上？在 `stuck` 这几个节点内部走一遍，看能不能走回自己。
///
/// 只走 `stuck` 里的节点：已经出列的那部分图是干净的，走进去会绕远路。
fn on_cycle(start: &str, edges: &BTreeMap<String, Vec<String>>, stuck: &[String]) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut stack: Vec<String> = edges
        .get(start)
        .map(|v| v.iter().filter(|c| stuck.contains(c)).cloned().collect())
        .unwrap_or_default();
    while let Some(id) = stack.pop() {
        if id == start {
            return true;
        }
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        if let Some(next) = edges.get(&id) {
            for child in next {
                if stuck.contains(child) {
                    stack.push(child.clone());
                }
            }
        }
    }
    false
}

/// 给环写一句话：`a → b → c → a`。只走第一条能闭合的路径，不追求列全 ——
/// 报错是要人看懂怎么改，不是要一份完整报告。
fn describe_cycle(start: &str, edges: &BTreeMap<String, Vec<String>>, stuck: &[String]) -> String {
    let mut path = vec![start.to_string()];
    let mut cur = start.to_string();
    for _ in 0..stuck.len() + 1 {
        let Some(next) = edges
            .get(&cur)
            .and_then(|v| v.iter().find(|c| stuck.contains(c)))
            .cloned()
        else {
            break;
        };
        if next == start {
            path.push(start.to_string());
            break;
        }
        if let Some(at) = path.iter().position(|p| *p == next) {
            // 走进了一个更小的环：把已经走过的部分截掉，只留这一圈。
            path.drain(..at);
            path.push(next);
            break;
        }
        path.push(next.clone());
        cur = next;
    }
    path.join(" → ")
}

// ---------------------------------------------------------------------------
// 散装脚本
// ---------------------------------------------------------------------------

/// 散装脚本的位置：`<exe>/scripts`，再是工作目录的 `scripts/`。
///
/// 它不是「兼容旧格式」，是「还没打包的脚本」：一个文件就是全部内容，不需要
/// id、不需要目录、不需要清单。写完直接跑，跑通了再考虑要不要变成包。
pub fn loose_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push(dir.join("scripts"));
    }
    out.push(PathBuf::from("scripts"));
    out
}

/// 扫一个散装目录：每个 `*.lua` 一个「包」。
pub fn scan_loose(root: &Path) -> Vec<Scanned> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let file = entry.path();
        if !file.is_file() || file.extension().is_none_or(|e| e != "lua") {
            continue;
        }
        let name = file
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let id = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(Scanned {
            id: id.clone(),
            dir: root.to_path_buf(),
            origin: Origin::Loose,
            result: Ok(Package {
                dir: root.to_path_buf(),
                manifest: PackageManifest {
                    api: API_VERSION,
                    name: id.clone(),
                    id,
                    version: String::new(),
                    description: String::new(),
                    requires: Vec::new(),
                    order: 0,
                    scripts: vec![name],
                    launch: String::new(),
                },
                state: State::Validated,
                origin: Origin::Loose,
            }),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// ---------------------------------------------------------------------------
// 报告
// ---------------------------------------------------------------------------

/// `--packages` 打的那张表。
///
/// 行序**就是装载顺序**：先跑一遍 [`sort`]，所以 `state` 那一列是有意义的结果，
/// 不是「扫到了」的同义词。原因按行 1:1 摊在表下面，用 id 当锚点。
pub fn report(scanned: &[Scanned], loose: &[Scanned]) -> String {
    let mut live: Vec<Package> = Vec::new();
    let mut rejected: Vec<Package> = Vec::new();
    let mut broken: Vec<Scanned> = Vec::new();
    for item in scanned {
        match &item.result {
            Ok(pkg) => {
                if pkg.state == State::Ignored {
                    rejected.push(pkg.clone());
                } else {
                    live.push(pkg.clone());
                }
            }
            Err(_) => broken.push(item.clone()),
        }
    }
    let (live, mut bad) = sort(live, HOST_VERSION);
    bad.append(&mut rejected);
    bad.sort_by(|a, b| a.id().cmp(b.id()));
    broken.sort_by(|a, b| a.id.cmp(&b.id));

    let mut out = String::new();
    let total = scanned.len();
    let ok = live.len();
    // 按需包也在 `live` 里（清单是好的、依赖排得开），但它们开机不跑。
    // 没有按需包时保持原来那行短报告，别让旧脚本/测试平白多一段文字。
    let auto = live.iter().filter(|p| !p.is_on_demand()).count();
    if auto == ok {
        out.push_str(&format!(
            "{} package(s): {ok} live, {} not loaded; {} loose script(s)\n",
            total,
            total - ok,
            loose.len()
        ));
    } else {
        out.push_str(&format!(
            "{} package(s): {ok} live ({auto} at startup, {} on demand), {} not loaded; {} loose script(s)\n",
            total,
            ok - auto,
            total - ok,
            loose.len()
        ));
    }
    if total == 0 && loose.is_empty() {
        out.push_str("(没有 packages/ 也没有 scripts/ 目录)\n");
        return out;
    }

    // 一行 = 六个格子 + 一句可选的原因。先攒齐再打，否则原因会和行错位。
    let mut rows: Vec<([String; 6], Option<String>)> = Vec::new();
    for pkg in &live {
        rows.push((cells(pkg), None));
    }
    for pkg in &bad {
        let why = match &pkg.state {
            State::Rejected(w) | State::Failed(w) => Some(w.clone()),
            State::Ignored => Some("同 id 的用户版覆盖了这份".to_string()),
            _ => None,
        };
        rows.push((cells(pkg), why));
    }
    for item in &broken {
        rows.push((
            [
                item.id.clone(),
                "-".to_string(),
                "-".to_string(),
                "rejected".to_string(),
                item.origin.as_str().to_string(),
                item.dir.display().to_string(),
            ],
            item.result.as_ref().err().cloned(),
        ));
    }

    if !rows.is_empty() {
        out.push_str(&format!(
            "{:<26} {:<8} {:<30} {:<10} {:<8} {}\n",
            "id", "version", "scripts", "state", "origin", "dir"
        ));
        for (cells, _) in &rows {
            out.push_str(&format!(
                "{:<26} {:<8} {:<30} {:<10} {:<8} {}\n",
                cells[0], cells[1], cells[2], cells[3], cells[4], cells[5]
            ));
        }
        for (cells, why) in &rows {
            if let Some(why) = why {
                out.push_str(&format!("    ! {}: {why}\n", cells[0]));
            }
        }
    }

    // 表里那列 `validated` 对按需包容易读错成「开机就跑」，单独列一遍。
    let on_demand: Vec<&Package> = live.iter().filter(|p| p.is_on_demand()).collect();
    if !on_demand.is_empty() {
        out.push_str("\non demand (startup skips these; 扩展 menu launches them)\n");
        for pkg in on_demand {
            out.push_str(&format!("  {:<24} {}\n", pkg.id(), pkg.menu_label()));
        }
    }

    if !loose.is_empty() {
        out.push_str("\nloose (no manifest, commands stay global)\n");
        for item in loose {
            if let Ok(pkg) = &item.result {
                out.push_str(&format!(
                    "  {:<24} {:<30} {}\n",
                    pkg.id(),
                    pkg.manifest.scripts.join(","),
                    pkg.dir.display()
                ));
            }
        }
    }
    out
}

/// 一行六个格子。列宽固定，太长的列表截断加省略号 —— 一张表要能一眼看完。
fn cells(pkg: &Package) -> [String; 6] {
    [
        pkg.id().to_string(),
        if pkg.manifest.version.is_empty() {
            "-".to_string()
        } else {
            pkg.manifest.version.clone()
        },
        truncate(&pkg.manifest.scripts.join(","), 30),
        pkg.state.as_str().to_string(),
        pkg.origin.as_str().to_string(),
        pkg.dir.display().to_string(),
    ]
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{keep}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dg-pkg-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// 造一个包目录，写清单和文件，回它的路径。
    fn write_pkg(root: &Path, id: &str, manifest: &str, files: &[&str]) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).expect("pkg dir");
        std::fs::write(dir.join("package.toml"), manifest).expect("manifest");
        for f in files {
            std::fs::write(dir.join(f), "").expect("script");
        }
        dir
    }

    // ── 清单 ──────────────────────────────────────────────────────────────

    #[test]
    fn a_minimal_manifest_parses() {
        let dir = tmp("min");
        let body = "api = 1\nid = \"acme.clock\"\nname = \"时钟\"\nversion = \"1.0.0\"\n";
        let p = write_pkg(&dir, "acme.clock", body, &["clock.lua"]);
        let m = parse_manifest(body, &p).expect("parses");
        assert_eq!(m.id, "acme.clock");
        assert_eq!(m.display_name(), "时钟");
        assert_eq!(m.version, "1.0.0");
        // 没写 `scripts`：目录里那个文件就是全部。
        assert_eq!(m.scripts, vec!["clock.lua"]);
        assert!(m.requires.is_empty());
        assert_eq!(m.order, 0);
        // 没写 `launch`：传统的开机装载包，菜单里不给它单独一行。
        assert!(!m.is_on_demand());
        assert_eq!(m.menu_label(), "时钟");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_declares_an_on_demand_package() {
        let dir = tmp("launch");
        let body = "api = 1\nid = \"snake\"\nname = \"贪吃蛇\"\nlaunch = \"开局(&S)\"\n";
        let p = write_pkg(&dir, "snake", body, &["snake.lua"]);
        let m = parse_manifest(body, &p).expect("parses");
        assert_eq!(m.launch, "开局(&S)");
        assert!(m.is_on_demand());
        assert_eq!(m.menu_label(), "开局(&S)", "菜单用 launch，不用 name");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_or_non_string_launch_is_rejected() {
        let dir = tmp("launchbad");
        for extra in [
            "launch = \"\"\n",
            "launch = \"   \"\n",
            "launch = true\n",
            "launch = 7\n",
        ] {
            let body = format!("api = 1\nid = \"a.b\"\n{extra}");
            let p = write_pkg(&dir, "a.b", &body, &["a.lua"]);
            let err = parse_manifest(&body, &p).expect_err("空/非字符串的 launch 不该通过");
            assert!(matches!(err, PackageError::BadType { .. }), "{extra:?}: {err}");
            assert!(err.to_string().contains("launch"), "{err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_name_falls_back_to_the_id() {
        let dir = tmp("noname");
        let body = "api = 1\nid = \"a.b\"\n";
        let p = write_pkg(&dir, "a.b", body, &["x.lua"]);
        let m = parse_manifest(body, &p).expect("parses");
        assert_eq!(m.display_name(), "a.b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scripts_default_to_every_lua_in_name_order() {
        let dir = tmp("order");
        let body = "api = 1\nid = \"a.b\"\n";
        let p = write_pkg(&dir, "a.b", body, &["z.lua", "a.lua", "m.lua", "notes.txt"]);
        let m = parse_manifest(body, &p).expect("parses");
        assert_eq!(m.scripts, vec!["a.lua", "m.lua", "z.lua"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_declared_script_list_keeps_the_author_order() {
        let dir = tmp("decl");
        let body = "api = 1\nid = \"a.b\"\nscripts = [\"z.lua\", \"a.lua\"]\n";
        let p = write_pkg(&dir, "a.b", body, &["z.lua", "a.lua", "m.lua"]);
        let m = parse_manifest(body, &p).expect("parses");
        assert_eq!(m.scripts, vec!["z.lua", "a.lua"], "写的顺序就是跑的顺序");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_declared_script_that_does_not_exist_is_an_error() {
        let dir = tmp("missing");
        let body = "api = 1\nid = \"a.b\"\nscripts = [\"ghost.lua\"]\n";
        let p = write_pkg(&dir, "a.b", body, &["real.lua"]);
        let err = parse_manifest(body, &p).expect_err("ghost must fail");
        assert!(matches!(err, PackageError::MissingScript { .. }), "{err}");
        assert!(err.to_string().contains("ghost.lua"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_without_any_lua_is_an_error() {
        let dir = tmp("empty");
        let body = "api = 1\nid = \"a.b\"\n";
        let p = write_pkg(&dir, "a.b", body, &[]);
        let err = parse_manifest(body, &p).expect_err("no lua");
        assert!(matches!(err, PackageError::NoScripts), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 从旧示例抄过来的清单会带 `kind` / `[facets]` / `[permissions]`。
    /// 这三段已经没了，**必须报错**而不是忽略 —— 静默忽略会让人以为分级还在。
    #[test]
    fn the_removed_sections_are_rejected_with_a_reason() {
        let dir = tmp("gone");
        for (extra, key) in [
            ("kind = \"skin\"\n", "kind"),
            ("[facets]\npaint = \"p.lua\"\n", "facets"),
            ("[permissions]\nassets = true\n", "permissions"),
        ] {
            let body = format!("api = 1\nid = \"a.b\"\n{extra}");
            let p = write_pkg(&dir, "a.b", &body, &["a.lua"]);
            let err = parse_manifest(&body, &p).expect_err("gone section");
            match err {
                PackageError::Gone { key: k, .. } => assert_eq!(k, key),
                other => panic!("{key} 应该报 Gone，实际是 {other}"),
            }
            let text = err.to_string();
            assert!(text.contains(key), "{text}");
            // 错误里要说清「那该怎么办」，不是只说「不行」。
            assert!(text.len() > 40, "只说「删了」不够：{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn any_api_but_ours_is_rejected() {
        let dir = tmp("api");
        for n in [2, 3, 9] {
            let body = format!("api = {n}\nid = \"a.b\"\n");
            let p = write_pkg(&dir, "a.b", &body, &["a.lua"]);
            let err = parse_manifest(&body, &p).expect_err("only api 1 is understood");
            let text = err.to_string();
            assert!(text.contains(&format!("api = {n}")), "{text}");
            assert!(text.contains("api = 1"), "{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_api_means_ours() {
        let dir = tmp("noapi");
        let body = "id = \"a.b\"\n";
        let p = write_pkg(&dir, "a.b", body, &["a.lua"]);
        let m = parse_manifest(body, &p).expect("parses");
        assert_eq!(m.api, API_VERSION);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_ids_are_rejected() {
        let dir = tmp("badid");
        for id in ["", "Acme", "acme/clock", "9ok", "a..b", "acme clock"] {
            let body = format!("api = 1\nid = \"{id}\"\n");
            let p = write_pkg(&dir, "pkg", &body, &["a.lua"]);
            let got = parse_manifest(&body, &p);
            if id == "9ok" {
                // 数字开头是合法的：id 会变成目录名，不会变成 Lua 标识符。
                assert!(got.is_ok(), "9ok 应该合法");
            } else {
                let err = got.expect_err(&format!("{id:?} 不该通过"));
                assert!(matches!(err, PackageError::BadId { .. }), "{err}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scripts_may_not_escape_the_package_dir() {
        let dir = tmp("escape");
        // 用 TOML 的字面量字符串（单引号）：`"a\b.lua"` 里的 `\b` 是转义字符，
        // 会让这条测试悄悄测到别的东西。
        for name in ["../evil.lua", "/abs.lua", r"a\b.lua", "C:/x.lua"] {
            let body = format!("api = 1\nid = \"a.b\"\nscripts = ['{name}']\n");
            let p = write_pkg(&dir, "a.b", &body, &["a.lua"]);
            let err = parse_manifest(&body, &p).expect_err(&format!("{name:?} 不该通过"));
            assert!(matches!(err, PackageError::BadType { .. }), "{name:?}: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // 前向兼容：新版本加的键不该让旧程序整份失败。
        let dir = tmp("extra");
        let body = "api = 1\nid = \"a.b\"\nhomepage = \"https://example.invalid\"\n";
        let p = write_pkg(&dir, "a.b", body, &["a.lua"]);
        let m = parse_manifest(body, &p).expect("extra keys must not fail the load");
        assert_eq!(m.id, "a.b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 依赖 ──────────────────────────────────────────────────────────────

    #[test]
    fn requires_parse_and_compare() {
        let r = Require::parse("acme.base >= 1.2").expect("parses");
        assert_eq!(r.id, "acme.base");
        assert_eq!(r.min.as_deref(), Some("1.2"));
        assert!(r.satisfied_by("1.2"));
        assert!(r.satisfied_by("1.10"), "1.10 比 1.2 新，不是字符串比较");
        assert!(r.satisfied_by("2.0"));
        assert!(!r.satisfied_by("1.1"));

        let bare = Require::parse("acme.base").expect("parses");
        assert!(bare.min.is_none());
        assert!(bare.satisfied_by("0.0.1"), "不挑版本就永远满足");

        for bad in ["", "a >= ", ">= 1.0", "a b"] {
            assert!(Require::parse(bad).is_err(), "{bad:?} 不该通过");
        }
    }

    #[test]
    fn a_package_may_not_require_itself() {
        let dir = tmp("selfreq");
        let body = "api = 1\nid = \"a.b\"\nrequires = [\"a.b >= 1\"]\n";
        let p = write_pkg(&dir, "a.b", body, &["a.lua"]);
        let err = parse_manifest(body, &p).expect_err("self dependency");
        assert!(matches!(err, PackageError::BadRequire { .. }), "{err}");
        assert!(err.to_string().contains("自己"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 发现与排序 ────────────────────────────────────────────────────────

    #[test]
    fn a_user_package_overrides_the_bundled_one_and_the_loser_is_kept() {
        let bundled = tmp("disc-b");
        let user = tmp("disc-u");
        for root in [&bundled, &user] {
            let p = write_pkg(
                root,
                "acme.mono",
                "api = 1\nid = \"acme.mono\"\nversion = \"1\"\n",
                &["a.lua"],
            );
            let _ = p;
        }
        let scanned = discover(&[
            (bundled.clone(), Origin::Bundled),
            (user.clone(), Origin::User),
        ]);
        assert_eq!(scanned.len(), 2, "两份都要留着");
        let live: Vec<&Scanned> = scanned
            .iter()
            .filter(|s| matches!(&s.result, Ok(p) if p.state != State::Ignored))
            .collect();
        assert_eq!(live.len(), 1, "只有一个生效");
        assert_eq!(live[0].origin, Origin::User, "后出现的根胜");
        let ignored = scanned
            .iter()
            .filter(|s| matches!(&s.result, Ok(p) if p.state == State::Ignored))
            .count();
        assert_eq!(ignored, 1, "输的那份标 Ignored，管理器要能回答「我改的为什么没生效」");
        let _ = std::fs::remove_dir_all(&bundled);
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 造一个只有清单的包（不落盘），给排序用。
    fn fake(id: &str, requires: &[(&str, Option<&str>)], order: i64) -> Package {
        Package {
            dir: PathBuf::from("."),
            manifest: PackageManifest {
                api: API_VERSION,
                id: id.to_string(),
                name: id.to_string(),
                version: "1".to_string(),
                description: String::new(),
                requires: requires
                    .iter()
                    .map(|(id, min)| Require {
                        id: id.to_string(),
                        min: min.map(str::to_string),
                    })
                    .collect(),
                order,
                scripts: vec!["a.lua".to_string()],
                launch: String::new(),
            },
            state: State::Validated,
            origin: Origin::Bundled,
        }
    }

    #[test]
    fn dependencies_come_before_their_dependents() {
        let pkgs = vec![
            fake("a.top", &[("a.mid", None)], 0),
            fake("a.mid", &[("a.base", None)], 0),
            fake("a.base", &[], 0),
        ];
        let (live, bad) = sort(pkgs, "1.0");
        assert!(bad.is_empty());
        let ids: Vec<&str> = live.iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["a.base", "a.mid", "a.top"]);
    }

    #[test]
    fn a_diamond_keeps_a_stable_order() {
        // a → b, a → c, b/c → d。完成序会随遍历方向变，最长链不会。
        let pkgs = vec![
            fake("a", &[("b", None), ("c", None)], 0),
            fake("b", &[("d", None)], 0),
            fake("c", &[("d", None)], 0),
            fake("d", &[], 0),
        ];
        let (live, _) = sort(pkgs, "1.0");
        let ids: Vec<&str> = live.iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["d", "b", "c", "a"]);
    }

    #[test]
    fn a_cycle_rejects_the_whole_ring_and_names_it() {
        let pkgs = vec![fake("a", &[("b", None)], 0), fake("b", &[("a", None)], 0)];
        let (live, bad) = sort(pkgs, "1.0");
        assert!(live.is_empty());
        assert_eq!(bad.len(), 2);
        for pkg in &bad {
            let State::Rejected(why) = &pkg.state else {
                panic!("{:?} 该是 Rejected", pkg.state);
            };
            assert!(why.contains("循环依赖"), "{why}");
            assert!(why.contains('→'), "要画出环：{why}");
        }
    }

    #[test]
    fn a_package_that_depends_on_a_stuck_one_says_so() {
        let pkgs = vec![
            fake("a", &[("b", None)], 0),
            fake("b", &[("c", None)], 0),
            fake("c", &[("b", None)], 0),
        ];
        let (_, bad) = sort(pkgs, "1.0");
        let a = bad.iter().find(|p| p.id() == "a").expect("a");
        let State::Rejected(why) = &a.state else {
            panic!("a 该是 Rejected");
        };
        assert!(why.contains("`b`"), "要指向真正的原因：{why}");
        assert!(!why.contains("循环"), "a 自己不在环上：{why}");
    }

    #[test]
    fn a_host_version_gate_rejects_early() {
        let pkgs = vec![fake("a", &[("dogedit", Some("9.0"))], 0)];
        let (live, bad) = sort(pkgs, "0.1.0");
        assert!(live.is_empty());
        assert!(bad[0].state.as_str() == "rejected");
        let State::Rejected(why) = &bad[0].state else {
            panic!("rejected");
        };
        assert!(why.contains("9.0") && why.contains("0.1.0"), "{why}");
    }

    #[test]
    fn order_breaks_ties_within_a_layer() {
        let pkgs = vec![fake("a.b", &[], 20), fake("a.a", &[], 10)];
        let (live, _) = sort(pkgs, "1.0");
        let ids: Vec<&str> = live.iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["a.a", "a.b"], "小的先装");
    }

    /// 停用的包既不装载也不报错：它进第三份，状态是 `Disabled`。
    #[test]
    fn a_disabled_package_is_neither_loaded_nor_a_failure() {
        let off: BTreeSet<String> = ["a.off".to_string()].into_iter().collect();
        let pkgs = vec![fake("a.off", &[], 0), fake("a.on", &[], 0)];
        let (live, disabled, bad) = sort_with(pkgs, "1.0", &off);
        assert_eq!(live.iter().map(|p| p.id()).collect::<Vec<_>>(), vec!["a.on"]);
        assert_eq!(disabled.iter().map(|p| p.id()).collect::<Vec<_>>(), vec!["a.off"]);
        assert_eq!(disabled[0].state, State::Disabled);
        assert!(bad.is_empty(), "停用不是坏包：{:?}", bad.len());
    }

    /// 依赖一个停用的包时，原因要写成「已停用」——说「没装成」会让人去翻清单。
    #[test]
    fn a_dependent_of_a_disabled_package_says_so() {
        let off: BTreeSet<String> = ["b".to_string()].into_iter().collect();
        let pkgs = vec![fake("a", &[("b", None)], 0), fake("b", &[], 0)];
        let (live, _, bad) = sort_with(pkgs, "1.0", &off);
        assert!(live.is_empty());
        assert_eq!(bad.len(), 1);
        let State::Rejected(why) = &bad[0].state else {
            panic!("a 该是 Rejected");
        };
        assert!(why.contains("`b`") && why.contains("已停用"), "{why}");
    }

    // ── 散装 ──────────────────────────────────────────────────────────────

    #[test]
    fn loose_scripts_become_one_package_each() {
        let dir = tmp("loose");
        for name in ["b.lua", "a.lua", "notes.txt"] {
            std::fs::write(dir.join(name), "").expect("write");
        }
        let got = scan_loose(&dir);
        assert_eq!(got.len(), 2, "只有 .lua 算");
        assert_eq!(got[0].id, "a", "按 id 排序");
        assert_eq!(got[0].origin, Origin::Loose);
        let pkg = got[0].result.as_ref().expect("ok");
        assert_eq!(pkg.manifest.scripts, vec!["a.lua"]);
        assert_eq!(pkg.script_paths(), vec![dir.join("a.lua")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_root_is_empty_not_an_error() {
        let got = scan_loose(Path::new("Z:\\no\\such\\dir"));
        assert!(got.is_empty());
    }

    // ── 报告 ──────────────────────────────────────────────────────────────

    #[test]
    fn the_report_is_in_load_order_and_explains_every_row() {
        let bundled = tmp("rep");
        write_pkg(
            &bundled,
            "a.top",
            "api = 1\nid = \"a.top\"\nversion = \"1\"\nrequires = [\"a.base\"]\n",
            &["a.lua"],
        );
        write_pkg(&bundled, "a.base", "api = 1\nid = \"a.base\"\n", &["a.lua"]);
        write_pkg(
            &bundled,
            "a.bad",
            "api = 1\nid = \"a.bad\"\nkind = \"skin\"\n",
            &["a.lua"],
        );
        let scanned = discover(&[(bundled.clone(), Origin::Bundled)]);
        let text = report(&scanned, &[]);

        assert!(text.contains("3 package(s): 2 live, 1 not loaded"), "{text}");
        // 行序 = 装载顺序：base 在 top 前面。
        let base = text.find("a.base").expect("a.base");
        let top = text.find("a.top").expect("a.top");
        assert!(base < top, "装载顺序反了：\n{text}");
        // 原因按 id 锚在表下，且说了为什么。
        assert!(text.contains("! a.bad:"), "{text}");
        assert!(text.contains("kind"), "{text}");
        let _ = std::fs::remove_dir_all(&bundled);
    }

    #[test]
    fn the_report_names_on_demand_packages_separately() {
        let root = tmp("rep-launch");
        write_pkg(
            &root,
            "a.lazy",
            "api = 1\nid = \"a.lazy\"\nlaunch = \"开局(&S)\"\n",
            &["a.lua"],
        );
        write_pkg(&root, "a.auto", "api = 1\nid = \"a.auto\"\n", &["a.lua"]);
        let scanned = discover(&[(root.clone(), Origin::Bundled)]);
        let text = report(&scanned, &[]);
        assert!(text.contains("1 at startup, 1 on demand"), "{text}");
        assert!(text.contains("on demand (startup skips these"), "{text}");
        assert!(text.contains("a.lazy"), "{text}");
        assert!(text.contains("开局(&S)"), "{text}");
        assert!(!text.contains("a.auto                  a.auto"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_machine_says_so_instead_of_printing_a_bare_header() {
        let text = report(&[], &[]);
        assert!(text.contains("0 package(s)"), "{text}");
        assert!(text.contains("没有 packages/"), "{text}");
    }

    #[test]
    fn loose_scripts_get_their_own_section() {
        let dir = tmp("rep-loose");
        std::fs::write(dir.join("fmt.lua"), "").expect("write");
        let loose = scan_loose(&dir);
        let text = report(&[], &loose);
        assert!(text.contains("1 loose script(s)"), "{text}");
        assert!(text.contains("loose (no manifest"), "{text}");
        assert!(text.contains("fmt"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
