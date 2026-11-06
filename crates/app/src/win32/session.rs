//! 会话状态：选项页「会话」那五项背后的实现。
//!
//! 五项各自独立，但共用一份状态目录（`[Session] TempFolder`，空则
//! `%LOCALAPPDATA%\DogEdit\session`）：
//!
//! ```text
//! <root>\session.ini      标签页清单 + 每文件的光标 / 书签
//! <root>\hot\<key>.dgab   热保存的正文（源编码原样字节，含 BOM）
//! <root>\hot\<key>.ini    热保存的元数据
//! ```
//!
//! | 选项 | ini 键 | 干什么 |
//! |---|---|---|
//! | 记忆曾经打开过的文件的光标位置 | `Session/Cursor` | 打开文件时光标回到上次离开的位置 |
//! | 启动时恢复上次退出前未关闭的文件 | `Session/File` | 退出时记下标签页清单，下次启动重开 |
//! | 记忆/恢复被修改的文件的内容(热保存) | `Session/SaveModified` | 退出时把脏文档抄一份，下次启动还原成带 `*` 的标签 |
//! | 记忆/恢复书签 | `Session/Marker` | 书签行跟着文件走 |
//! | 临时文件夹 | `Session/TempFolder` | 上面这些数据放哪 |
//!
//! 和自动备份（[`super::autobackup`]）的分工：那边是**崩溃兜底**，按间隔
//! 不停写、有自己的恢复对话框；这边是**退出时的快照**，只在退出那一刻写
//! 一次、下次启动静默还原。所以热保存的载荷不参与 `has_pending` 的判定，
//! 两个功能可以同时开。
//!
//! 退出那一刻的时序（`WM_CLOSE`）：先 [`State::capture`] 写清单和热保存，
//! 再走正常的「保存吗」逐份询问。用户在询问里选「是」，`save_file` 会把
//! 刚写出去的那份热保存删掉（内容已经落盘，留着下次会重复还原）；选
//! 「否」或未命名文档，热保存留着 —— 这正是「不丢内容」的含义。取消退出
//! 则整批删掉，回到原样。

#![allow(unsafe_op_in_unsafe_fn)]

use std::path::{Path, PathBuf};

use dg_core::Document;

use super::grepdlg::{ini_get, ini_load, ini_remove_section, ini_save, ini_set};

/// 光标 / 书签记忆最多记多少个文件；超了丢最久没碰过的那条。
const MEMORY_MAX: usize = 256;

/// 热保存的载荷留几天。启动时顺手清一次，免得关掉选项后永远堆着。
const HOT_KEEP_DAYS: u64 = 30;

/// 选项页那五项。
#[derive(Clone, Debug)]
pub(super) struct Settings {
    /// 记忆曾经打开过的文件的光标位置。
    pub cursor: bool,
    /// 启动时恢复上次退出前未关闭的文件。
    pub files: bool,
    /// 记忆/恢复被修改的文件的内容（热保存）。
    pub hot: bool,
    /// 记忆/恢复书签。
    pub markers: bool,
    /// 「自定义临时文件夹」；`None` 用默认根。
    pub temp_dir: Option<PathBuf>,
}

impl Settings {
    pub(super) fn load() -> Self {
        let ini = super::optionsdlg::IniText::load(&super::optionsdlg::ini_path());
        let temp = ini
            .get("Session", "TempFolder")
            .unwrap_or_default()
            .trim()
            .to_string();
        Self {
            cursor: read_bool(&ini, "Session", "Cursor", true),
            files: read_bool(&ini, "Session", "File", false),
            hot: read_bool(&ini, "Session", "SaveModified", false),
            markers: read_bool(&ini, "Session", "Marker", false),
            temp_dir: if temp.is_empty() {
                None
            } else {
                Some(PathBuf::from(temp))
            },
        }
    }

    /// 状态目录：设了临时文件夹就用它，否则 `%LOCALAPPDATA%\DogEdit\session`。
    pub(super) fn root(&self) -> PathBuf {
        self.temp_dir
            .clone()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(default_root)
    }
}

fn read_bool(ini: &super::optionsdlg::IniText, section: &str, key: &str, default: bool) -> bool {
    match ini.get(section, key) {
        Some(v) => {
            let v = v.trim();
            v.eq_ignore_ascii_case("Y") || v == "1" || v.eq_ignore_ascii_case("true")
        }
        None => default,
    }
}

/// `%LOCALAPPDATA%\DogEdit\session`。没有 `LOCALAPPDATA` 的场合退到临时
/// 目录 —— 和自动备份同一个兜底思路，至少不会整条路径失效。
fn default_root() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join("DogEdit").join("session"),
        _ => std::env::temp_dir().join("DogEdit").join("session"),
    }
}

fn state_path(root: &Path) -> PathBuf {
    root.join("session.ini")
}

/// 热保存载荷的目录。
pub(super) fn hot_dir(root: &Path) -> PathBuf {
    root.join("hot")
}

fn hot_payload(root: &Path, key: &str) -> PathBuf {
    hot_dir(root).join(format!("{key}.dgab"))
}

fn hot_meta_path(root: &Path, key: &str) -> PathBuf {
    hot_dir(root).join(format!("{key}.ini"))
}

/// 恢复时喂给 `open_file` 的临时文件。**不能**用 `<key>.dgab` 本身：
/// 渐进式加载在后台线程读盘，边读边删会读到半份。
pub(super) fn staging_path(root: &Path, key: &str) -> PathBuf {
    hot_dir(root).join(format!("{key}.restore.dgab"))
}

// ---------------------------------------------------------------------------
// 光标 / 书签记忆
// ---------------------------------------------------------------------------

/// 每文件一条的记忆。两条列表都按最近使用排序，`[0]` 最新。
#[derive(Default)]
pub(super) struct Memory {
    cursors: Vec<(String, (usize, usize))>,
    marks: Vec<(String, Vec<usize>)>,
}

impl Memory {
    pub(super) fn load(root: &Path) -> Self {
        let ini = ini_load(&state_path(root));
        let mut out = Self::default();
        let count: usize = ini_get(&ini, "Cursor", "Count", "0").parse().unwrap_or(0);
        for i in 0..count.min(MEMORY_MAX) {
            let raw = ini_get(&ini, "Cursor", &format!("Item{i}"), "");
            let Some((path, line, col)) = split3(&raw) else {
                continue;
            };
            if let (Ok(l), Ok(c)) = (line.parse(), col.parse()) {
                out.cursors.push((path.to_string(), (l, c)));
            }
        }
        let count: usize = ini_get(&ini, "Mark", "Count", "0").parse().unwrap_or(0);
        for i in 0..count.min(MEMORY_MAX) {
            let raw = ini_get(&ini, "Mark", &format!("Item{i}"), "");
            let Some((path, list)) = raw.rsplit_once('|') else {
                continue;
            };
            let lines: Vec<usize> = list
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if !lines.is_empty() {
                out.marks.push((path.to_string(), lines));
            }
        }
        out
    }

    pub(super) fn save(&self, root: &Path) {
        // 合并写：`session.ini` 里还有 `[Open]`，整份重写会把它抹掉
        //（退出时先写清单、关标签又写一次记忆，顺序上正好踩得到）。
        let mut ini = ini_load(&state_path(root));
        ini_remove_section(&mut ini, "Cursor");
        ini_remove_section(&mut ini, "Mark");
        ini_set(&mut ini, "Cursor", "Count", &self.cursors.len().to_string());
        for (i, (path, (line, col))) in self.cursors.iter().enumerate() {
            ini_set(&mut ini, "Cursor", &format!("Item{i}"), &format!("{path}|{line}|{col}"));
        }
        ini_set(&mut ini, "Mark", "Count", &self.marks.len().to_string());
        for (i, (path, lines)) in self.marks.iter().enumerate() {
            let list = lines
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(",");
            ini_set(&mut ini, "Mark", &format!("Item{i}"), &format!("{path}|{list}"));
        }
        let _ = std::fs::create_dir_all(root);
        ini_save(&ini, &state_path(root));
    }

    /// 同一个文件大小写不同也算同一条 —— Windows 的路径不区分大小写。
    fn slot<'a, T>(list: &'a [(String, T)], path: &str) -> Option<usize> {
        list.iter()
            .position(|(p, _)| p.eq_ignore_ascii_case(path))
    }

    pub(super) fn cursor(&self, path: &str) -> Option<(usize, usize)> {
        Self::slot(&self.cursors, path).map(|i| self.cursors[i].1)
    }

    /// 记一条光标，并把它挪到最前面。重复的路径只留一条。
    pub(super) fn set_cursor(&mut self, path: &str, caret: (usize, usize)) {
        if let Some(i) = Self::slot(&self.cursors, path) {
            let (p, _) = self.cursors.remove(i);
            self.cursors.insert(0, (p, caret));
        } else {
            self.cursors.insert(0, (path.to_string(), caret));
            self.cursors.truncate(MEMORY_MAX);
        }
    }

    pub(super) fn marks(&self, path: &str) -> Option<Vec<usize>> {
        Self::slot(&self.marks, path).map(|i| self.marks[i].1.clone())
    }

    /// 空列表等于「这个文件没有书签」—— 直接删条目，别留一行空的。
    pub(super) fn set_marks(&mut self, path: &str, lines: Vec<usize>) {
        if let Some(i) = Self::slot(&self.marks, path) {
            self.marks.remove(i);
        }
        if lines.is_empty() {
            return;
        }
        self.marks.insert(0, (path.to_string(), lines));
        self.marks.truncate(MEMORY_MAX);
    }
}

/// `a|b|c` → 三段。路径里不会有 `|`（Win32 文件名不允许），所以从
/// 右边切两次就够，左边那一段原样留着。
fn split3(raw: &str) -> Option<(&str, &str, &str)> {
    let (rest, col) = raw.rsplit_once('|')?;
    let (path, line) = rest.rsplit_once('|')?;
    if path.is_empty() {
        return None;
    }
    Some((path, line, col))
}

// ---------------------------------------------------------------------------
// 标签页清单
// ---------------------------------------------------------------------------

/// 上次退出时开着的一个标签页。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OpenItem {
    /// 有磁盘文件的标签。`cursor` 是退出时的光标。
    File {
        path: String,
        cursor: Option<(usize, usize)>,
    },
    /// 热保存的脏文档，正文在 `hot/<key>.dgab`。
    Hot { key: String },
}

pub(super) struct OpenList {
    pub items: Vec<OpenItem>,
    /// 退出时活动的那个标签在 `items` 里的下标。
    pub active: usize,
}

impl Default for OpenList {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            active: 0,
        }
    }
}

pub(super) fn read_open(root: &Path) -> OpenList {
    let ini = ini_load(&state_path(root));
    let count: usize = ini_get(&ini, "Open", "Count", "0").parse().unwrap_or(0);
    let mut items = Vec::new();
    for i in 0..count.min(1024) {
        let raw = ini_get(&ini, "Open", &format!("Item{i}"), "");
        if let Some(rest) = raw.strip_prefix("H|") {
            if !rest.is_empty() {
                items.push(OpenItem::Hot {
                    key: rest.to_string(),
                });
            }
        } else if let Some(rest) = raw.strip_prefix("F|") {
            let (path, cursor) = match rest.rsplit_once('|') {
                Some((p, lc)) => match lc.split_once(',') {
                    Some((l, c)) => match (l.parse(), c.parse()) {
                        (Ok(l), Ok(c)) => (p, Some((l, c))),
                        _ => (p, None),
                    },
                    None => (rest, None),
                },
                None => (rest, None),
            };
            if !path.is_empty() {
                items.push(OpenItem::File {
                    path: path.to_string(),
                    cursor,
                });
            }
        }
    }
    let active = ini_get(&ini, "Open", "Active", "0")
        .parse::<usize>()
        .unwrap_or(0)
        .min(items.len().saturating_sub(1));
    OpenList { items, active }
}

pub(super) fn write_open(root: &Path, list: &OpenList) {
    let mut ini = ini_load(&state_path(root));
    ini_remove_section(&mut ini, "Open");
    ini_set(&mut ini, "Open", "Count", &list.items.len().to_string());
    ini_set(&mut ini, "Open", "Active", &list.active.to_string());
    for (i, item) in list.items.iter().enumerate() {
        let value = match item {
            OpenItem::File {
                path,
                cursor: Some((line, col)),
            } => format!("F|{path}|{line},{col}"),
            OpenItem::File { path, cursor: None } => format!("F|{path}"),
            OpenItem::Hot { key } => format!("H|{key}"),
        };
        ini_set(&mut ini, "Open", &format!("Item{i}"), &value);
    }
    let _ = std::fs::create_dir_all(root);
    ini_save(&ini, &state_path(root));
}

// ---------------------------------------------------------------------------
// 热保存载荷
// ---------------------------------------------------------------------------

/// 一份热保存的元数据。正文单独存，这里只放还原标签要用的东西。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct HotMeta {
    /// 原文件路径；未命名文档为空。
    pub path: String,
    /// 标签名（未命名文档就是「无标题」这类本地化名字）。
    pub title: String,
    /// 退出时的光标。
    pub cursor: (usize, usize),
    /// 源编码名，恢复时按它加载（自动识别也行，指定编码重载过的那种只能靠它）。
    pub encoding: String,
    /// 源文件有没有 BOM。
    pub bom: bool,
    /// 写入时间（Unix 秒），回收用。
    pub time: u64,
}

impl HotMeta {
    fn to_text(&self) -> String {
        format!(
            "Path={}\r\nTitle={}\r\nCursor={},{}\r\nEncoding={}\r\nBom={}\r\nTime={}\r\n",
            self.path,
            self.title,
            self.cursor.0,
            self.cursor.1,
            self.encoding,
            if self.bom { "Y" } else { "N" },
            self.time
        )
    }

    fn parse(text: &str) -> Self {
        let mut out = Self::default();
        // 键值行的切分在 [`dg_core::ini`]；Path/Title/Encoding 按写的原样收。
        for (k, v) in dg_core::ini::Ini::parse(text).rows() {
            match k.trim() {
                "Path" => out.path = v.to_string(),
                "Title" => out.title = v.to_string(),
                "Cursor" => {
                    if let Some((l, c)) = v.split_once(',') {
                        out.cursor = (
                            l.trim().parse().unwrap_or(0),
                            c.trim().parse().unwrap_or(0),
                        );
                    }
                }
                "Encoding" => out.encoding = v.to_string(),
                "Bom" => out.bom = v.trim().eq_ignore_ascii_case("Y"),
                "Time" => out.time = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
        out
    }
}

/// 把一份文档的正文写到热保存载荷里。正文是**源编码原样字节**（含 BOM、
/// 每行各自的换行符），所以还原出来的字节和没保存过的那份一模一样。
pub(super) fn write_hot(root: &Path, key: &str, doc: &Document, meta: &HotMeta) -> bool {
    let dir = hot_dir(root);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let tmp = dir.join(format!("{key}.dgab.tmp{}", std::process::id()));
    let ok = std::fs::File::create(&tmp)
        .and_then(|f| {
            use std::io::Write as _;
            let mut w = std::io::BufWriter::with_capacity(256 * 1024, f);
            doc.save_to(&mut w)?;
            w.flush()
        })
        .is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    let payload = hot_payload(root, key);
    if std::fs::rename(&tmp, &payload).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    // 写入时间由这里盖章，调用方不必自己取时钟。
    let mut meta = meta.clone();
    meta.time = now_secs();
    std::fs::write(hot_meta_path(root, key), meta.to_text()).is_ok()
}

/// 读回一份热保存。元数据或正文缺一个就算没有 —— 半份数据还原不出东西。
pub(super) fn read_hot(root: &Path, key: &str) -> Option<(Vec<u8>, HotMeta)> {
    let meta = std::fs::read_to_string(hot_meta_path(root, key)).ok()?;
    let bytes = std::fs::read(hot_payload(root, key)).ok()?;
    Some((bytes, HotMeta::parse(&meta)))
}

pub(super) fn drop_hot(root: &Path, key: &str) {
    let _ = std::fs::remove_file(hot_payload(root, key));
    let _ = std::fs::remove_file(hot_meta_path(root, key));
}

/// 清掉过期的载荷。热保存是「退出时写、下次启动读」，读到就删，所以这里
/// 只兜两种情况：选项被关掉（没人读）和启动时命令行带了文件（跳过一次）。
pub(super) fn prune_hot(root: &Path) {
    let Ok(rd) = std::fs::read_dir(hot_dir(root)) else {
        return;
    };
    let now = now_secs();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stamp = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if stamp + HOT_KEEP_DAYS * 86_400 < now {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 挂在 EditorState 上的状态
// ---------------------------------------------------------------------------

/// 会话状态：设置、记忆，以及这次退出已经写出去的热保存清单。
pub(super) struct State {
    pub settings: Settings,
    pub memory: Memory,
    /// 这次退出写出去的热保存（文档 id → key）。用户选「保存」时按 id 删掉
    /// 对应那份，取消退出时整批删掉。
    hot: Vec<(u64, String)>,
}

impl State {
    pub(super) fn new() -> Self {
        let settings = Settings::load();
        let memory = Memory::load(&settings.root());
        Self {
            settings,
            memory,
            hot: Vec::new(),
        }
    }

    /// 选项里点了「应用」：重读设置。记忆不重读 —— 它只在启动时进内存，
    /// 中途换目录也只是换个地方写。
    pub(super) fn reload(&mut self) {
        self.settings = Settings::load();
    }

    pub(super) fn note_hot(&mut self, id: u64, key: String) {
        self.hot.retain(|(i, _)| *i != id);
        self.hot.push((id, key));
    }

    /// 取走某份文档的热保存键（`save_file` 用完就删）。
    pub(super) fn forget_hot(&mut self, id: u64) -> Option<String> {
        let pos = self.hot.iter().position(|(i, _)| *i == id)?;
        Some(self.hot.remove(pos).1)
    }

    /// 取消退出：把这次写出去的全部清掉。
    pub(super) fn take_hot(&mut self) -> Vec<(u64, String)> {
        std::mem::take(&mut self.hot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dg-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn memory_round_trips_through_disk() {
        let root = temp_root("mem");
        let mut m = Memory::default();
        m.set_cursor(r"C:\work\a.txt", (12, 4));
        m.set_marks(r"C:\work\a.txt", vec![1, 3, 9]);
        m.set_cursor(r"C:\work\b.txt", (0, 0));
        m.save(&root);

        let back = Memory::load(&root);
        assert_eq!(back.cursor(r"C:\work\a.txt"), Some((12, 4)));
        assert_eq!(back.cursor(r"C:\work\b.txt"), Some((0, 0)));
        assert_eq!(back.marks(r"C:\work\a.txt"), Some(vec![1, 3, 9]));
        assert_eq!(back.cursor(r"C:\work\c.txt"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn memory_is_case_insensitive_and_moves_to_front() {
        let mut m = Memory::default();
        m.set_cursor("C:\\Work\\A.TXT", (1, 1));
        m.set_cursor("C:\\other.txt", (2, 2));
        m.set_cursor("c:\\work\\a.txt", (3, 3));
        assert_eq!(m.cursors.len(), 2);
        assert_eq!(m.cursors[0].0, "C:\\Work\\A.TXT");
        assert_eq!(m.cursor("C:\\WORK\\a.txt"), Some((3, 3)));
    }

    #[test]
    fn memory_caps_at_the_limit() {
        let mut m = Memory::default();
        for i in 0..(MEMORY_MAX + 20) {
            m.set_cursor(&format!("C:\\f{i}.txt"), (i, 0));
        }
        assert_eq!(m.cursors.len(), MEMORY_MAX);
        // 最新的那条在，最旧的被挤掉了。
        assert!(m.cursor(&format!("C:\\f{}.txt", MEMORY_MAX + 19)).is_some());
        assert!(m.cursor("C:\\f0.txt").is_none());
    }

    #[test]
    fn empty_marks_drop_the_entry() {
        let mut m = Memory::default();
        m.set_marks("C:\\a.txt", vec![2, 5]);
        m.set_marks("C:\\a.txt", Vec::new());
        assert_eq!(m.marks("C:\\a.txt"), None);
        assert!(m.marks.is_empty());
    }

    #[test]
    fn open_list_round_trips() {
        let root = temp_root("open");
        let list = OpenList {
            items: vec![
                OpenItem::File {
                    path: r"C:\a.txt".into(),
                    cursor: Some((7, 2)),
                },
                OpenItem::Hot {
                    key: "abc123".into(),
                },
                OpenItem::File {
                    path: r"C:\b.txt".into(),
                    cursor: None,
                },
            ],
            active: 1,
        };
        write_open(&root, &list);
        let back = read_open(&root);
        assert_eq!(back.items, list.items);
        assert_eq!(back.active, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn memory_save_keeps_the_open_list() {
        // 退出时先写清单、关标签时又写一次记忆：两次写同一个文件，
        // 谁也不能把对方那几段抹掉。
        let root = temp_root("merge");
        write_open(
            &root,
            &OpenList {
                items: vec![OpenItem::File {
                    path: r"C:\a.txt".into(),
                    cursor: None,
                }],
                active: 0,
            },
        );
        let mut m = Memory::default();
        m.set_cursor(r"C:\a.txt", (1, 2));
        m.save(&root);
        assert_eq!(read_open(&root).items.len(), 1);

        write_open(&root, &OpenList::default());
        assert_eq!(Memory::load(&root).cursor(r"C:\a.txt"), Some((1, 2)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_list_ignores_junk() {        let root = temp_root("junk");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            state_path(&root),
            "[Open]\nCount=4\nActive=9\nItem0=\nItem1=X|nope\nItem2=F|\nItem3=F|C:\\ok.txt\n",
        )
        .unwrap();
        let back = read_open(&root);
        assert_eq!(back.items.len(), 1);
        // Active 超出范围要夹回来，不能留个越界下标。
        assert_eq!(back.active, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hot_payload_keeps_bytes_and_meta() {
        let root = temp_root("hot");
        // 非 UTF-8、带 BOM 的字节：还原必须逐字节相同。
        let doc = Document::from_text("hello\r\nworld\n");
        let meta = HotMeta {
            path: r"C:\work\a.txt".into(),
            title: "无标题".into(),
            cursor: (1, 3),
            encoding: "UTF-8".into(),
            bom: true,
            time: 12345,
        };
        assert!(write_hot(&root, "k1", &doc, &meta));
        let (bytes, back) = read_hot(&root, "k1").unwrap();
        assert_eq!(bytes, b"hello\r\nworld\n");
        // 时间由 `write_hot` 盖章，其余字段原样回来。
        assert!(back.time > 0);
        assert_eq!(HotMeta { time: 0, ..back }, HotMeta { time: 0, ..meta });
        drop_hot(&root, "k1");
        assert!(read_hot(&root, "k1").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hot_meta_survives_chinese_and_empty_path() {
        let meta = HotMeta {
            path: String::new(),
            title: "无标题".into(),
            cursor: (0, 0),
            encoding: "GBK".into(),
            bom: false,
            time: 7,
        };
        assert_eq!(HotMeta::parse(&meta.to_text()), meta);
    }

    #[test]
    fn default_root_is_under_local_appdata() {
        // 只要有 LOCALAPPDATA，默认根就必须在它下面 —— 用户抱怨「状态文件
        // 乱放」时第一个要看的就是这里。
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            let root = Settings {
                cursor: true,
                files: false,
                hot: false,
                markers: false,
                temp_dir: None,
            }
            .root();
            assert!(root.starts_with(PathBuf::from(base)));
        }
    }

    #[test]
    fn temp_folder_overrides_the_root() {
        let s = Settings {
            cursor: true,
            files: false,
            hot: false,
            markers: false,
            temp_dir: Some(PathBuf::from(r"D:\tmp\dogedit")),
        };
        assert_eq!(s.root(), PathBuf::from(r"D:\tmp\dogedit"));
    }
}
