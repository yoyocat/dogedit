//! 解析器下载的网络与缓存层（无 win32 依赖，纯逻辑 + 后台线程）。
//!
//! 「解析器下载」的数据面：
//!
//! * **来源** [`RegistrySource`]：`[Download] Registry` 的值 —— `http(s)://`
//!   开头是远端基址（Cloudflare Worker + 静态资源），否则是本地镜像目录
//!   （`registry.json` + `pkg/…`，与部署目录同布局，测试零网络）。
//! * **缓存** [`CacheFile`]：整表 + ETag + 负缓存（查过没有候选的扩展名
//!   记一个到期时刻，30 天内不再为它发请求），落
//!   `%LOCALAPPDATA%\DogEdit\parsercache.json`。
//! * **任务** [`NetJob`]/[`NetEvent`]：`spawn` 在 std 线程上跑一个任务，
//!   事件进 crossbeam channel，`wake` 闭包（调用方注入，通常是
//!   `PostMessageW(hwnd, WM_APP+n, …)`）负责把 UI 叫醒；取消一律是
//!   `Arc<AtomicBool>`（与查找对话框同一套约定）。
//!
//! 没有「后台自动」：只有用户开管理框刷新、点下载，或开了
//! `[Download] AutoQuery` 后首次打开未知扩展名文件时才发请求。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use dg_syntax::install::{install_package, manifest_from_zip};
use dg_syntax::{RegistryDoc, RemotePackage};
use serde::{Deserialize, Serialize};

/// 默认注册表基址（`deploy/parser-registry` 的线上地址）；
/// `[Download] Registry` 设置随时覆盖（也接受本地镜像目录路径）。
pub const DEFAULT_SOURCE: &str = "https://dogedit-parsers.xqtxyz.workers.dev";

/// 用户解析器的唯一可写落点，与 `scan_syntax_plugins` 的第三个根一致。
pub fn user_parsers_root() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|b| PathBuf::from(b).join("dogedit").join("parsers"))
}

/// 缓存文件：`%LOCALAPPDATA%\DogEdit\parsercache.json`。
pub fn cache_path() -> Option<PathBuf> {
    match std::env::var_os("LOCALAPPDATA") {
        Some(base) if !base.is_empty() => {
            Some(PathBuf::from(base).join("DogEdit").join("parsercache.json"))
        }
        _ => std::env::temp_dir().join("DogEdit").join("parsercache.json").into(),
    }
}

/// 负缓存有效期（秒）：查过没有候选的扩展名，30 天内不再发查询。
pub const NEGATIVE_TTL_SECS: u64 = 30 * 24 * 3600;
/// 整表大小上限。
const REGISTRY_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 包 zip 大小上限（哈希校验才是真闸，这里只拦离谱的）。
const PACKAGE_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// 注册表来源：远端基址或本地镜像目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrySource {
    Remote { base: String },
    Local { dir: PathBuf },
}

impl RegistrySource {
    /// `[Download] Registry` → 来源。空值用 [`DEFAULT_SOURCE`]。
    pub fn from_setting(value: &str) -> Self {
        let v = value.trim();
        if v.is_empty() {
            return Self::from_base(DEFAULT_SOURCE);
        }
        if v.starts_with("http://") || v.starts_with("https://") {
            Self::from_base(v)
        } else {
            Self::Local { dir: PathBuf::from(v) }
        }
    }

    fn from_base(base: &str) -> Self {
        Self::Remote {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    /// 缓存里的来源键：换源 = 缓存作废（ETag、负缓存都不沿用）。
    fn key(&self) -> String {
        match self {
            Self::Remote { base } => base.clone(),
            Self::Local { dir } => dir.to_string_lossy().to_string(),
        }
    }

    /// 管理框「来源」行显示的文字。
    pub fn describe(&self) -> String {
        match self {
            Self::Remote { base } => base.clone(),
            Self::Local { dir } => dir.display().to_string(),
        }
    }

    fn registry_url(&self) -> String {
        match self {
            Self::Remote { base } => format!("{base}/registry.json"),
            Self::Local { dir } => dir.join("registry.json").to_string_lossy().to_string(),
        }
    }

    fn package_path(&self, rel: &str) -> PathBuf {
        match self {
            Self::Remote { .. } => PathBuf::from(rel),
            Self::Local { dir } => dir.join(rel),
        }
    }

    fn package_url(&self, rel: &str) -> String {
        match self {
            Self::Remote { base } => format!("{base}/{rel}"),
            Self::Local { dir } => dir.join(rel).to_string_lossy().to_string(),
        }
    }
}

/// 整表 + ETag + 负缓存的落盘形态。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheFile {
    pub schema: u32,
    /// 来源键（[`RegistrySource::key`]），换源即作废。
    pub source: String,
    #[serde(default)]
    pub etag: String,
    /// 最近一次成功整表的 epoch 秒。
    pub fetched_at: u64,
    pub registry: RegistryDoc,
    /// 扩展名 → 查无候选的到期时刻（epoch 秒）。
    #[serde(default)]
    pub negative: BTreeMap<String, u64>,
}

pub const CACHE_SCHEMA: u32 = 1;

impl CacheFile {
    fn new(source: &RegistrySource, registry: RegistryDoc, etag: String) -> Self {
        Self {
            schema: CACHE_SCHEMA,
            source: source.key(),
            etag,
            fetched_at: now_secs(),
            registry,
            negative: BTreeMap::new(),
        }
    }

    /// 缓存的整表是否属于这个来源、还能拿 ETag 条件请求。
    fn matches(&self, source: &RegistrySource) -> bool {
        self.schema == CACHE_SCHEMA && self.source == source.key()
    }

    /// 扩展名候选：ABI 过滤后的远端条目（首开提醒与提示框共用）。
    pub fn candidates(&self, ext: &str, host_abi: (u32, u32)) -> Vec<RemotePackage> {
        self.registry.candidates_for_ext(ext, host_abi)
    }

    /// 这个扩展名的负缓存是否还没到期（到期了就允许再查一次）。
    pub fn is_negative(&self, ext: &str) -> bool {
        self.negative.get(ext).is_some_and(|&until| now_secs() < until)
    }

    /// 记一条负缓存。
    pub fn record_negative(&mut self, ext: &str) {
        self.negative.insert(ext.to_string(), now_secs() + NEGATIVE_TTL_SECS);
    }
}

pub fn load_cache(path: &Path) -> Option<CacheFile> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    serde_json::from_value(json).ok()
}

/// 读缓存，只认属于这个来源的（换源 = 作废）。首屏列表与首开提醒都走它。
pub fn cached_for(source: &RegistrySource) -> Option<CacheFile> {
    cache_path()
        .and_then(|p| load_cache(&p))
        .filter(|c| c.matches(source))
}

pub fn save_cache(path: &Path, cache: &CacheFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(cache).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// 远端整表拉取结果。`Unchanged` = 304，调用方继续用现有缓存。
pub enum FetchResult {
    Fresh(Box<CacheFile>),
    Unchanged,
}

/// 拉一次整表：远端走 ETag 条件请求，本地直接读盘。成功就落缓存。
pub fn fetch_registry(
    source: &RegistrySource,
    cache_path: &Path,
    cancel: &AtomicBool,
) -> Result<FetchResult, String> {
    let old = load_cache(cache_path).filter(|c| c.matches(source));
    let (text, etag, unchanged) = match source {
        RegistrySource::Local { dir } => {
            let p = dir.join("registry.json");
            (std::fs::read_to_string(&p).map_err(|e| format!("读取 {} 失败: {e}", p.display()))?, String::new(), false)
        }
        RegistrySource::Remote { base: _ } => {
            let etag = old.as_ref().map(|c| c.etag.clone()).unwrap_or_default();
            http_get(&source.registry_url(), if etag.is_empty() { None } else { Some(&etag) }, REGISTRY_MAX_BYTES, cancel)?
        }
    };
    if unchanged {
        return Ok(FetchResult::Unchanged);
    }
    let doc = RegistryDoc::parse(&text)?;
    let mut cache = CacheFile::new(source, doc, etag);
    // 负缓存的答案是「当时的整表」给的：这次拉到了新内容，旧答案可能
    // 不成立，作废重挣；304（内容没变）在上面早退，负缓存原地保留。
    cache.negative = Default::default();
    save_cache(cache_path, &cache)?;
    Ok(FetchResult::Fresh(Box::new(cache)))
}

/// 首开提醒的门槛：这个扩展名值不值得为它打扰用户。明显没有解析器的
/// 常见杂项后缀（`.txt`/`.log`、备份与临时文件这些）直接静默 —— 查询
/// 也是白查，负缓存还得占一行。带符号的写法（`.c++`/`.h++`）是正经
/// 后缀，放行：查询参数有百分号编码，服务端能原样匹配。命中即整条
/// 链路跳过：不提示、不查询、不记负缓存。
pub fn should_ask_for_ext(ext: &str) -> bool {
    const NEVER_ASK: [&str; 23] = [
        "bak", "cache", "crdownload", "download", "err", "lock", "log", "nfo", "old", "orig",
        "out", "part", "partial", "pid", "rtf", "sav", "save", "swp", "swo", "temp", "text",
        "tmp", "txt",
    ];
    let ext = ext.to_ascii_lowercase();
    !ext.is_empty() && !NEVER_ASK.contains(&ext.as_str())
}

/// 单扩展名查询（首开提醒）：远端走 `/api/query?ext=`，本地直接过滤镜像。
/// 查完空手而归就记负缓存 —— 同一个扩展名不问第二遍。
pub fn query_ext(
    source: &RegistrySource,
    cache_path: &Path,
    ext: &str,
    cancel: &AtomicBool,
) -> Result<Vec<RemotePackage>, String> {
    let packages = match source {
        RegistrySource::Local { dir } => {
            let p = dir.join("registry.json");
            let text =
                std::fs::read_to_string(&p).map_err(|e| format!("读取 {} 失败: {e}", p.display()))?;
            RegistryDoc::parse(&text)?.candidates_for_ext(ext, host_abi())
        }
        RegistrySource::Remote { base } => {
            let url = format!("{base}/api/query?ext={}", percent_encode(ext));
            let (text, _, _) = http_get(&url, None, REGISTRY_MAX_BYTES, cancel)?;
            let resp: QueryResponse =
                serde_json::from_str(&text).map_err(|e| format!("query 响应解析失败: {e}"))?;
            // ABI 是本机属性，服务端不知道 —— 客户端再滤一遍。
            resp.results
                .into_iter()
                .filter(|p| dg_syntax::remote::abi_compatible(p.abi, host_abi()))
                .collect()
        }
    };
    if packages.is_empty() {
        record_negative(cache_path, source, ext);
    }
    Ok(packages)
}

/// query 参数的最小百分号编码：保留字母数字与 `-._~`，其余逐字节转义。
/// `+` 不转的话，服务端 query 解析会把它当空格（`c++` 变 `c  `）。
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 记一条负缓存：已有同源缓存就在上面改（保住整表与 ETag），没有就现建
/// 一份只含负缓存的 —— 首开提醒往往发生在第一次拉整表之前。
fn record_negative(cache_path: &Path, source: &RegistrySource, ext: &str) {
    let mut cache = load_cache(cache_path)
        .filter(|c| c.matches(source))
        .unwrap_or_else(|| CacheFile::new(source, RegistryDoc::default(), String::new()));
    cache.record_negative(ext);
    let _ = save_cache(cache_path, &cache);
}

/// `/api/query` 的响应形状（worker.js 的输出）。
#[derive(Deserialize)]
struct QueryResponse {
    #[serde(default)]
    #[allow(dead_code)]
    query: String,
    #[serde(default)]
    results: Vec<RemotePackage>,
}

/// 本机 tree-sitter 兼容 ABI 区间（与 dynlang 加载校验同一套常量）。
pub fn host_abi() -> (u32, u32) {
    (
        tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION as u32,
        tree_sitter::LANGUAGE_VERSION as u32,
    )
}

/// 下载一个包并装进 `root`：进度经 `on_progress` 上报（返回 false = 取消），
/// 落盘前先过 sha256。
pub fn download_and_install(
    source: &RegistrySource,
    pkg: &RemotePackage,
    root: &Path,
    on_progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let bytes = match source {
        RegistrySource::Local { dir: _ } => {
            let p = source.package_path(&pkg.path);
            let data = std::fs::read(&p).map_err(|e| format!("读取 {} 失败: {e}", p.display()))?;
            let _ = on_progress(data.len() as u64, Some(data.len() as u64));
            data
        }
        RegistrySource::Remote { base: _ } => {
            let url = source.package_url(&pkg.path);
            let agent = http_agent();
            let resp = agent
                .get(&url)
                .call()
                .map_err(ureq_err("下载包失败"))?;
            let total = resp
                .header("Content-Length")
                .and_then(|s| s.parse::<u64>().ok());
            let mut reader = resp.into_reader();
            read_capped(&mut reader, PACKAGE_MAX_BYTES, cancel, &mut |done| on_progress(done, total))
                .map_err(|e| format!("下载 {} 失败: {e}", pkg.path))?
        }
    };
    if bytes.len() as u64 != pkg.size {
        return Err(format!(
            "包大小不符：注册表声明 {} 字节，实际 {} 字节",
            pkg.size,
            bytes.len()
        ));
    }
    install_package(root, &pkg.name, &bytes, Some(&pkg.sha256)).map_err(|e| e.to_string())
}

/// 装完主包后，把它 query 里 `; inherits:` 声明的父包也装上（尽力而为）。
///
/// 父包按名字在同一份注册表里找；已经落在 `root` 下的跳过。父包可能还
/// 继承别的包，最多跟四层 —— Helix 的继承链没有那么深。
fn install_query_parents(
    source: &RegistrySource,
    cache_path: &Path,
    root: &Path,
    installed: &Path,
    on_progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    cancel: &AtomicBool,
) {
    let mut wanted = package_inherits(installed);
    if wanted.is_empty() {
        return;
    }
    let Some(registry) = cached_registry(source, cache_path, cancel) else {
        return;
    };
    for _ in 0..4 {
        let mut next: Vec<String> = Vec::new();
        for name in wanted {
            if root.join(&name).join("manifest.toml").exists() {
                continue;
            }
            let Some(pkg) = registry.find(&name) else {
                continue;
            };
            if let Ok(dir) = download_and_install(source, pkg, root, on_progress, cancel) {
                next.extend(package_inherits(&dir));
            }
        }
        if next.is_empty() {
            break;
        }
        wanted = next;
    }
}

/// 一份已装包的 query 里出现过的 `; inherits:` 名字（去重、保持出现顺序）。
fn package_inherits(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.join("queries")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("scm") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for name in dg_syntax::query_inherits(&text) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// 注册表：同源缓存优先，没有就现拉一次。拉不到返回 `None`（调用方放行）。
fn cached_registry(
    source: &RegistrySource,
    cache_path: &Path,
    cancel: &AtomicBool,
) -> Option<RegistryDoc> {
    if let Some(cache) = load_cache(cache_path).filter(|c| c.matches(source)) {
        return Some(cache.registry);
    }
    match fetch_registry(source, cache_path, cancel) {
        Ok(FetchResult::Fresh(c)) => Some(c.registry),
        Ok(FetchResult::Unchanged) => load_cache(cache_path).map(|c| c.registry),
        Err(_) => None,
    }
}

/// 「从本地文件安装」：读 zip → 从 manifest 认包名 → 免哈希安装。
/// 返回 (包名, 版本)（管理框刷新行用）。
pub fn install_local_file(root: &Path, zip_path: &Path) -> Result<(String, String), String> {
    let bytes = std::fs::read(zip_path)
        .map_err(|e| format!("读取 {} 失败: {e}", zip_path.display()))?;
    if bytes.len() as u64 > PACKAGE_MAX_BYTES {
        return Err(format!("文件超过 {} 字节上限", PACKAGE_MAX_BYTES));
    }
    let (name, version) = manifest_from_zip(&bytes).map_err(|e| e.to_string())?;
    install_package(root, &name, &bytes, None).map_err(|e| e.to_string())?;
    Ok((name, version))
}

/// 一个后台任务。`spawn` 每次开一条 std 线程跑完即退。
pub enum NetJob {
    /// 整表刷新（管理框的「刷新」）。
    FetchRegistry,
    /// 单扩展名查询（首开提醒）。
    QueryExt { ext: String },
    /// 下载 + 安装一个注册表包。
    Install { pkg: Box<RemotePackage> },
    /// 从本地 zip 安装。
    InstallLocal { zip: PathBuf },
}

/// 任务结果事件。UI 侧按 token/窗口各自认领。
pub enum NetEvent {
    Registry(Result<FetchResult, String>),
    Query { ext: String, result: Result<Vec<RemotePackage>, String> },
    InstallDone { name: String, result: Result<PathBuf, String> },
    InstallLocalDone { zip: PathBuf, result: Result<(String, String), String> },
    /// 下载进度（done = 已收字节，total = Content-Length，可能未知）。
    Progress { done: u64, total: Option<u64> },
}

/// 把一个任务丢到后台线程。事件进 `tx`，每条事件后调一次 `wake`；下载
/// 进度按 100ms 节流唤醒，免得消息队列被刷爆。
pub fn spawn(
    job: NetJob,
    source: RegistrySource,
    cache_path: PathBuf,
    install_root: PathBuf,
    tx: Sender<NetEvent>,
    wake: impl Fn() + Send + 'static,
    cancel: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("parser-download".into())
        .spawn(move || run_job(job, source, cache_path, install_root, &tx, &wake, &cancel))
        .ok();
}

fn run_job(
    job: NetJob,
    source: RegistrySource,
    cache_path: PathBuf,
    install_root: PathBuf,
    tx: &Sender<NetEvent>,
    wake: &(impl Fn() + Send + ?Sized),
    cancel: &AtomicBool,
) {
    let mut last_wake = std::time::Instant::now();
    match job {
        NetJob::FetchRegistry => {
            let result = fetch_registry(&source, &cache_path, cancel);
            let _ = tx.send(NetEvent::Registry(result));
            wake();
        }
        NetJob::QueryExt { ext } => {
            let result = query_ext(&source, &cache_path, &ext, cancel);
            let _ = tx.send(NetEvent::Query { ext, result });
            wake();
        }
        NetJob::Install { pkg } => {
            let name = pkg.name.clone();
            let mut progress = |done, total| {
                // 事件每块都发（channel 便宜），唤醒节流 —— UI drain 时
                // 反正只看最后一条。
                let _ = tx.send(NetEvent::Progress { done, total });
                if last_wake.elapsed() >= Duration::from_millis(100) {
                    last_wake = std::time::Instant::now();
                    wake();
                }
                !cancel.load(Ordering::Relaxed)
            };
            let result = download_and_install(&source, &pkg, &install_root, &mut progress, cancel)
                .map(|dir| {
                    // `; inherits:` 只是名字，父语言的 query 不在这个包里：
                    // 不补装，C++ 这类就只有自家那几十条规则。父包按同一份
                    // 注册表补，失败不算主包失败 —— 少一半颜色也比装不上强。
                    install_query_parents(
                        &source,
                        &cache_path,
                        &install_root,
                        &dir,
                        &mut progress,
                        cancel,
                    );
                    dir
                });
            let _ = tx.send(NetEvent::InstallDone { name, result });
            wake();
        }
        NetJob::InstallLocal { zip } => {
            let result = install_local_file(&install_root, &zip).map(|(name, version)| {
                let dir = install_root.join(&name);
                let mut progress = |_, _| !cancel.load(Ordering::Relaxed);
                install_query_parents(
                    &source,
                    &cache_path,
                    &install_root,
                    &dir,
                    &mut progress,
                    cancel,
                );
                (name, version)
            });
            let _ = tx.send(NetEvent::InstallLocalDone { zip, result });
            wake();
        }
    }
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .user_agent("dogedit-parser-download")
        .build()
}

/// GET 一个 URL，返回 (正文, ETag)。`unchanged = true` 表示 304。
#[allow(clippy::type_complexity)]
fn http_get(
    url: &str,
    etag: Option<&str>,
    cap: u64,
    cancel: &AtomicBool,
) -> Result<(String, String, bool), String> {
    let agent = http_agent();
    let mut req = agent.get(url);
    if let Some(tag) = etag {
        req = req.set("If-None-Match", tag);
    }
    match req.call() {
        Ok(resp) => {
            // ureq 只把 ≥400 抬成 Err，304 是正常的 Ok(resp)（空体）——
            // 在这里认。
            if resp.status() == 304 {
                return Ok((String::new(), String::new(), true));
            }
            let etag = resp.header("ETag").unwrap_or("").to_string();
            let mut reader = resp.into_reader();
            let bytes =
                read_capped(&mut reader, cap, cancel, &mut |_| true).map_err(|e| e.to_string())?;
            String::from_utf8(bytes).map_err(|_| "响应不是 UTF-8".to_string()).map(|t| (t, etag, false))
        }
        Err(ureq::Error::Status(304, _)) => Ok((String::new(), String::new(), true)),
        Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
        Err(e) => Err(ureq_err("请求失败")(e)),
    }
}

fn ureq_err(prefix: &'static str) -> impl Fn(ureq::Error) -> String {
    move |e| match e {
        ureq::Error::Status(code, _) => format!("{prefix}: HTTP {code}"),
        ureq::Error::Transport(t) => format!("{prefix}: {}", t),
    }
}

/// 流式读到上限为止；`on_chunk` 返回 false 中途放弃（取消）。
fn read_capped(
    reader: &mut dyn Read,
    cap: u64,
    cancel: &AtomicBool,
    on_chunk: &mut dyn FnMut(u64) -> bool,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() as u64 > cap {
            return Err(format!("响应超过 {cap} 字节上限"));
        }
        if !on_chunk(out.len() as u64) {
            return Err("已取消".into());
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dg-download-{tag}-{}-{}",
            std::process::id(),
            now_secs() * 1000 + std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_millis() as u64
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 一个完整的本地镜像：registry.json + pkg/…zip（真包，能装）。
    fn mirror(tag: &str) -> (PathBuf, PathBuf) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        use zip::ZipWriter;
        let dir = tmp(tag);
        let pkg_dir = dir.join("pkg").join("demo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        w.start_file("manifest.toml", SimpleFileOptions::default()).unwrap();
        w.write_all(
            b"name = \"demo\"\ndisplay = \"Demo\"\nversion = \"1.0.0\"\nextensions = [\"dm\"]\n",
        )
        .unwrap();
        w.start_file("demo.dll", SimpleFileOptions::default()).unwrap();
        w.write_all(b"fake").unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let zip_path = pkg_dir.join("1.0.0.zip");
        std::fs::write(&zip_path, &bytes).unwrap();
        let sha = dg_syntax::install::sha256_hex(&bytes);
        std::fs::write(
            dir.join("registry.json"),
            format!(
                r#"{{"schema":1,"updated":"","packages":[{{"name":"demo","display":"Demo","version":"1.0.0","extensions":["dm"],"size":{size},"sha256":"{sha}","path":"pkg/demo/1.0.0.zip","description":"演示"}}]}}"#,
                size = bytes.len(),
            ),
        )
        .unwrap();
        (dir, zip_path)
    }

    #[test]
    fn setting_becomes_the_right_source() {
        assert_eq!(
            RegistrySource::from_setting("https://x.workers.dev/"),
            RegistrySource::Remote { base: "https://x.workers.dev".into() }
        );
        assert_eq!(
            RegistrySource::from_setting("D:\\mirror"),
            RegistrySource::Local { dir: "D:\\mirror".into() }
        );
        assert_eq!(
            RegistrySource::from_setting(""),
            RegistrySource::Remote { base: DEFAULT_SOURCE.into() }
        );
    }

    #[test]
    fn prompt_gate_passes_code_suffixes_and_blocks_noise() {
        // 常用后缀照问，带符号的 `.c++`/`.h++` 也是正经后缀。
        for ext in ["cpp", "rs", "py", "tfvars", "markdown", "c++", "h++"] {
            assert!(should_ask_for_ext(ext), "{ext} 该问");
        }
        // 明显没有解析器的杂项、空的：静默。
        for ext in ["txt", "log", "TXT", "bak", "tmp", "crdownload", ""] {
            assert!(!should_ask_for_ext(ext), "{ext} 不该问");
        }
    }

    #[test]
    fn percent_encode_escapes_query_unsafe_bytes() {
        assert_eq!(percent_encode("cpp"), "cpp");
        assert_eq!(percent_encode("c++"), "c%2B%2B");
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn local_mirror_fetch_query_and_install_roundtrip() {
        let (dir, zip_path) = mirror("roundtrip");
        let root = tmp("roundtrip-root");
        let cache_file = dir.join("cache.json");
        let source = RegistrySource::Local { dir: dir.clone() };
        let cancel = AtomicBool::new(false);
        // 整表 → 缓存里有一条、负缓存为空。
        let fresh = match fetch_registry(&source, &cache_file, &cancel).unwrap() {
            FetchResult::Fresh(c) => *c,
            FetchResult::Unchanged => panic!("首次不该是 Unchanged"),
        };
        assert_eq!(fresh.registry.packages.len(), 1);
        // 查询 → 命中 demo；查一个没有的 → 空 + 负缓存。
        assert_eq!(query_ext(&source, &cache_file, "dm", &cancel).unwrap().len(), 1);
        assert!(query_ext(&source, &cache_file, "zz", &cancel).unwrap().is_empty());
        let cache = load_cache(&cache_file).unwrap();
        assert!(cache.is_negative("zz"));
        assert!(!cache.is_negative("dm"));
        // 装包 → 注册表能扫到，sha256 过闸。
        let pkg = fresh.registry.packages[0].clone();
        let installed = download_and_install(&source, &pkg, &root, &mut |_, _| true, &cancel).unwrap();
        assert!(installed.join("demo.dll").exists());
        assert_eq!(cache.candidates("dm", host_abi()).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
        let _ = zip_path;
    }

    #[test]
    fn hash_mismatch_surfaces_from_registry_size_check() {
        let (dir, _) = mirror("sizegate");
        let root = tmp("sizegate-root");
        let cache_file = dir.join("cache.json");
        let source = RegistrySource::Local { dir: dir.clone() };
        let cancel = AtomicBool::new(false);
        assert!(matches!(
            fetch_registry(&source, &cache_file, &cancel).unwrap(),
            FetchResult::Fresh(_)
        ));
        // 篡改声明大小 → 大小闸先拦（本地镜像的哈希闸在 install 内，同样拦）。
        let mut cache = load_cache(&cache_file).unwrap();
        cache.registry.packages[0].size = 999;
        let pkg = cache.registry.packages[0].clone();
        let err = download_and_install(&source, &pkg, &root, &mut |_, _| true, &cancel).unwrap_err();
        assert!(err.contains("大小不符"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 装主包时按 `; inherits: base` 把父包一并装进同一个根 —— C++ 的
    /// 高亮规则大半在 C 包里，少了它等于没装。
    #[test]
    fn installing_a_package_pulls_in_its_query_parent() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let dir = tmp("inherit");
        let root = tmp("inherit-root");
        let mut entries = Vec::new();
        for (name, queries) in [
            ("base", "(string) @string\n"),
            ("leaf", "; inherits: base\n(comment) @comment\n"),
        ] {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            w.start_file("manifest.toml", SimpleFileOptions::default()).unwrap();
            w.write_all(
                format!(
                    "name = \"{name}\"\ndisplay = \"{name}\"\nversion = \"1.0.0\"\nextensions = [\"{name}\"]\n"
                )
                .as_bytes(),
            )
            .unwrap();
            w.start_file(format!("{name}.dll"), SimpleFileOptions::default()).unwrap();
            w.write_all(b"fake").unwrap();
            w.start_file("queries/highlights.scm", SimpleFileOptions::default()).unwrap();
            w.write_all(queries.as_bytes()).unwrap();
            let bytes = w.finish().unwrap().into_inner();
            let pkg_dir = dir.join("pkg").join(name);
            std::fs::create_dir_all(&pkg_dir).unwrap();
            std::fs::write(pkg_dir.join("1.0.0.zip"), &bytes).unwrap();
            entries.push(format!(
                r#"{{"name":"{name}","display":"{name}","version":"1.0.0","extensions":["{name}"],"size":{},"sha256":"{}","path":"pkg/{name}/1.0.0.zip"}}"#,
                bytes.len(),
                dg_syntax::install::sha256_hex(&bytes),
            ));
        }
        std::fs::write(
            dir.join("registry.json"),
            format!(
                r#"{{"schema":1,"updated":"","packages":[{}]}}"#,
                entries.join(",")
            ),
        )
        .unwrap();

        let source = RegistrySource::Local { dir: dir.clone() };
        let cache_file = dir.join("cache.json");
        let cancel = AtomicBool::new(false);
        let doc = match fetch_registry(&source, &cache_file, &cancel).unwrap() {
            FetchResult::Fresh(c) => *c,
            FetchResult::Unchanged => panic!("首次不该是 Unchanged"),
        };
        let leaf = doc.registry.find("leaf").expect("leaf 在注册表里");
        let installed =
            download_and_install(&source, leaf, &root, &mut |_, _| true, &cancel).unwrap();
        let mut progress = |_, _| true;
        install_query_parents(&source, &cache_file, &root, &installed, &mut progress, &cancel);
        assert!(
            root.join("base").join("manifest.toml").exists(),
            "父包应一并装上"
        );
        assert!(root.join("base").join("queries/highlights.scm").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_file_install_reads_the_name_from_the_zip() {
        let (dir, _) = mirror("localfile");
        let root = tmp("localfile-root");
        // 复用镜像里的 zip 当「用户选的文件」。
        let zip_path = dir.join("pkg").join("demo").join("1.0.0.zip");
        let (name, version) = install_local_file(&root, &zip_path).unwrap();
        assert_eq!(name, "demo");
        assert_eq!(version, "1.0.0");
        assert!(root.join("demo").join("demo.dll").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn negative_cache_expires() {
        let (dir, _) = mirror("negative");
        let cache_file = dir.join("cache.json");
        let source = RegistrySource::Local { dir: dir.clone() };
        let cancel = AtomicBool::new(false);
        // 首开提醒的真实顺序：还没有整表缓存就要落负缓存 —— 现建缓存文件。
        assert!(query_ext(&source, &cache_file, "zz", &cancel).unwrap().is_empty());
        let mut cache = load_cache(&cache_file).unwrap();
        assert!(cache.is_negative("zz"));
        // 过期即恢复可查。
        *cache.negative.get_mut("zz").unwrap() = now_secs() - 1;
        assert!(!cache.is_negative("zz"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_fetch_clears_stale_negatives() {
        let (dir, _) = mirror("neg-clear");
        let cache_file = dir.join("cache.json");
        let source = RegistrySource::Local { dir: dir.clone() };
        let cancel = AtomicBool::new(false);
        assert!(query_ext(&source, &cache_file, "zz", &cancel).unwrap().is_empty());
        assert!(load_cache(&cache_file).unwrap().is_negative("zz"));
        // 拉到新内容（本地镜像每次读盘都算）→ 旧负缓存作废；注册表换了
        // 内容，当时的「查无」可能已经不成立。
        assert!(matches!(fetch_registry(&source, &cache_file, &cancel).unwrap(), FetchResult::Fresh(_)));
        assert!(!load_cache(&cache_file).unwrap().is_negative("zz"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端到端（可选）：`DG_E2E_MIRROR` 指向 `tools/make_registry.py` 的产物
    /// 目录（真 Helix DLL）时，把「整表 → 查询 → 下载 → 解包校验」整条链
    /// 对真数据跑一遍；平时（CI）没有镜像就跳过。
    #[test]
    fn e2e_against_generated_mirror_when_present() {
        let Some(mirror_dir) = std::env::var_os("DG_E2E_MIRROR") else {
            eprintln!("skip: 未设 DG_E2E_MIRROR（tools/make_registry.py 的产物目录）");
            return;
        };
        let dir = PathBuf::from(mirror_dir);
        let source = RegistrySource::Local { dir: dir.clone() };
        let cache_file = dir.join(".e2e-cache.json");
        let root = tmp("e2e-root");
        let cancel = AtomicBool::new(false);
        let _ = std::fs::remove_file(&cache_file);
        let doc = match fetch_registry(&source, &cache_file, &cancel).unwrap() {
            FetchResult::Fresh(c) => *c,
            FetchResult::Unchanged => panic!("首次不该是 Unchanged"),
        };
        assert!(!doc.registry.packages.is_empty(), "镜像里应有包");
        for pkg in &doc.registry.packages {
            // 查询按扩展名能找到自己；下载+安装落到 root，DLL 与 queries 就位。
            let ext = pkg.extensions.first().map(String::as_str).unwrap_or_default();
            let hit = query_ext(&source, &cache_file, ext, &cancel).unwrap();
            assert!(hit.iter().any(|c| c.name == pkg.name), "{} 查不到", pkg.name);
            let installed = download_and_install(&source, pkg, &root, &mut |_, _| true, &cancel)
                .unwrap_or_else(|e| format!("{} 安装失败: {e}", pkg.name).into());
            assert!(installed.join("manifest.toml").exists());
            let dlls: Vec<_> = std::fs::read_dir(&installed)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "dll"))
                .collect();
            assert!(!dlls.is_empty(), "{} 没有 DLL", pkg.name);
            assert!(installed.join("queries").exists(), "{} 没有 queries", pkg.name);
        }
        let _ = std::fs::remove_file(&cache_file);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 端到端（可选）：`DG_E2E_BASE` 指向跑着的 worker（`wrangler dev` 或
    /// 线上地址）时，用真 HTTP 客户端走「整表 → ETag 304 → 查询 → 下载」。
    #[test]
    fn e2e_against_http_worker_when_present() {
        let Ok(base) = std::env::var("DG_E2E_BASE") else {
            eprintln!("skip: 未设 DG_E2E_BASE（wrangler dev 或线上 workers.dev）");
            return;
        };
        let source = RegistrySource::Remote { base: base.trim_end_matches('/').to_string() };
        let dir = tmp("e2e-http");
        let cache_file = dir.join("cache.json");
        let root = tmp("e2e-http-root");
        let cancel = AtomicBool::new(false);
        let doc = match fetch_registry(&source, &cache_file, &cancel).unwrap() {
            FetchResult::Fresh(c) => *c,
            FetchResult::Unchanged => panic!("首次不该是 Unchanged"),
        };
        assert!(!doc.registry.packages.is_empty());
        // ETag 生效：第二次整表是 304 → Unchanged。
        assert!(matches!(fetch_registry(&source, &cache_file, &cancel).unwrap(), FetchResult::Unchanged));
        let pkg = doc.registry.packages[0].clone();
        let hit = query_ext(&source, &cache_file, &pkg.extensions[0], &cancel).unwrap();
        assert!(hit.iter().any(|c| c.name == pkg.name));
        // 带符号后缀：查询参数经百分号编码原样到达服务端（`+` 不转会被
        // 当空格）。registry 里有人认领 `.c++` 时这条才算数。
        if doc.registry.packages.iter().any(|p| p.extensions.iter().any(|e| e == "c++")) {
            let signed = query_ext(&source, &cache_file, "c++", &cancel).unwrap();
            assert!(
                signed.iter().any(|p| p.extensions.iter().any(|e| e == "c++")),
                "c++ 查询应命中认领它的包"
            );
        }
        let installed = download_and_install(&source, &pkg, &root, &mut |_, _| true, &cancel).unwrap();
        assert!(installed.join("manifest.toml").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
    }
}
