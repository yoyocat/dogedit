//! 自动备份与本地历史。
//!
//! 每个文档一份目录，目录里每个版本一对文件：
//!
//! ```text
//! %LOCALAPPDATA%\DogEdit\backup\3f\3f9c1d2a7b40e815\
//!     doc.ini     原路径、标题、创建时间
//!     v0007.dgab  版本内容（源编码原样字节）
//!     v0007.ini   时间、大小、Hash、Rev、基线戳、光标
//! ```
//!
//! 目录名是原路径小写后的 FNV-1a：同名不同目录不会撞，长路径也不会
//! 撑破 `MAX_PATH`。未命名文档没有路径，键是随机的 `u` 开头的十六进制，
//! 在文档活着的时候固定不变。
//!
//! 写入走「临时文件 → rename」：崩在写的一半只会留下一个 `.tmp`，
//! 扫描时看不见它，也不会被当成一版。内容用 `Document::save_to` 流式
//! 写，不物化整文；大文件臂它自己会返回 `Unsupported`，所以大文件
//! 天然不进来。

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlgtpl::{self, Class, Item};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dg_core::Document;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, GetClientRect, GetDlgItem, GetWindow, GetWindowLongPtrW, GWLP_USERDATA, GW_CHILD,
    GW_HWNDNEXT, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, LB_SETITEMDATA,
    PostQuitMessage, SendMessageW, SetWindowLongPtrW, SetWindowTextW, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_ERASEBKGND, WM_INITDIALOG, WM_SETFONT,
};
use windows::core::PCWSTR;

use crate::win32::chrome;
use crate::win32::customdraw;
use crate::win32::prompt::{run_modal_loop, show_modal};
use crate::win32::window::to_colorref;

/// 每份文件留几版。`[Save] BackupVersions` 覆盖它。
pub(super) const DEFAULT_VERSIONS: u32 = 10;
/// 版本留多少天。`[Save] KeepDays` 覆盖它，0 表示永久保留。
pub(super) const DEFAULT_KEEP_DAYS: u32 = 30;
/// 整棵备份树的大小上限（MB）。`[Save] BudgetMB` 覆盖它，0 表示不限制。
pub(super) const DEFAULT_BUDGET_MB: u32 = 256;
/// FNV-1a 的 64 位偏移基。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// 「备份与历史」的命令号，落在本实现的扩展段 36000+。定义放在命令表旁边：
/// 菜单行能不能点是 `cmdids::is_answerable` 说了算，它看不到这个模块。
pub(super) const CMD_BACKUP_HISTORY: u32 = crate::cmdids::BACKUP_HISTORY_ID;
/// 启动时那条「有未保存的备份」的自投消息 —— 首帧之后才弹，模态框不能
/// 开在 `WM_CREATE` 里。
pub(super) const WM_BACKUP_PROMPT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 0x60;
/// 超过这个大小的文档不备份，`[Save] BigFile`（MB）覆盖它。
const DEFAULT_BIG_FILE_MB: u32 = 4;

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

/// `[Save]` 里与备份有关的那几项，外加选项页写进去的落点。
#[derive(Clone, Debug)]
pub(super) struct Settings {
    /// 使用自动备份。
    pub enabled: bool,
    /// 自动保存时间间隔；`None` 表示 0（禁止定时备份）。
    pub interval: Option<Duration>,
    /// 超过这个字节数就不备。
    pub big_file: u64,
    /// 每份文件留几版。
    pub versions: u32,
    /// 版本留多少天；0 表示永久保留。
    pub keep_days: u32,
    /// 整棵备份树的上限（MB）；0 表示不限制。
    pub budget_mb: u32,
    /// 「自定义文件夹」勾上时的落点。
    pub custom_dir: Option<PathBuf>,
}

impl Settings {
    pub(super) fn load() -> Self {
        let ini = super::optionsdlg::IniText::load(&super::optionsdlg::ini_path());
        let minutes = read_u32(&ini, "Save", "AutoSaveTime", 0);
        let big_mb = read_u32(&ini, "Save", "BigFile", DEFAULT_BIG_FILE_MB).clamp(1, 100_000);
        let versions = read_u32(&ini, "Save", "BackupVersions", DEFAULT_VERSIONS).clamp(1, 500);
        let keep_days = read_u32(&ini, "Save", "KeepDays", DEFAULT_KEEP_DAYS).min(3650);
        let budget_mb = read_u32(&ini, "Save", "BudgetMB", DEFAULT_BUDGET_MB).min(65536);
        let custom = if read_bool(&ini, "Save", "SpecialDir", false) {
            ini.get("Save", "Dir")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        } else {
            None
        };
        // 探针用的压缩开关：间隔的单位是分钟，崩溃恢复的测试等不起。
        let interval = match std::env::var("DOGEDIT_BACKUP_MS").ok().and_then(|v| v.parse::<u64>().ok())
        {
            Some(ms) if ms > 0 => Some(Duration::from_millis(ms)),
            _ if minutes > 0 => Some(Duration::from_secs(minutes as u64 * 60)),
            _ => None,
        };
        Self {
            enabled: read_bool(&ini, "Save", "AutoBackup", false),
            interval,
            big_file: big_mb as u64 * 1024 * 1024,
            versions,
            keep_days,
            budget_mb,
            custom_dir: custom,
        }
    }

    /// 回收用的三个上限。0 在这两项里是「不限制」，落到 `Option` 上。
    pub(super) fn limits(&self) -> Limits {
        Limits {
            versions: self.versions,
            keep: (self.keep_days > 0).then(|| Duration::from_secs(self.keep_days as u64 * 86_400)),
            budget: (self.budget_mb > 0).then(|| self.budget_mb as u64 * 1024 * 1024),
        }
    }

    /// 定时备份是不是真的会跑。
    pub(super) fn timer_on(&self) -> bool {
        self.enabled && self.interval.is_some()
    }

    /// 备份根。勾了「自定义文件夹」就用它，否则用用户数据目录。
    pub(super) fn root(&self) -> PathBuf {
        self.custom_dir
            .clone()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(default_root)
    }
}

fn read_u32(ini: &super::optionsdlg::IniText, section: &str, key: &str, default: u32) -> u32 {
    ini.get(section, key)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
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

/// `%LOCALAPPDATA%\DogEdit\backup`。没有 `LOCALAPPDATA` 的场合退到临时目录，
/// 至少不会因为写不出去而整条路径失效。
fn default_root() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join("DogEdit").join("backup"),
        _ => std::env::temp_dir().join("DogEdit").join("backup"),
    }
}

// ---------------------------------------------------------------------------
// 键与路径
// ---------------------------------------------------------------------------

/// 路径 → 目录名。Windows 的路径不区分大小写，所以先折成小写再哈希，
/// 否则 `C:\a.txt` 与 `c:\a.txt` 会各存一份。
pub(super) fn key_for_path(path: &str) -> String {
    format!("{:016x}", fnv1a(path.to_lowercase().as_bytes()))
}

/// 未命名文档的键。`id` 是进程内的文档编号，`nanos` 让同时开着的两个
/// 空文档不会撞，前缀 `u` 让它在目录里一眼能认出来。
pub(super) fn key_for_untitled(id: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("u{:015x}", fnv1a(format!("{id}:{nanos}").as_bytes()) & 0x0FFF_FFFF_FFFF_FFFF)
}

/// 一份文档的目录。
pub(super) fn doc_dir(root: &Path, key: &str) -> PathBuf {
    let shard = &key[..2.min(key.len())];
    root.join(shard).join(key)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = FNV_OFFSET;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 元数据
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// 定时器写的、还没保存进磁盘的内容。
    Auto,
    /// 保存前磁盘上的那一份。
    Presave,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Auto => "auto",
            Kind::Presave => "presave",
        }
    }

    fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("presave") {
            Kind::Presave
        } else {
            Kind::Auto
        }
    }
}

/// 一版备份的描述，就是 `vNNNN.ini` 的内容。
#[derive(Clone, Debug)]
pub(super) struct Meta {
    pub time: u64,
    pub kind: Kind,
    pub size: u64,
    pub hash: u64,
    pub rev: u64,
    /// 写这一版时原文件在磁盘上的大小与 mtime（秒）。未命名文档没有。
    pub base: Option<(u64, u64)>,
    pub encoding: String,
    pub bom: bool,
    pub eol: u8,
    pub caret: (usize, usize),
}

impl Meta {
    fn to_text(&self) -> String {
        let mut s = String::from("[Version]\n");
        s.push_str(&format!("Time={}\n", self.time));
        s.push_str(&format!("Kind={}\n", self.kind.as_str()));
        s.push_str(&format!("Size={}\n", self.size));
        s.push_str(&format!("Hash={:016x}\n", self.hash));
        s.push_str(&format!("Rev={}\n", self.rev));
        let (bs, bm) = self.base.unwrap_or((0, 0));
        s.push_str(&format!("BaseSize={bs}\n"));
        s.push_str(&format!("BaseMTime={bm}\n"));
        s.push_str(&format!("Encoding={}\n", self.encoding));
        s.push_str(&format!("Bom={}\n", if self.bom { "Y" } else { "N" }));
        s.push_str(&format!("Eol={}\n", self.eol));
        s.push_str(&format!("Caret={},{}\n", self.caret.0, self.caret.1));
        s
    }

    fn parse(text: &str) -> Option<Self> {
        let mut meta = Meta {
            time: 0,
            kind: Kind::Auto,
            size: 0,
            hash: 0,
            rev: 0,
            base: None,
            encoding: String::from("UTF-8"),
            bom: false,
            eol: 2,
            caret: (0, 0),
        };
        let mut base_size = 0u64;
        let mut base_mtime = 0u64;
        // 键值行的切分在 [`dg_core::ini`]；这里只认自己的键。
        for (k, v) in dg_core::ini::Ini::parse(text).rows() {
            let v = v.trim();
            match k.trim() {
                "Time" => meta.time = v.parse().ok()?,
                "Kind" => meta.kind = Kind::parse(v),
                "Size" => meta.size = v.parse().ok()?,
                "Hash" => meta.hash = u64::from_str_radix(v, 16).ok()?,
                "Rev" => meta.rev = v.parse().unwrap_or(0),
                "BaseSize" => base_size = v.parse().unwrap_or(0),
                "BaseMTime" => base_mtime = v.parse().unwrap_or(0),
                "Encoding" => meta.encoding = v.to_string(),
                "Bom" => meta.bom = v.eq_ignore_ascii_case("Y"),
                "Eol" => meta.eol = v.parse().unwrap_or(2),
                "Caret" => {
                    if let Some((a, b)) = v.split_once(',') {
                        meta.caret = (a.parse().unwrap_or(0), b.parse().unwrap_or(0));
                    }
                }
                _ => {}
            }
        }
        if base_size > 0 {
            meta.base = Some((base_size, base_mtime));
        }
        Some(meta)
    }
}

/// 一份文档的 `doc.ini`。
pub(super) struct DocMeta {
    pub path: String,
    pub title: String,
    pub created: u64,
}

impl DocMeta {
    fn to_text(&self) -> String {
        format!(
            "[Doc]\nPath={}\nTitle={}\nCreated={}\n",
            self.path, self.title, self.created
        )
    }

    fn parse(text: &str) -> Self {
        let mut m = DocMeta {
            path: String::new(),
            title: String::new(),
            created: 0,
        };
        for (k, v) in dg_core::ini::Ini::parse(text).rows() {
            let v = v.trim();
            match k.trim() {
                "Path" => m.path = v.to_string(),
                "Title" => m.title = v.to_string(),
                "Created" => m.created = v.parse().unwrap_or(0),
                _ => {}
            }
        }
        m
    }
}

// ---------------------------------------------------------------------------
// 写入
// ---------------------------------------------------------------------------

/// 写一版需要文档之外的这点东西。
pub(super) struct WriteInfo<'a> {
    pub kind: Kind,
    /// 原文件路径；未命名文档是空串。
    pub path: &'a str,
    /// 标签名，未命名文档用它做提示。
    pub title: &'a str,
    pub caret: (usize, usize),
    pub rev: usize,
    pub eol: u8,
    pub encoding: String,
    pub bom: bool,
}

/// 一版落盘的结果。
pub(super) enum Written {
    /// 新加了一版。带版本号：对话框和状态提示要指名道姓。
    New(#[allow(dead_code)] u32),
    /// 内容与最新一版逐字节相同，只把它的时间往后挪了。
    Same,
    /// 跳过（太大、写不出去）。
    Skipped,
}

/// 写一版。返回 `None` 表示连目录都没建起来。
pub(super) fn write_version(
    root: &Path,
    key: &str,
    doc: &Document,
    info: &WriteInfo<'_>,
    keep: u32,
) -> Option<Written> {
    let dir = doc_dir(root, key);
    std::fs::create_dir_all(&dir).ok()?;
    let (next, newest_hash) = next_slot(&dir);
    let base = file_stamp(info.path);
    let tmp = dir.join(format!("v{next:04}.dgab.tmp{}", std::process::id()));
    let mut hasher = HashingWriter::new(std::io::BufWriter::with_capacity(
        256 * 1024,
        std::fs::File::create(&tmp).ok()?,
    ));
    let ok = doc.save_to(&mut hasher).is_ok() && hasher.inner.flush().is_ok();
    let (hash, size) = (hasher.hash(), hasher.bytes());
    drop(hasher);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return Some(Written::Skipped);
    }
    let meta = Meta {
        time: now_secs(),
        kind: info.kind,
        size,
        hash,
        rev: info.rev as u64,
        base,
        encoding: info.encoding.clone(),
        bom: info.bom,
        eol: info.eol,
        caret: info.caret,
    };
    commit(&dir, &tmp, next, newest_hash, meta, keep, info)
}

/// 把磁盘上的一份文件原样收一版（保存前的快照）。不经过解码 ——
/// 复制的是字节，回滚时拿到的就是当时那个文件。
pub(super) fn write_file_version(
    root: &Path,
    key: &str,
    src: &Path,
    info: &WriteInfo<'_>,
    keep: u32,
) -> Option<Written> {
    let dir = doc_dir(root, key);
    std::fs::create_dir_all(&dir).ok()?;
    let (next, newest_hash) = next_slot(&dir);
    let base = file_stamp(src.to_str().unwrap_or(""));
    let tmp = dir.join(format!("v{next:04}.dgab.tmp{}", std::process::id()));
    let mut hasher = HashingWriter::new(std::io::BufWriter::with_capacity(
        256 * 1024,
        std::fs::File::create(&tmp).ok()?,
    ));
    let ok = std::fs::File::open(src)
        .map(|mut f| std::io::copy(&mut f, &mut hasher).is_ok())
        .unwrap_or(false)
        && hasher.inner.flush().is_ok();
    let (hash, size) = (hasher.hash(), hasher.bytes());
    drop(hasher);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return Some(Written::Skipped);
    }
    let meta = Meta {
        time: now_secs(),
        kind: info.kind,
        size,
        hash,
        rev: info.rev as u64,
        base,
        encoding: info.encoding.clone(),
        bom: info.bom,
        eol: info.eol,
        caret: info.caret,
    };
    commit(&dir, &tmp, next, newest_hash, meta, keep, info)
}

/// 下一版该用几号、最新一版的内容指纹是多少。
fn next_slot(dir: &Path) -> (u32, Option<u64>) {
    let versions = list_versions(dir);
    let next = versions.last().map(|(i, _)| *i + 1).unwrap_or(1);
    (next, versions.last().map(|(_, m)| m.hash))
}

/// 临时文件已经在磁盘上了：内容与最新一版相同就只改时间，否则改名成一版，
/// 顺手写 meta、补 `doc.ini`、轮转。
fn commit(
    dir: &Path,
    tmp: &Path,
    next: u32,
    newest_hash: Option<u64>,
    meta: Meta,
    keep: u32,
    info: &WriteInfo<'_>,
) -> Option<Written> {
    if newest_hash == Some(meta.hash) {
        let _ = std::fs::remove_file(tmp);
        if let Some((index, mut newest)) = list_versions(dir).pop() {
            newest.time = now_secs();
            let _ = std::fs::write(dir.join(format!("v{index:04}.ini")), newest.to_text());
        }
        return Some(Written::Same);
    }
    let target = dir.join(format!("v{next:04}.dgab"));
    if std::fs::rename(tmp, &target).is_err() {
        let _ = std::fs::remove_file(tmp);
        return Some(Written::Skipped);
    }
    let _ = std::fs::write(dir.join(format!("v{next:04}.ini")), meta.to_text());
    // 未命名文档也要写 `doc.ini`：`Path=` 空、`Title=` 是标签上那个名字。少了它，
    // 对话框只能按目录名认人，恢复出来的标签也没有标题可还原。
    let doc_ini = dir.join("doc.ini");
    if !doc_ini.exists() {
        let m = DocMeta {
            path: info.path.to_string(),
            title: info.title.to_string(),
            created: now_secs(),
        };
        let _ = std::fs::write(&doc_ini, m.to_text());
    }
    prune(dir, keep.max(1));
    Some(Written::New(next))
}

/// 原文件此刻的大小与 mtime（秒）。未命名文档、读不到的路径都答 `None`。
fn file_stamp(path: &str) -> Option<(u64, u64)> {
    if path.is_empty() {
        return None;
    }
    std::fs::metadata(path).ok().map(|m| (m.len(), mtime_secs(&m)))
}

/// 边写边算 FNV-1a，省掉为了去重再读一遍文件。保存那条路径也用它：
/// 算出来的指纹正好能认出「哪一版已经变成磁盘上的内容了」。
pub(super) struct HashingWriter<W: Write> {
    inner: W,
    hash: u64,
    bytes: u64,
}

impl<W: Write> HashingWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            hash: FNV_OFFSET,
            bytes: 0,
        }
    }

    /// 到目前为止写过的字节数。
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// 到目前为止的内容指纹。
    pub(super) fn hash(&self) -> u64 {
        self.hash
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        for b in &buf[..n] {
            self.hash ^= *b as u64;
            self.hash = self.hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn mtime_secs(m: &std::fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 读与回收
// ---------------------------------------------------------------------------

/// 目录里现有的版本，按编号升序。
pub(super) fn list_versions(dir: &Path) -> Vec<(u32, Meta)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(u32, Meta)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("ini") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(digits) = stem.strip_prefix('v') else {
            continue;
        };
        let Ok(index) = digits.parse::<u32>() else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if let Some(meta) = Meta::parse(&text) {
            out.push((index, meta));
        }
    }
    out.sort_by_key(|(i, _)| *i);
    out
}

/// 保存成功之后，把内容与磁盘一致的自动版本删掉 —— 它们已经没有信息量，
/// 留着只会把真正的历史挤出 `BackupVersions` 的名额。保存前的快照不动。
pub(super) fn drop_saved(root: &Path, key: &str, hash: u64) {
    let dir = doc_dir(root, key);
    for (index, meta) in list_versions(&dir) {
        if meta.kind == Kind::Auto && meta.hash == hash {
            let _ = std::fs::remove_file(dir.join(format!("v{index:04}.dgab")));
            let _ = std::fs::remove_file(dir.join(format!("v{index:04}.ini")));
        }
    }
}

/// 回收用的三个上限，全部来自选项页（`[Save] BackupVersions / KeepDays / BudgetMB`）。
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    /// 每个文件留几版。
    pub versions: u32,
    /// 多久之前的版本算过期；`None` = 永久保留。
    pub keep: Option<Duration>,
    /// 整棵树的总量上限；`None` = 不限制。
    pub budget: Option<u64>,
}

/// 只留最新的 `keep` 版。
pub(super) fn prune(dir: &Path, keep: u32) {
    let versions = list_versions(dir);
    if versions.len() <= keep as usize {
        return;
    }
    let cut = versions.len() - keep as usize;
    for (index, _) in &versions[..cut] {
        let _ = std::fs::remove_file(dir.join(format!("v{index:04}.dgab")));
        let _ = std::fs::remove_file(dir.join(format!("v{index:04}.ini")));
    }
}

/// 整棵树的回收：每个文件超出 N 版的旧版、超过保留天数的目录、总量超预算
/// 的最旧版本，以及原文件早已不在且整目录都过期的空壳。
pub(super) fn gc(root: &Path, limits: Limits) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let now = now_secs();
    let mut all: Vec<(u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    for shard in rd.flatten() {
        if !shard.path().is_dir() {
            continue;
        }
        let Ok(dirs) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for doc in dirs.flatten() {
            let dir = doc.path();
            if !dir.is_dir() {
                continue;
            }
            let mut versions = list_versions(&dir);
            if versions.is_empty() {
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            }
            // 用户把版本数调小之后，这里就是它生效的地方：先按 N 砍，再谈
            // 过期和预算——砍掉的版本本来也不该算进总量。
            if versions.len() > limits.versions as usize {
                prune(&dir, limits.versions);
                versions = list_versions(&dir);
            }
            if let Some(keep) = limits.keep {
                let newest = versions.iter().map(|(_, m)| m.time).max().unwrap_or(0);
                if newest + keep.as_secs() < now {
                    let _ = std::fs::remove_dir_all(&dir);
                    continue;
                }
            }
            for (index, meta) in versions {
                let payload = dir.join(format!("v{index:04}.dgab"));
                let size = std::fs::metadata(&payload).map(|m| m.len()).unwrap_or(0);
                total += size;
                all.push((meta.time, payload));
            }
        }
    }
    let Some(budget) = limits.budget else {
        return;
    };
    if total <= budget {
        return;
    }
    all.sort_by_key(|(t, _)| *t);
    let mut excess = total - budget;
    for (_, payload) in all {
        if excess == 0 {
            break;
        }
        let size = std::fs::metadata(&payload).map(|m| m.len()).unwrap_or(0);
        let ini = payload.with_extension("ini");
        let _ = std::fs::remove_file(&payload);
        let _ = std::fs::remove_file(&ini);
        excess = excess.saturating_sub(size);
    }
}

pub(super) fn read_doc_meta(dir: &Path) -> Option<DocMeta> {
    let text = std::fs::read_to_string(dir.join("doc.ini")).ok()?;
    Some(DocMeta::parse(&text))
}

// ---------------------------------------------------------------------------
// 定时器状态
// ---------------------------------------------------------------------------

/// 挂在窗口上的那份状态：设置、轮转游标、定时器是否已 arm。
pub(super) struct State {
    pub settings: Settings,
    pub cursor: usize,
    pub armed: bool,
    /// 上一次整树回收的时间，一小时一次就够。
    pub gc_at: Option<Instant>,
}

impl State {
    pub(super) fn new() -> Self {
        Self {
            settings: Settings::load(),
            cursor: 0,
            armed: false,
            gc_at: None,
        }
    }

    /// 设置改了之后重读；定时器由调用方按 `timer_on()` 重新 arm。
    pub(super) fn reload(&mut self) {
        self.settings = Settings::load();
    }
}


// ---------------------------------------------------------------------------
// 扫描：备份根里都有什么
// ---------------------------------------------------------------------------

/// 一份原文件在备份根里的全部记录。
pub(super) struct FileEntry {
    pub dir: PathBuf,
    pub path: String,
    pub title: String,
    /// 版本按编号升序。
    pub versions: Vec<(u32, Meta)>,
    /// 原文件自最新一版备份之后被改过（或已经不在了）。
    pub changed: bool,
    /// 还有没进磁盘的自动备份。
    pub pending: bool,
}

impl FileEntry {
    /// 列表里那一行：有路径就用文件名，未命名就用标签名。
    pub(super) fn name(&self) -> String {
        if !self.path.is_empty() {
            Path::new(&self.path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.clone())
        } else if !self.title.is_empty() {
            self.title.clone()
        } else {
            String::from("无标题")
        }
    }

    fn newest_time(&self) -> u64 {
        self.versions.iter().map(|(_, m)| m.time).max().unwrap_or(0)
    }
}

fn current_stamp(path: &str) -> Option<(u64, u64)> {
    file_stamp(path)
}

/// 扫一遍备份根。有未保存内容的排前面，其余按最新一版的时间倒序。
pub(super) fn scan(root: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let Ok(shards) = std::fs::read_dir(root) else {
        return out;
    };
    for shard in shards.flatten() {
        if !shard.path().is_dir() {
            continue;
        }
        let Ok(dirs) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for d in dirs.flatten() {
            let dir = d.path();
            if !dir.is_dir() {
                continue;
            }
            let versions = list_versions(&dir);
            if versions.is_empty() {
                continue;
            }
            let meta = read_doc_meta(&dir).unwrap_or(DocMeta {
                path: String::new(),
                title: String::new(),
                created: 0,
            });
            let stamp = current_stamp(&meta.path);
            let newest_base = versions.last().and_then(|(_, m)| m.base);
            let changed = match (stamp, newest_base) {
                (Some(now), Some(base)) => now != base,
                _ => false,
            };
            let file_mtime = stamp.map(|s| s.1);
            let pending = versions.iter().any(|(_, m)| {
                m.kind == Kind::Auto
                    && match file_mtime {
                        Some(t) => m.time > t,
                        None => true,
                    }
            });
            out.push(FileEntry {
                dir,
                path: meta.path,
                title: meta.title,
                versions,
                changed,
                pending,
            });
        }
    }
    out.sort_by(|a, b| {
        b.pending
            .cmp(&a.pending)
            .then(b.newest_time().cmp(&a.newest_time()))
    });
    out
}

/// 有需要提示的备份吗（启动时用）。
pub(super) fn has_pending(root: &Path) -> bool {
    scan(root).iter().any(|e| e.pending)
}

// ---------------------------------------------------------------------------
// 时间与预览
// ---------------------------------------------------------------------------

/// Unix 秒 → 本地时间 `YYYY-MM-DD HH:MM:SS`。走系统的时区转换，
/// 不自己算 —— 自己算出来的是 UTC，差八小时。
fn local_time_text(secs: u64) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    use windows::Win32::System::Time::FileTimeToSystemTime;
    // Unix 纪元到 1601-01-01 的 100ns 数。
    let ticks = (secs + 11_644_473_600) * 10_000_000;
    let ft = FILETIME {
        dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    unsafe {
        let mut local = FILETIME::default();
        let mut st = SYSTEMTIME::default();
        if FileTimeToLocalFileTime(&ft, &mut local).is_ok()
            && FileTimeToSystemTime(&local, &mut st).is_ok()
        {
            return format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
            );
        }
    }
    format!("{secs}")
}

fn read_head(path: &Path, limit: u64) -> Vec<u8> {
    use std::io::Read as _;
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(limit).read_to_end(&mut buf);
    }
    buf
}

fn decode_text(bytes: &[u8], encoding: &str) -> String {
    if let Ok(cp) = encoding.parse::<u16>() {
        return dg_core::encoding::decode_codepage(cp, bytes);
    }
    let enc = encoding_rs::Encoding::for_label(encoding.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    enc.decode(bytes).0.into_owned()
}

/// 预览：最多 200 行或 64 KB，谁先到算谁。4 MB 的版本也不许卡住列表。
fn preview_text(dir: &Path, index: u32, meta: &Meta) -> String {
    const LIMIT: u64 = 64 * 1024;
    const LINES: usize = 200;
    let bytes = read_head(&dir.join(format!("v{index:04}.dgab")), LIMIT);
    let text = decode_text(&bytes, &meta.encoding);
    let mut lines: Vec<&str> = text.lines().collect();
    let truncated = lines.len() > LINES || bytes.len() as u64 >= LIMIT;
    if lines.len() > LINES {
        lines.truncate(LINES);
    }
    let mut out = lines.join("\r\n");
    if truncated {
        out.push_str("\r\n……（仅预览前 200 行）");
    }
    out
}

// ---------------------------------------------------------------------------
// 对话框
// ---------------------------------------------------------------------------

const DLG_STYLE: u32 = 0x80c8_00c0;
const DLG_W: u16 = 400;
const DLG_H: u16 = 232;
const DLG_TITLE: &str = "备份与历史";
/// 语言表里的 `[DLG36010]`。手搓的模板没有资源 id，这个数只是给
/// `localize_dialog` 一个查表的键，和命令号一致。
const DLG_ID: u32 = CMD_BACKUP_HISTORY;

const ID_FILES: i32 = 1300;
const ID_VERSIONS: i32 = 1301;
const ID_PREVIEW: i32 = 1302;
const ID_STATUS: i32 = 1303;
const ID_RESTORE: i32 = 1304;
const ID_SAVEAS: i32 = 1305;
const ID_DISCARD: i32 = 1306;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_STATIC: u16 = 0x0082;
const ATOM_LISTBOX: u16 = 0x0083;

/// 三栏：文件 / 版本 / 预览，底下一行状态和四个按钮。
const ITEMS: &[Item] = &[
    Item { id: ID_FILES, class: Class::Atom(ATOM_LISTBOX), style: 0x50a1_0101, x: 6, y: 6, w: 140, h: 168, label: "" },
    Item { id: ID_VERSIONS, class: Class::Atom(ATOM_LISTBOX), style: 0x50a1_0101, x: 150, y: 6, w: 120, h: 168, label: "" },
    Item { id: ID_PREVIEW, class: Class::Atom(ATOM_EDIT), style: 0x50a1_0844, x: 274, y: 6, w: 120, h: 168, label: "" },
    Item { id: ID_STATUS, class: Class::Atom(ATOM_STATIC), style: 0x5002_0000, x: 6, y: 178, w: 388, h: 10, label: "" },
    Item { id: ID_RESTORE, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 6, y: 194, w: 60, h: 15, label: "恢复(&R)" },
    Item { id: ID_SAVEAS, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 70, y: 194, w: 60, h: 15, label: "另存为(&A)..." },
    Item { id: ID_DISCARD, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 134, y: 194, w: 60, h: 15, label: "丢弃(&D)" },
    Item { id: 2, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 334, y: 194, w: 60, h: 15, label: "关闭(&C)" },
];

fn build_template() -> Vec<u8> {
    dlgtpl::build_legacy(DLG_TITLE, DLG_STYLE, 0, DLG_W, DLG_H, 400, ITEMS)
}

struct Dlg {
    entries: Vec<FileEntry>,
    restore: Option<(PathBuf, u32)>,
    destroyed: bool,
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
    font: HFONT,
}

unsafe fn state(dlg: HWND) -> &'static mut Dlg {
    &mut *(GetWindowLongPtrW(dlg, GWLP_USERDATA) as *mut Dlg)
}

unsafe fn set_text(dlg: HWND, id: i32, text: &str) {
    if let Ok(ctl) = GetDlgItem(Some(dlg), id) {
        let mut w: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let _ = SetWindowTextW(ctl, PCWSTR(w.as_mut_ptr()));
    }
}

unsafe fn list_sel(list: HWND) -> i32 {
    SendMessageW(list, LB_GETCURSEL, None, None).0 as i32
}

unsafe fn list_add(list: HWND, text: &str, data: usize) {
    let mut w: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let idx = SendMessageW(
        list,
        LB_ADDSTRING,
        Some(WPARAM(0)),
        Some(LPARAM(w.as_mut_ptr() as isize)),
    )
    .0;
    if idx >= 0 {
        let _ = SendMessageW(
            list,
            LB_SETITEMDATA,
            Some(WPARAM(idx as usize)),
            Some(LPARAM(data as isize)),
        );
    }
}

unsafe fn list_clear(list: HWND) {
    SendMessageW(list, LB_RESETCONTENT, None, None);
}

/// 选中某份文件之后：版本列表、预览、状态行一起换。
unsafe fn show_file(dlg: HWND) {
    let st = state(dlg);
    let files = GetDlgItem(Some(dlg), ID_FILES).unwrap_or_default();
    let Some(entry) = st.entries.get(list_sel(files).max(0) as usize) else {
        return;
    };
    let versions = GetDlgItem(Some(dlg), ID_VERSIONS).unwrap_or_default();
    list_clear(versions);
    for (index, meta) in entry.versions.iter().rev() {
        let kind = match meta.kind {
            Kind::Auto => "自动",
            Kind::Presave => "保存前",
        };
        let label = format!(
            "{}  {}  {}",
            local_time_text(meta.time),
            kind,
            human_size(meta.size)
        );
        list_add(versions, &label, *index as usize);
    }
    let _ = SendMessageW(versions, LB_SETCURSEL, Some(WPARAM(0)), None);
    show_version(dlg);
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// 选中某个版本之后：预览与状态行。
unsafe fn show_version(dlg: HWND) {
    let st = state(dlg);
    let files = GetDlgItem(Some(dlg), ID_FILES).unwrap_or_default();
    let Some(entry) = st.entries.get(list_sel(files).max(0) as usize) else {
        return;
    };
    let versions = GetDlgItem(Some(dlg), ID_VERSIONS).unwrap_or_default();
    let vsel = list_sel(versions).max(0) as usize;
    let Some((index, meta)) = entry.versions.iter().rev().nth(vsel) else {
        set_text(dlg, ID_STATUS, "");
        set_text(dlg, ID_PREVIEW, "");
        return;
    };
    let kind = match meta.kind {
        Kind::Auto => "自动备份",
        Kind::Presave => "保存前快照",
    };
    let note = if entry.path.is_empty() {
        "未命名文档"
    } else if entry.changed {
        "原文件自备份后已修改"
    } else {
        "原文件未改动"
    };
    let status = format!("{kind} · {} · {note}", local_time_text(meta.time));
    set_text(dlg, ID_STATUS, &status);
    set_text(dlg, ID_PREVIEW, &preview_text(&entry.dir, *index, meta));
}

/// 丢掉某份文件里还没进磁盘的自动备份（历史版本不动）。
unsafe fn discard_pending(dlg: HWND) {
    let st = state(dlg);
    let files = GetDlgItem(Some(dlg), ID_FILES).unwrap_or_default();
    let fsel = list_sel(files).max(0) as usize;
    let Some(entry) = st.entries.get_mut(fsel) else {
        return;
    };
    entry.versions.retain(|(index, meta)| {
        if meta.kind == Kind::Auto {
            let _ = std::fs::remove_file(entry.dir.join(format!("v{index:04}.dgab")));
            let _ = std::fs::remove_file(entry.dir.join(format!("v{index:04}.ini")));
            false
        } else {
            true
        }
    });
    entry.pending = false;
    show_file(dlg);
}

/// 另存为：原样复制字节，不重新编码。
unsafe fn save_as(dlg: HWND) {
    let st = state(dlg);
    let files = GetDlgItem(Some(dlg), ID_FILES).unwrap_or_default();
    let fsel = list_sel(files).max(0) as usize;
    let Some(entry) = st.entries.get(fsel) else {
        return;
    };
    let versions = GetDlgItem(Some(dlg), ID_VERSIONS).unwrap_or_default();
    let vsel = list_sel(versions).max(0) as usize;
    let Some((index, _)) = entry.versions.iter().rev().nth(vsel) else {
        return;
    };
    let name = entry.name();
    let stem = Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.clone());
    let ext = Path::new(&name)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let default = format!("{stem}.v{index}{ext}");
    let Some(target) = crate::win32::window::file_dialog_free(
        dlg,
        true,
        "另存为",
        "All Files\0*.*\0",
        Some(&default),
    ) else {
        return;
    };
    let src = entry.dir.join(format!("v{index:04}.dgab"));
    if std::fs::copy(&src, &target).is_ok() {
        set_text(dlg, ID_STATUS, &format!("已另存为 {target}"));
    }
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            let st = state(hwnd);
            let font = st.font;
            let mut child = GetWindow(hwnd, GW_CHILD).unwrap_or_default();
            while !child.0.is_null() {
                let _ = SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
                child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
            }
            crate::win32::lang::localize_dialog(hwnd, DLG_ID);
            let files = GetDlgItem(Some(hwnd), ID_FILES).unwrap_or_default();
            for (i, entry) in st.entries.iter().enumerate() {
                let label = format!("{}  ({} 版)", entry.name(), entry.versions.len());
                list_add(files, &label, i);
            }
            if !st.entries.is_empty() {
                let _ = SendMessageW(files, LB_SETCURSEL, Some(WPARAM(0)), None);
                show_file(hwnd);
            }
            1
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let notif = (wparam.0 >> 16) as i32;
            match id {
                ID_FILES if notif == 1 => {
                    show_file(hwnd);
                    1
                }
                ID_VERSIONS if notif == 1 => {
                    show_version(hwnd);
                    1
                }
                ID_RESTORE => {
                    let st = state(hwnd);
                    let files = GetDlgItem(Some(hwnd), ID_FILES).unwrap_or_default();
                    let fsel = list_sel(files).max(0) as usize;
                    let versions = GetDlgItem(Some(hwnd), ID_VERSIONS).unwrap_or_default();
                    let vsel = list_sel(versions).max(0) as usize;
                    if let Some(entry) = st.entries.get(fsel) {
                        if let Some((index, _)) = entry.versions.iter().rev().nth(vsel) {
                            st.restore = Some((entry.dir.clone(), *index));
                        }
                    }
                    let _ = DestroyWindow(hwnd);
                    1
                }
                ID_SAVEAS => {
                    save_as(hwnd);
                    1
                }
                ID_DISCARD => {
                    discard_pending(hwnd);
                    1
                }
                2 => {
                    let _ = DestroyWindow(hwnd);
                    1
                }
                _ => 0,
            }
        }
        WM_ERASEBKGND => {
            let st = state(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let st = state(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.field_face, st.field)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            state(hwnd).destroyed = true;
            PostQuitMessage(0);
            1
        }
        _ => 0,
    }
}

/// 打开「备份与历史」。返回要恢复的那一版（目录 + 版本号），`None` 表示
/// 用户只是看了看。另存为和丢弃在对话框里就地做完，不惊动编辑器。
pub(super) fn run_dialog(
    frame: HWND,
    theme: &dg_render::theme::Theme,
    root: &Path,
) -> Option<(PathBuf, u32)> {
    use crate::win32::view::instance;
    use windows::Win32::UI::WindowsAndMessaging::{CreateDialogIndirectParamW, DLGTEMPLATE};

    let entries = scan(root);
    if entries.is_empty() {
        return None;
    }
    let mut st = Dlg {
        entries,
        restore: None,
        destroyed: false,
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        ink: to_colorref(theme.toolbar_fg),
        face: to_colorref(theme.toolbar_bg),
        field_face: to_colorref(theme.toolbar_field),
        font: unsafe { super::uifont::create(dpi_of(frame)) },
    };
    let param = LPARAM(&mut st as *mut Dlg as isize);
    let tmpl = build_template();
    let dlg = unsafe {
        CreateDialogIndirectParamW(
            Some(instance()),
            &*(tmpl.as_ptr() as *const DLGTEMPLATE),
            Some(frame),
            Some(dlg_proc),
            param,
        )
    };
    let Ok(dlg) = dlg else {
        unsafe {
            let _ = DeleteObject(st.bg.into());
            let _ = DeleteObject(st.field.into());
            let _ = DeleteObject(st.font.into());
        }
        return None;
    };
    unsafe {
        let dark = chrome::theme_is_dark(theme);
        chrome::set_chrome_theme(theme);
        chrome::dark_theme_children(dlg, dark);
        chrome::title_bar(dlg, dark);
        show_modal(dlg, frame);
        run_modal_loop(dlg, frame, std::ptr::addr_of!(st.destroyed));
        let _ = DeleteObject(st.bg.into());
        let _ = DeleteObject(st.field.into());
        let _ = DeleteObject(st.font.into());
    }
    st.restore
}

fn dpi_of(hwnd: HWND) -> i32 {
    unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) as i32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dogedit-backup-{tag}-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn key_is_stable_and_case_folded() {
        let a = key_for_path(r"C:\Work\a.txt");
        let b = key_for_path(r"c:\work\A.TXT");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn same_name_different_dir_does_not_collide() {
        assert_ne!(
            key_for_path(r"C:\one\a.txt"),
            key_for_path(r"C:\two\a.txt")
        );
    }

    #[test]
    fn untitled_keys_differ() {
        assert_ne!(key_for_untitled(1), key_for_untitled(1));
        assert!(key_for_untitled(1).starts_with('u'));
    }

    #[test]
    fn doc_dir_shards_by_prefix() {
        let root = Path::new(r"C:\backup");
        let key = key_for_path(r"C:\a.txt");
        let dir = doc_dir(root, &key);
        assert_eq!(dir.parent().unwrap(), root.join(&key[..2]));
        assert_eq!(dir.file_name().unwrap().to_str().unwrap(), key);
    }

    #[test]
    fn meta_round_trips() {
        let meta = Meta {
            time: 1_730_000_123,
            kind: Kind::Presave,
            size: 18_342,
            hash: 0x9f2c_1234_5678_9abc,
            rev: 1042,
            base: Some((18_001, 1_730_000_000)),
            encoding: String::from("UTF-8"),
            bom: true,
            eol: 2,
            caret: (120, 7),
        };
        let back = Meta::parse(&meta.to_text()).unwrap();
        assert_eq!(back.time, meta.time);
        assert_eq!(back.kind, meta.kind);
        assert_eq!(back.size, meta.size);
        assert_eq!(back.hash, meta.hash);
        assert_eq!(back.rev, meta.rev);
        assert_eq!(back.base, meta.base);
        assert_eq!(back.encoding, meta.encoding);
        assert_eq!(back.bom, meta.bom);
        assert_eq!(back.eol, meta.eol);
        assert_eq!(back.caret, meta.caret);
    }

    #[test]
    fn writing_versions_rotates() {
        let root = temp_root("rotate");
        let key = key_for_untitled(1);
        // 每次现造一份：`encoding` 是 String，用 `..info` 会把它移走。
        let info = |rev: usize| WriteInfo {
            kind: Kind::Auto,
            path: "",
            title: "无标题1",
            caret: (0, 0),
            rev,
            eol: 2,
            encoding: String::from("UTF-8"),
            bom: false,
        };
        for i in 0..3 {
            // 每版内容都不一样，否则会走去重那条路。
            let doc = Document::from_text(&format!("hello {i}\n"));
            assert!(matches!(
                write_version(&root, &key, &doc, &info(i), 2),
                Some(Written::New(_))
            ));
        }
        let dir = doc_dir(&root, &key);
        assert_eq!(list_versions(&dir).len(), 2);
        // 最新的两版是 v0002、v0003
        let left: Vec<u32> = list_versions(&dir).iter().map(|(i, _)| *i).collect();
        assert_eq!(left, vec![2, 3]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn identical_content_does_not_add_a_version() {
        let root = temp_root("dedup");
        let doc = Document::from_text("same\n");
        let info = WriteInfo {
            kind: Kind::Auto,
            path: "",
            title: "",
            caret: (0, 0),
            rev: 1,
            eol: 2,
            encoding: String::from("UTF-8"),
            bom: false,
        };
        let key = key_for_untitled(2);
        assert!(matches!(
            write_version(&root, &key, &doc, &info, 10),
            Some(Written::New(1))
        ));
        let info2 = WriteInfo { rev: 2, ..info };
        assert!(matches!(
            write_version(&root, &key, &doc, &info2, 10),
            Some(Written::Same)
        ));
        assert_eq!(list_versions(&doc_dir(&root, &key)).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn content_is_written_verbatim() {
        let root = temp_root("bytes");
        let doc = Document::from_text("a\r\nb\n");
        let info = WriteInfo {
            kind: Kind::Presave,
            path: "",
            title: "",
            caret: (1, 0),
            rev: 7,
            eol: 2,
            encoding: String::from("UTF-8"),
            bom: false,
        };
        let key = key_for_untitled(3);
        write_version(&root, &key, &doc, &info, 10).unwrap();
        let bytes = std::fs::read(doc_dir(&root, &key).join("v0001.dgab")).unwrap();
        assert_eq!(bytes, b"a\r\nb\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 未命名文档也要有 `doc.ini`：没有它，对话框只能按目录名认人，恢复出来的
    /// 标签也没有标题可还原。
    #[test]
    fn untitled_version_records_its_title() {
        let root = temp_root("untitled");
        let doc = Document::from_text("scratch\n");
        let info = WriteInfo {
            kind: Kind::Auto,
            path: "",
            title: "无标题",
            caret: (0, 7),
            rev: 1,
            eol: 2,
            encoding: String::from("UTF-8"),
            bom: false,
        };
        let key = key_for_untitled(9);
        assert!(matches!(
            write_version(&root, &key, &doc, &info, 10),
            Some(Written::New(1))
        ));
        let dir = doc_dir(&root, &key);
        let meta = read_doc_meta(&dir).unwrap();
        assert!(meta.path.is_empty());
        assert_eq!(meta.title, "无标题");
        // 扫描要按标题认出它，而不是给一个空名字。
        let entries = scan(&root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), "无标题");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 三个上限都来自选项页，`gc` 是它们唯一生效的地方。
    fn gc_info(rev: usize) -> WriteInfo<'static> {
        WriteInfo {
            kind: Kind::Auto,
            path: "",
            title: "无标题",
            caret: (0, 0),
            rev,
            eol: 2,
            encoding: String::from("UTF-8"),
            bom: false,
        }
    }

    #[test]
    fn gc_trims_each_file_to_the_configured_version_count() {
        let root = temp_root("gc-versions");
        let key = key_for_untitled(11);
        for i in 0..4 {
            let doc = Document::from_text(&format!("v{i}\n"));
            write_version(&root, &key, &doc, &gc_info(i), 10).unwrap();
        }
        let dir = doc_dir(&root, &key);
        assert_eq!(list_versions(&dir).len(), 4);
        // 用户把「每文件保留版本数」调到 2：剩下的该是最新的两版。
        gc(
            &root,
            Limits {
                versions: 2,
                keep: None,
                budget: None,
            },
        );
        let left: Vec<u32> = list_versions(&dir).iter().map(|(i, _)| *i).collect();
        assert_eq!(left, vec![3, 4]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gc_drops_a_directory_past_the_keep_window() {
        let root = temp_root("gc-age");
        let key = key_for_untitled(12);
        let doc = Document::from_text("old\n");
        write_version(&root, &key, &doc, &gc_info(1), 10).unwrap();
        let dir = doc_dir(&root, &key);
        // 把那一版的时间改成 40 天前：保留 30 天的设置就该把它清掉。
        let mut meta = list_versions(&dir)[0].1.clone();
        meta.time = now_secs() - 40 * 86_400;
        std::fs::write(dir.join("v0001.ini"), meta.to_text()).unwrap();
        gc(
            &root,
            Limits {
                versions: 10,
                keep: Some(Duration::from_secs(30 * 86_400)),
                budget: None,
            },
        );
        assert!(!dir.exists());
        // `keep: None`（用户填 0 = 永久保留）不能删它。
        let doc = Document::from_text("old\n");
        write_version(&root, &key, &doc, &gc_info(2), 10).unwrap();
        let mut meta = list_versions(&dir)[0].1.clone();
        meta.time = now_secs() - 400 * 86_400;
        std::fs::write(dir.join("v0001.ini"), meta.to_text()).unwrap();
        gc(
            &root,
            Limits {
                versions: 10,
                keep: None,
                budget: None,
            },
        );
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gc_trims_the_oldest_versions_to_fit_the_budget() {
        let root = temp_root("gc-budget");
        let key = key_for_untitled(13);
        // 每版 2 字节（内容各不同，否则走去重那条路），三版 6 字节；
        // 预算 2 字节只够留最新那一版。
        for (i, text) in ["a\n", "b\n", "c\n"].iter().enumerate() {
            let doc = Document::from_text(text);
            write_version(&root, &key, &doc, &gc_info(i), 10).unwrap();
        }
        let dir = doc_dir(&root, &key);
        assert_eq!(list_versions(&dir).len(), 3);
        gc(
            &root,
            Limits {
                versions: 10,
                keep: None,
                budget: Some(2),
            },
        );
        let left: Vec<u32> = list_versions(&dir).iter().map(|(i, _)| *i).collect();
        assert_eq!(left, vec![3]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
