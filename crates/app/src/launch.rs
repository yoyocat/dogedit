//! 一次启动要做的事：命令行解析出来的文件、在文件中查找的文件夹，以及
//! 「这次一定是新进程」这类只影响启动本身的开关。
//!
//! 单实例模式下第二次启动并不建窗：它把这份请求编成一段字节，用
//! `WM_COPYDATA` 交给已经开着的那个窗口，然后退出。编码就放在这里，
//! 收发两端走的是同一份代码，不会出现「发出去的对方读不懂」。

/// Windows 文件名不允许控制字符，换行因此是安全的分隔符；正文用 UTF-8，
/// 中文路径不会因为进程间传字节而丢字。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// 命令行上要打开的文件，按出现的顺序。
    pub files: Vec<String>,
    /// `-g` 带来的文件夹：开窗后直接把「在文件中查找和替换」的文件夹填成它。
    pub grep_dir: Option<String>,
    /// 「文件 → 新建实例」、页签的分离到新实例、改主题后的重启都带这个开关：
    /// 它们要的就是一个新进程，不能被转交给旧窗口。
    pub new_instance: bool,
    /// 安全模式也要单独开窗，否则「临时进来一趟」会变成往旧窗口里塞文件。
    pub safe_mode: bool,
    pub selftest: bool,
    pub packages: bool,
    pub doctor: bool,
    pub strict: bool,
    pub lint_file: Option<String>,
}

/// 进程间请求的开头。换了格式就换这个版本号，旧窗口遇到对不上的会直接忽略。
const HANDOFF_MAGIC: &str = "dogedit-handoff-1";

impl Launch {
    pub fn parse(args: &[String]) -> Self {
        let mut out = Self::default();
        let mut rest = args.iter().skip(1);
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--selftest" => out.selftest = true,
                "--packages" => out.packages = true,
                "--ui-doctor" => out.doctor = true,
                "--strict" => out.strict = true,
                "--ui-lint" => out.lint_file = rest.next().cloned(),
                "--safe-mode" => out.safe_mode = true,
                "--new-instance" => out.new_instance = true,
                // 目录右键那条菜单项写的就是这个参数；`--grep` 是给命令行用的长名。
                "-g" | "--grep" => out.grep_dir = rest.next().cloned().filter(|s| !s.is_empty()),
                other if other.starts_with("--") => {}
                other => out.files.push(other.to_string()),
            }
        }
        out
    }

    /// 编成 `WM_COPYDATA` 的正文：每行一个条目，`f` 是文件、`g` 是文件夹。
    pub fn handoff_bytes(&self) -> Vec<u8> {
        let mut out = String::from(HANDOFF_MAGIC);
        out.push('\n');
        for file in &self.files {
            out.push('f');
            out.push_str(file);
            out.push('\n');
        }
        if let Some(dir) = &self.grep_dir {
            out.push('g');
            out.push_str(dir);
            out.push('\n');
        }
        out.into_bytes()
    }

    /// 解一份请求。开头对不上就当不是发给自己的消息。
    pub fn from_handoff(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.split('\n');
        if lines.next()? != HANDOFF_MAGIC {
            return None;
        }
        let mut out = Self::default();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            match line.as_bytes()[0] {
                b'f' => out.files.push(line[1..].to_string()),
                b'g' => out.grep_dir = Some(line[1..].to_string()),
                _ => return None,
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        std::iter::once("dogedit.exe")
            .chain(items.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn parses_files_flags_and_the_value_taking_switch() {
        let l = Launch::parse(&args(&[
            "--ui-lint",
            "ui.lua",
            "a.txt",
            "--strict",
            "b.txt",
            "--unknown-flag",
        ]));
        assert_eq!(l.files, ["a.txt", "b.txt"]);
        assert_eq!(l.lint_file.as_deref(), Some("ui.lua"));
        assert!(l.strict);
        assert!(!l.new_instance);
    }

    #[test]
    fn parses_the_folder_switch_both_ways() {
        assert_eq!(
            Launch::parse(&args(&["-g", "C:\\work\\src"])).grep_dir.as_deref(),
            Some("C:\\work\\src")
        );
        assert_eq!(
            Launch::parse(&args(&["--grep", "D:\\中文 目录"])).grep_dir.as_deref(),
            Some("D:\\中文 目录")
        );
        assert_eq!(Launch::parse(&args(&["-g"])).grep_dir, None);
    }

    #[test]
    fn internal_switches_are_kept_apart_from_files() {
        let l = Launch::parse(&args(&["--safe-mode", "--new-instance", "a.txt"]));
        assert!(l.safe_mode);
        assert!(l.new_instance);
        assert_eq!(l.files, ["a.txt"]);
    }

    #[test]
    fn handoff_round_trips_paths_and_the_folder() {
        let l = Launch {
            files: vec!["C:\\a b\\读我.txt".to_string(), "D:\\x.rs".to_string()],
            grep_dir: Some("E:\\中文 目录".to_string()),
            ..Default::default()
        };
        assert_eq!(Launch::from_handoff(&l.handoff_bytes()), Some(l));
    }

    #[test]
    fn an_empty_handoff_is_still_a_request_to_come_forward() {
        assert_eq!(
            Launch::from_handoff(&Launch::default().handoff_bytes()),
            Some(Launch::default())
        );
    }

    #[test]
    fn a_foreign_message_is_not_a_request() {
        assert_eq!(Launch::from_handoff(b"something-else\nf.txt"), None);
        assert_eq!(Launch::from_handoff(b"dogedit-handoff-1\nq.txt"), None);
    }
}
