//! `setting.ini` 的读写 —— 不开窗也能用的那一半。
//!
//! 为什么单列一个模块：诊断（`--ui-doctor`）要知道用户停用了哪些包，它没有窗口，
//! 也不该去碰 `EditorState`。把「文件在哪、某个段里某个键是什么」放在这里，
//! 窗口那边（`setting_key` / `persist_setting_key`）和诊断走同一条路 ——
//! 否则「诊断说会装、程序不装」这种分歧迟早出现。
//!
//! 这里只做**机械的**读写：不给键名加默认值、不解释值的含义。行怎么切、
//! 段名键名的大小写、认哪几种编码，都是 [`dg_core::ini`] 的那套唯一语义；
//! `setting.ini` 用户可以手改，写回时只动目标行。

use dg_core::ini::Ini;
use std::path::{Path, PathBuf};

/// 读和写共用的那一份 `setting.ini`：先 exe 旁，再工作目录。
///
/// 两处都找不到就是 `None`：没有配置文件时**不新建**，写入直接
/// 放弃。这样「用户没配过」和「用户配了个空」在磁盘上是两件事。
pub fn path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("setting.ini"));
    }
    candidates.push(PathBuf::from("setting.ini"));
    candidates.into_iter().find(|p| p.exists())
}

/// 一份 `.ini` 的正文，已经解好码（BOM 剥掉）。
///
/// 正文按**严格** UTF-8 解：手写文件里混进一个非 UTF-8 字节时宁可当
/// 「没配过」，也不要把它替换成 U+FFFD 再写回去 —— 那等于替用户改了
/// 他没碰的行。
pub fn text(path: &Path) -> Option<String> {
    dg_core::ini::decode(&std::fs::read(path).ok()?)
}

/// `[section]` 下的一个值。
///
/// 空值当「没有」：手写 ini 里 `Disabled=` 和干脆不写这一行是同一个意思。
pub fn section(section: &str, key: &str) -> Option<String> {
    let path = path()?;
    let text = text(&path)?;
    section_in(&text, section, key)
}

/// [`section`] 的纯文本版（测试和「已经读到内存里」的调用方用这个）。
pub fn section_in(text: &str, section: &str, key: &str) -> Option<String> {
    Ini::parse(text).get(section, key).map(str::to_string)
}

/// 写回 `[section] key=value`，保留文件的其它行。
///
/// 段不存在就补在文件末尾；没有配置文件时什么都不写。写回是 UTF-8：
/// 旧的 UTF-16 文件在第一次改写时换成 UTF-8，读取两种都认。
pub fn write_section(section: &str, key: &str, value: &str) {
    let Some(path) = path() else {
        return;
    };
    write_to(&path, section, key, value);
}

/// [`write_section`] 的指定路径版。
pub fn write_to(path: &Path, section: &str, key: &str, value: &str) {
    let Some(mut doc) = Ini::load(path) else {
        return;
    };
    doc.set(section, key, value);
    let _ = doc.save(path);
}

// ---------------------------------------------------------------------------
// 包启停
// ---------------------------------------------------------------------------

/// 停用名单在 `setting.ini` 里的位置：`[Scripts] Disabled=a,b`。
///
/// 为什么是 ini 而不是另开一个状态文件：管理面不另存状态，用户手写
/// 一行也能改，和主题那套「一切都在文本里」保持一致。
pub const SECTION: &str = "Scripts";
pub const DISABLED_KEY: &str = "Disabled";

/// 用户停用了哪些包（按 id）。读不到、没写、写空，都是「一个也没停」。
pub fn disabled_packages() -> Vec<String> {
    split_list(&section(SECTION, DISABLED_KEY).unwrap_or_default())
}

/// 记下停用名单。空名单写空值 —— 不删行，删行会动到用户文件里其它段的空行。
pub fn set_disabled_packages(ids: &[String]) {
    write_section(SECTION, DISABLED_KEY, &ids.join(","));
}

/// 逗号分隔的 id 表 → 去重、去空白、保序。
///
/// 保序是为了「写进去什么，读回来还是什么」：`sort` 不看这个顺序，但探针会比
/// 文件字节，乱序会让 diff 变成噪声。
pub fn split_list(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in value.split(',') {
        let id = part.trim();
        if !id.is_empty() && !out.iter().any(|x| x == id) {
            out.push(id.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("dg-settings-tests");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    /// 写进去的读得回来，而且没碰别的段。
    #[test]
    fn a_written_key_reads_back() {
        let path = temp("roundtrip.ini");
        std::fs::write(&path, "[Edit]\r\nAutoPair=Y\r\n[Scripts]\r\nDisabled=old\r\n").unwrap();
        write_to(&path, SECTION, DISABLED_KEY, "a,b");
        let text = self::text(&path).unwrap();
        assert_eq!(section_in(&text, "Scripts", "Disabled").as_deref(), Some("a,b"));
        assert_eq!(section_in(&text, "Edit", "AutoPair").as_deref(), Some("Y"));
        let _ = std::fs::remove_file(&path);
    }

    /// 段不存在就补在末尾；写回统一成 UTF-8（无 BOM），旧的宽文件换过来。
    #[test]
    fn a_write_normalizes_the_encoding_to_utf8() {
        let path = temp("append.ini");
        let mut bytes = vec![0xFF, 0xFE];
        for u in "[Edit]\r\nAutoPair=Y".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        write_to(&path, SECTION, DISABLED_KEY, "x");
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(&raw[..1], b"[", "写回后应当是 UTF-8 无 BOM");
        let text = self::text(&path).unwrap();
        assert_eq!(section_in(&text, "Scripts", "Disabled").as_deref(), Some("x"));
        assert_eq!(section_in(&text, "Edit", "AutoPair").as_deref(), Some("Y"));
        let _ = std::fs::remove_file(&path);
    }

    /// 空值、空段、没这个段 —— 三种都是「一个也没停」。
    #[test]
    fn an_empty_list_means_nothing_is_off() {
        assert!(split_list("").is_empty());
        assert!(split_list("  , ,").is_empty());
        assert!(section_in("[Scripts]\r\nDisabled=\r\n", SECTION, DISABLED_KEY).is_none());
        assert!(section_in("[Edit]\r\nAutoPair=Y\r\n", SECTION, DISABLED_KEY).is_none());
    }

    /// 同一个 id 写两遍只算一个，空白被吃掉。
    #[test]
    fn a_list_is_trimmed_and_deduped() {
        assert_eq!(split_list(" a , b ,a, "), vec!["a", "b"]);
    }

    /// 段名、键名都不分大小写（手写文件里 `[scripts]` 和 `disabled=` 也认）。
    #[test]
    fn names_are_case_insensitive() {
        let text = "[scripts]\r\nDisabled=a\r\n";
        assert_eq!(section_in(text, SECTION, DISABLED_KEY).as_deref(), Some("a"));
        assert_eq!(section_in(text, SECTION, "disabled").as_deref(), Some("a"));
    }

    /// 三种编码都要露出第一行 `[Section]`，否则它下面的每个键都读成「不存在」，
    /// 写回时还会补出一个重复的段。（这条测试原来在 `window.rs`，跟着解码一起搬来。）
    #[test]
    fn every_ini_shape_loses_its_bom() {
        let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
        utf8_bom.extend_from_slice(b"[DateTime]\r\nDate=${y}\r\n");
        let utf16: Vec<u16> = "[DateTime]\r\nDate=${y}\r\n".encode_utf16().collect();
        let mut utf16_le = vec![0xFF, 0xFE];
        for u in &utf16 {
            utf16_le.extend_from_slice(&u.to_le_bytes());
        }
        for (name, bytes) in [
            ("bom", utf8_bom),
            ("plain", b"[DateTime]\r\nDate=${y}\r\n".to_vec()),
            ("utf16", utf16_le),
        ] {
            let path = temp(&format!("shape_{name}.ini"));
            std::fs::write(&path, &bytes).unwrap();
            let text = self::text(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            assert!(text.starts_with("[DateTime]"), "{name}: {text:?}");
        }
    }

    /// 混进一个非 UTF-8 字节时宁可当「没配过」，也不要把 U+FFFD 写回去 ——
    /// 那等于替用户改了他没碰的行。
    #[test]
    fn a_non_utf8_file_reads_as_nothing_and_is_left_alone() {
        let path = temp("broken.ini");
        let bytes = b"[Scripts]\r\nDisabled=\xff\xfe\xff\r\n".to_vec();
        std::fs::write(&path, &bytes).unwrap();
        assert!(self::text(&path).is_none());
        write_to(&path, SECTION, DISABLED_KEY, "a");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "坏文件被改了");
        let _ = std::fs::remove_file(&path);
    }
}
