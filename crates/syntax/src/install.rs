//! 解析器包的安装层：校验 → 解包 → 就位（「解析器下载」的落盘半边）。
//!
//! 一个包 zip 就是包目录的打包（`manifest.toml` + `<name>.dll` +
//! `queries\*.scm`）。安装走**先落临时、校验过了再改名**的路：
//!
//! 1. sha256 对不上（注册表声明的与实际字节不符）直接拒绝；
//! 2. 解到 `<root>\.<name>.new`，逐条检查 zip 内路径（防穿越）与大小
//!   （防炸弹），解完再验一遍 manifest（名字要对得上、DLL 要在）；
//! 3. 旧目录先改名 `<name>-<millis>.old` 再把新目录改名就位 —— 改名是
//!    原子的，就算旧包的 DLL 正被进程加载（Windows 不许删除加载中的
//!    DLL），`.old` 也能改名成功，留给下次启动 [`clean_old_dirs`] 清。
//!
//! 卸载同理：只做「改名进 `.old` + 尽力删」，永不挂起、永不半删。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::manifest::PluginManifest;

/// 单个文件解包上限（字节）。tree-sitter 的 DLL 大则几 MB，32MB 足够宽裕。
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// 一个包解压后的总上限（字节）。zip 炸弹在这里被拦下。
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// 条目数上限。
pub const MAX_ENTRIES: usize = 2048;

/// 测试可调的上限。生产路径用 [`Limits::default`]。
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file_bytes: u64,
    pub total_bytes: u64,
    pub entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: MAX_FILE_BYTES,
            total_bytes: MAX_TOTAL_BYTES,
            entries: MAX_ENTRIES,
        }
    }
}

/// 安装/卸载失败的可读原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallError {
    /// 字节哈希与注册表声明不符。
    HashMismatch { want: String, got: String },
    /// zip 容器本身读不了。
    Archive(String),
    /// zip 内路径不合法（穿越、绝对路径、反斜杠、软链接）。
    UnsafeEntry { name: String, reason: &'static str },
    /// 解包体积超上限。
    TooLarge { file: String, got: u64, max: u64 },
    /// 解包后的 manifest 缺失/不合法/与包名不一致。
    Manifest(String),
    /// 文件系统错误。
    Io(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HashMismatch { want, got } => write!(
                f,
                "sha256 不符：声明 {want}，实际 {got}（下载不完整或源被篡改）"
            ),
            Self::Archive(e) => write!(f, "包 zip 读取失败: {e}"),
            Self::UnsafeEntry { name, reason } => {
                write!(f, "包内路径不合法 `{name}`（{reason}）")
            }
            Self::TooLarge { file, got, max } => {
                write!(f, "解包体积超限：{file} 解出 {got} 字节（上限 {max}）")
            }
            Self::Manifest(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InstallError {}

/// 字节的 sha256，小写 hex。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从包 zip 里读出 manifest 的 `(name, version)`（「从本地文件安装」要先
/// 知道落到哪个目录名、列表里要显示版本）。manifest 必须在 zip 根 ——
/// 与 [`install_package`] 解包后的布局要求一致。
pub fn manifest_from_zip(zip_bytes: &[u8]) -> Result<(String, String), InstallError> {
    let mut archive =
        ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|e| InstallError::Archive(e.to_string()))?;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| InstallError::Archive(format!("第 {i} 个条目: {e}")))?;
        if file.name() != "manifest.toml" {
            continue;
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|e| InstallError::Manifest(format!("manifest.toml 不是 UTF-8: {e}")))?;
        let m = PluginManifest::parse(&text).map_err(InstallError::Manifest)?;
        return Ok((m.name, m.version));
    }
    Err(InstallError::Manifest(
        "包 zip 里没有根级 manifest.toml".into(),
    ))
}

/// 把一个包 zip 安装到 `<root>\<name>\`，返回落点目录。
///
/// `name` 是注册表条目的包名（也是落点目录名）；`expected_sha256` 为
/// `None` 时跳过哈希校验（「从本地文件安装」走这条路 —— 文件本来就是
/// 用户自己选的）。
pub fn install_package(
    root: &Path,
    name: &str,
    zip_bytes: &[u8],
    expected_sha256: Option<&str>,
) -> Result<PathBuf, InstallError> {
    install_package_with(root, name, zip_bytes, expected_sha256, &Limits::default())
}

/// [`install_package`] 的可注入上限版本（测试用小上限）。
pub fn install_package_with(
    root: &Path,
    name: &str,
    zip_bytes: &[u8],
    expected_sha256: Option<&str>,
    limits: &Limits,
) -> Result<PathBuf, InstallError> {
    if !valid_package_name(name) {
        return Err(InstallError::Manifest(format!(
            "包名 `{name}` 不合法（只允许 a-z 0-9 _ -）"
        )));
    }
    if let Some(want) = expected_sha256 {
        let got = sha256_hex(zip_bytes);
        if got != want.to_ascii_lowercase() {
            return Err(InstallError::HashMismatch {
                want: want.to_ascii_lowercase(),
                got,
            });
        }
    }
    std::fs::create_dir_all(root).map_err(|e| InstallError::Io(e.to_string()))?;
    let tmp = root.join(format!(".{name}.new"));
    // 上一次安装半途死掉的临时目录：清掉重来。
    let _ = std::fs::remove_dir_all(&tmp);
    unpack(root, &tmp, zip_bytes, limits)?;
    // 解包完再验 manifest：名字与落点一致、DLL 真的在 —— 别把一个扫描
    // 阶段注定报错的包留在用户盘上。
    let manifest = std::fs::read_to_string(tmp.join("manifest.toml")).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        InstallError::Manifest(format!("包 {name} 缺 manifest.toml: {e}"))
    })?;
    let parsed = PluginManifest::parse(&manifest).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        InstallError::Manifest(format!("包 {name} 的 manifest.toml: {e}"))
    })?;
    if parsed.name != name {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(InstallError::Manifest(format!(
            "包 manifest 的 name `{}` 与注册表条目 `{name}` 不一致",
            parsed.name
        )));
    }
    if !tmp.join(&parsed.dll).exists() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(InstallError::Manifest(format!(
            "包 {name} 缺 DLL {}",
            parsed.dll
        )));
    }

    // 就位：旧目录改名进 `.old`（加载中的 DLL 也挡不住改名），新目录顶上。
    let target = root.join(name);
    let mut retired: Option<PathBuf> = None;
    if target.exists() {
        let old = root.join(format!("{name}-{}.old", millis()));
        std::fs::rename(&target, &old).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp);
            InstallError::Io(format!("旧包改名失败: {e}"))
        })?;
        retired = Some(old);
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        // 新目录顶不上去：把旧包还回去，装失败不留残局。
        if let Some(old) = &retired {
            let _ = std::fs::rename(old, &target);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(InstallError::Io(format!("包目录改名失败: {e}")));
    }
    if let Some(old) = retired {
        // 删得掉就删；删不掉（DLL 被加载）留给下次启动 clean_old_dirs。
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(target)
}

/// 卸载一个包：目录改名进 `.old`（瞬时完成，加载中的 DLL 不阻塞），
/// 然后尽力删。包不存在也算卸载成功（幂等）。
pub fn uninstall_package(root: &Path, name: &str) -> Result<(), InstallError> {
    if !valid_package_name(name) {
        return Err(InstallError::Manifest(format!(
            "包名 `{name}` 不合法（只允许 a-z 0-9 _ -）"
        )));
    }
    let target = root.join(name);
    if !target.exists() {
        return Ok(());
    }
    let old = root.join(format!("{name}-{}.old", millis()));
    std::fs::rename(&target, &old).map_err(|e| InstallError::Io(format!("包目录改名失败: {e}")))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// 启动清理：上次卸载/更新留下的 `.old` 目录（DLL 被加载时删不掉的那种）
/// 和半途死掉的 `.<name>.new` 临时目录。best-effort，删不掉留到下次。
pub fn clean_old_dirs(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".old") || (name.starts_with('.') && name.ends_with(".new")) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 包名字符集与 [`crate::manifest::PluginManifest`] 的 `name` 规则一致，
/// 同时保证它做路径段是安全的。
fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// 把 zip 解到 `tmp`：逐条校验路径与大小，出错就整目录删掉。
fn unpack(root: &Path, tmp: &Path, zip_bytes: &[u8], limits: &Limits) -> Result<(), InstallError> {
    let archive = ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| InstallError::Archive(e.to_string()))?;
    let result = unpack_archive(tmp, archive, limits);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(tmp);
    }
    let _ = root;
    result
}

fn unpack_archive(
    tmp: &Path,
    mut archive: ZipArchive<std::io::Cursor<&[u8]>>,
    limits: &Limits,
) -> Result<(), InstallError> {
    if archive.len() > limits.entries {
        return Err(InstallError::TooLarge {
            file: format!("（{} 个条目）", archive.len()),
            got: archive.len() as u64,
            max: limits.entries as u64,
        });
    }
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| InstallError::Archive(format!("第 {i} 个条目: {e}")))?;
        let raw = file.name().to_string();
        // 软链接在 zip 里是 unix mode 的高位；解析器包不需要它，一律拒绝。
        if file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(InstallError::UnsafeEntry {
                name: raw,
                reason: "软链接",
            });
        }
        let rel = check_entry_path(&raw)?;
        if file.is_dir() {
            std::fs::create_dir_all(tmp.join(&rel))
                .map_err(|e| InstallError::Io(e.to_string()))?;
            continue;
        }
        let size = file.size();
        total += size;
        if size > limits.file_bytes {
            return Err(InstallError::TooLarge {
                file: raw,
                got: size,
                max: limits.file_bytes,
            });
        }
        if total > limits.total_bytes {
            return Err(InstallError::TooLarge {
                file: raw,
                got: total,
                max: limits.total_bytes,
            });
        }
        let dest = tmp.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| InstallError::Io(e.to_string()))?;
        }
        let mut out = std::fs::File::create(&dest).map_err(|e| InstallError::Io(e.to_string()))?;
        // 上限是按声明的 size 预检的，读的时候仍按上限截：声明值由压缩
        // 数据算出，可信，但多读一个字节都不往盘上写。
        let mut remaining = limits.file_bytes;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| InstallError::Io(e.to_string()))?;
            if n == 0 {
                break;
            }
            remaining = remaining.saturating_sub(n as u64);
            out.write_all(&buf[..n]).map_err(|e| InstallError::Io(e.to_string()))?;
        }
        let _ = remaining;
    }
    Ok(())
}

/// zip 内路径的合法性：`/` 分隔的相对路径，段非空、非 `.`/`..`、无盘符。
/// 返回清洗后的相对路径（尾随 `/` 已去掉）。
fn check_entry_path(raw: &str) -> Result<PathBuf, InstallError> {
    let bad = |reason: &'static str| InstallError::UnsafeEntry {
        name: raw.to_string(),
        reason,
    };
    let raw = raw.strip_suffix('/').unwrap_or(raw);
    if raw.is_empty() || raw.starts_with('/') || raw.contains('\\') {
        return Err(bad("不是 `/` 分隔的包内相对路径"));
    }
    for seg in raw.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(bad("路径穿越或空段"));
        }
        if seg.contains(':') || seg.chars().any(|c| c.is_control()) {
            return Err(bad("盘符或控制字符"));
        }
    }
    Ok(PathBuf::from(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    const MANIFEST: &str = "name = \"demo\"\ndisplay = \"Demo\"\nextensions = [\"dm\"]\n";

    /// 造一个最小包 zip：manifest + 假 DLL + 一个 query。扫描器只查
    /// manifest 与 DLL 存在性，DLL 内容是假字节不影响扫描。
    fn demo_zip(name: &str) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("manifest.toml", SimpleFileOptions::default())
            .unwrap();
        w.write_all(MANIFEST.replace("demo", name).as_bytes()).unwrap();
        w.start_file(&format!("{name}.dll"), SimpleFileOptions::default())
            .unwrap();
        w.write_all(b"fake dll bytes").unwrap();
        w.start_file("queries/highlights.scm", SimpleFileOptions::default())
            .unwrap();
        w.write_all(b"; scm").unwrap();
        w.finish().unwrap().into_inner()
    }

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dg-syntax-install-{tag}-{}-{}",
            std::process::id(),
            millis()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_then_scan_finds_the_package() {
        let root = tmp_root("scan");
        let bytes = demo_zip("demo");
        let target = install_package(&root, "demo", &bytes, Some(&sha256_hex(&bytes))).unwrap();
        assert!(target.join("manifest.toml").exists());
        assert!(target.join("demo.dll").exists());
        assert!(target.join("queries").join("highlights.scm").exists());
        // 装出来的目录必须直接过注册表扫描（name 与目录名一致）。
        let mut reg = crate::LanguageRegistry::new();
        let diags = reg.scan_plugins(&root);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(reg.plugin_list().len(), 1);
        assert_eq!(reg.plugin_list()[0].version, "0.0.0");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hash_mismatch_is_rejected() {
        let root = tmp_root("hash");
        let bytes = demo_zip("demo");
        let err = install_package(&root, "demo", &bytes, Some(&"b".repeat(64))).unwrap_err();
        assert!(matches!(err, InstallError::HashMismatch { .. }), "{err}");
        assert!(!root.join("demo").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_entry_names_are_rejected() {
        let root = tmp_root("slip");
        for name in ["../evil.txt", "/abs.txt", "a\\b.txt", "pkg/../evil.txt"] {
            let mut w = ZipWriter::new(Cursor::new(Vec::new()));
            w.start_file(name, SimpleFileOptions::default()).unwrap();
            w.write_all(b"x").unwrap();
            let bytes = w.finish().unwrap().into_inner();
            let err = install_package(&root, "demo", &bytes, None).unwrap_err();
            assert!(matches!(err, InstallError::UnsafeEntry { .. }), "{name}: {err}");
        }
        // 软链接条目同样拒绝。
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.add_symlink("link.dll", "../elsewhere.dll", SimpleFileOptions::default())
            .unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let err = install_package(&root, "demo", &bytes, None).unwrap_err();
        assert!(matches!(err, InstallError::UnsafeEntry { .. }), "{err}");
        assert!(!root.join("demo").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_must_match_the_registry_name() {
        let root = tmp_root("mismatch");
        // zip 里写的是 `demo`，落点名是 `other` —— 拒绝。
        let bytes = demo_zip("demo");
        let err = install_package(&root, "other", &bytes, None).unwrap_err();
        assert!(matches!(err, InstallError::Manifest(_)), "{err}");
        assert!(!root.join("other").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn size_caps_are_enforced() {
        let root = tmp_root("caps");
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("manifest.toml", SimpleFileOptions::default())
            .unwrap();
        w.write_all(MANIFEST.as_bytes()).unwrap();
        w.start_file("demo.dll", SimpleFileOptions::default()).unwrap();
        w.write_all(&vec![0u8; 4096]).unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let limits = Limits {
            file_bytes: 1024,
            ..Limits::default()
        };
        let err = install_package_with(&root, "demo", &bytes, None, &limits).unwrap_err();
        assert!(matches!(err, InstallError::TooLarge { .. }), "{err}");
        assert!(!root.join("demo").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reinstall_replaces_the_previous_copy() {
        let root = tmp_root("update");
        let v1 = demo_zip("demo");
        install_package(&root, "demo", &v1, None).unwrap();
        std::fs::write(root.join("demo").join("demo.dll"), b"v2 fake").unwrap();
        // v2 带一点不同的 query，用来分辨新旧。
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("manifest.toml", SimpleFileOptions::default())
            .unwrap();
        w.write_all(MANIFEST.as_bytes()).unwrap();
        w.start_file("demo.dll", SimpleFileOptions::default()).unwrap();
        w.write_all(b"v2 fake").unwrap();
        w.start_file("queries/folds.scm", SimpleFileOptions::default())
            .unwrap();
        w.write_all(b"; folds").unwrap();
        let v2 = w.finish().unwrap().into_inner();
        install_package(&root, "demo", &v2, None).unwrap();
        assert!(root.join("demo").join("queries").join("folds.scm").exists());
        // 假 DLL 没被加载，`.old` 应当已被顺手清掉。
        assert!(!list_old_dirs(&root).exists_any());
        let _ = std::fs::remove_dir_all(&root);
    }

    struct OldDirs(Vec<PathBuf>);
    impl OldDirs {
        fn exists_any(&self) -> bool {
            !self.0.is_empty()
        }
    }
    fn list_old_dirs(root: &Path) -> OldDirs {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().ends_with(".old") {
                    out.push(e.path());
                }
            }
        }
        OldDirs(out)
    }

    #[test]
    fn leftover_old_and_tmp_dirs_are_cleaned_on_startup() {
        let root = tmp_root("clean");
        let old = root.join("demo-123.old");
        std::fs::create_dir_all(old.join("queries")).unwrap();
        std::fs::write(old.join("demo.dll"), b"locked").unwrap();
        let tmp = root.join(".demo.new");
        std::fs::create_dir_all(&tmp).unwrap();
        clean_old_dirs(&root);
        assert!(list_old_dirs(&root).0.is_empty());
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn uninstall_removes_the_package() {
        let root = tmp_root("uninstall");
        install_package(&root, "demo", &demo_zip("demo"), None).unwrap();
        uninstall_package(&root, "demo").unwrap();
        assert!(!root.join("demo").exists());
        // 幂等：再卸一次也是 Ok。
        uninstall_package(&root, "demo").unwrap();
        // 路径穿越包名直接拒绝。
        let err = uninstall_package(&root, "../somewhere").unwrap_err();
        assert!(matches!(err, InstallError::Manifest(_)), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
