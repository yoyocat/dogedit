//! ctags as an **opt-in** symbol source. It is off unless the user asks for it. The
//! reasons are practical: a `ctags -R` over a real tree costs seconds, the bundled 5.8
//! build parses no Rust, and the answer that is coming is the syntax tree (or LSP
//! workspace symbols). Running it behind the user's back made `@` slow on trees it could
//! not even read. Turning it on is one section in `setting.ini` next to the executable:
//! ```ini [Ctags] Enabled=1 Dir= ; empty = the project root (the goto-anything root)
//! Extensions=.c,.h,.cpp ; empty = DEFAULT_EXTENSIONS below ``` `Dir` and `Extensions`
//! exist because "index everything I can see" is never what anyone means: the scan is
//! confined to one directory, and only tags whose file carries one of the listed
//! extensions are kept. Both are read on every use — the file is a few kilobytes and the
//! dialog opens on a keystroke, so a cache would only make an edited `setting.ini` stale.

use std::path::Path;

use super::optionsdlg::{ini_path, read_bool, IniText};

/// What ctags 5.8 actually parses something useful out of, used when
/// `Extensions` is empty. Deliberately not "everything": the point of the
/// setting is to keep the scan off files the user never asked about.
const DEFAULT_EXTENSIONS: &[&str] = &[
    ".c", ".h", ".cpp", ".hpp", ".cc", ".cxx", ".java", ".cs", ".php", ".py", ".rb", ".pl", ".lua",
];

/// How many tags one scan keeps. A whole-drive tree can produce millions; the
/// query only ever shows a hundred.
const TAG_CAP: usize = 50_000;

const SECTION: &str = "Ctags";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Tag {
    pub(super) name: String,
    pub(super) path: String,
    /// 0-based: ctags reports 1-based `line:` and the jump takes 0-based.
    pub(super) line0: usize,
}

/// The `[Ctags]` section, resolved: what to scan, and which files count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Config {
    /// `Enabled=1`. Everything in this module is a no-op when it is false.
    pub(super) enabled: bool,
    /// The directory the scan is confined to. The project root unless `Dir`
    /// says otherwise; meaningless when `enabled` is false.
    pub(super) dir: String,
    /// Lower-case, each with its leading dot.
    pub(super) extensions: Vec<String>,
}

/// Reads `[Ctags]`. `root` is the project root the dialog is about to use, which
/// is what `Dir` falls back to.
pub(super) fn config(root: &str) -> Config {
    let ini = IniText::load(&ini_path());
    let enabled = read_bool(&ini, SECTION, "Enabled", false);
    let dir = ini.get(SECTION, "Dir").unwrap_or_default();
    let dir = dir.trim();
    let dir = if dir.is_empty() {
        root.to_string()
    } else {
        dir.to_string()
    };
    let mut extensions = parse_extensions(ini.get(SECTION, "Extensions").as_deref().unwrap_or(""));
    if extensions.is_empty() {
        extensions = DEFAULT_EXTENSIONS.iter().map(|e| e.to_string()).collect();
    }
    Config {
        enabled,
        dir,
        extensions,
    }
}

/// `Extensions=.c,.h;.CPP` — split on either separator, tolerate the missing
/// dot and any case, keep the order and drop duplicates. Pure, so the spelling
/// the user is allowed to get away with is testable.
fn parse_extensions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split([',', ';', ' ', '\t']) {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        let lower = item.to_ascii_lowercase();
        let with_dot = if lower.starts_with('.') {
            lower
        } else {
            format!(".{lower}")
        };
        if !out.contains(&with_dot) {
            out.push(with_dot);
        }
    }
    out
}

/// Whether `path` is a file the user asked ctags to look at.
pub(super) fn allows(path: &str, cfg: &Config) -> bool {
    let lower = path.to_ascii_lowercase();
    cfg.extensions.iter().any(|e| lower.ends_with(e.as_str()))
}

/// A user-provided `bin\ctags.exe` next to the executable, or a `ctags`
/// installed on `PATH`.
pub(super) fn exe() -> Option<String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let mut p = dir.to_path_buf();
            p.push("bin");
            p.push("ctags.exe");
            if p.is_file() {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["ctags.exe", "ctags"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// `ctags -R` over [`Config::dir`], keeping only the extensions the user listed.
/// The scan runs with the directory as the working directory and `.` as the
/// argument. The walker's skip list goes in as `--exclude`, so `target\` and
/// `node_modules\` are not walked twice over. ctags 5.8 has no "only these
/// extensions" option (`--languages` is language-based, and its extension map is
/// fixed), so the filter is applied to what comes back. The scan still costs
/// what it costs; the filter decides what the symbol query can see.
pub(super) fn scan(cfg: &Config) -> Vec<Tag> {
    if !cfg.enabled {
        return Vec::new();
    }
    let Some(exe) = exe() else {
        return Vec::new();
    };
    let mut cmd = std::process::Command::new(&exe);
    dg_core::process::no_console_window(&mut cmd);
    cmd.current_dir(&cfg.dir);
    for dir in dg_core::fileindex::skip_dirs() {
        cmd.arg(format!("--exclude={dir}"));
    }
    let Ok(out) = cmd.args(["-R", "--fields=+n", "-f", "-", "."]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut tags = parse(&text, TAG_CAP);
    tags.retain(|t| allows(&t.path, cfg));
    let base = Path::new(&cfg.dir);
    for t in &mut tags {
        t.path = norm_path(&base.join(&t.path).to_string_lossy());
    }
    tags
}

/// One file's tags, for the symbol panel's fallback. Empty unless the user
/// enabled ctags and the file's extension is one they listed.
pub(super) fn document_symbols(path: &str, cfg: &Config) -> Vec<(String, usize)> {
    if !cfg.enabled || !allows(path, cfg) {
        return Vec::new();
    }
    let Some(exe) = exe() else {
        return Vec::new();
    };
    // ctags 5.8 default output: name<TAB>file<TAB>/pattern/;"<TAB>kind<...line:NN
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["--fields=+n", "-f", "-", path]);
    dg_core::process::no_console_window(&mut cmd);
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    parse(&text, TAG_CAP)
        .into_iter()
        .map(|t| (t.name, t.line0))
        .collect()
}

/// Parses `ctags -f -` output. Lines are `name<TAB>file<TAB>/pattern/;"<TAB>
/// kind<TAB>line:N`; pseudo-tags (`!_TAG_...`) and rows without a `line:` field
/// are skipped. Pure, so the format handling is testable.
fn parse(text: &str, cap: usize) -> Vec<Tag> {
    let mut out = Vec::new();
    for line in text.lines() {
        if out.len() >= cap {
            break;
        }
        if line.starts_with('!') {
            continue; // !_TAG_FILE_FORMAT and friends
        }
        let mut fields = line.split('\t');
        let (Some(name), Some(path)) = (fields.next(), fields.next()) else {
            continue;
        };
        if name.is_empty() || path.is_empty() {
            continue;
        }
        // Skip the search pattern, then look for `line:N` among the extras.
        let mut line_no = None;
        for extra in fields.skip(1) {
            if let Some(n) = extra.strip_prefix("line:") {
                line_no = n.trim().parse::<usize>().ok();
            }
        }
        let Some(n) = line_no else { continue };
        out.push(Tag {
            name: name.to_string(),
            path: path.to_string(),
            line0: n.saturating_sub(1),
        });
    }
    out
}

/// `E:\root\./src/main.rs` and `E:\root\src\main.rs` have to compare equal, or
/// the active document's tags never match its symbols.
pub(super) fn norm_path(p: &str) -> String {
    p.replace('/', "\\").replace("\\.\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, exts: &str) -> Config {
        Config {
            enabled,
            dir: r"E:\proj".to_string(),
            extensions: parse_extensions(exts),
        }
    }

    /// The spelling the user is allowed to get away with: either separator, the
    /// dot optional, any case, duplicates dropped.
    #[test]
    fn parse_extensions_is_forgiving() {
        assert_eq!(
            parse_extensions(".c, h;.CPP .c"),
            vec![".c".to_string(), ".h".to_string(), ".cpp".to_string()]
        );
        assert!(parse_extensions("   ").is_empty());
        assert!(parse_extensions("").is_empty());
    }

    /// Off means off: no scan, no per-document run, whatever the extensions say.
    #[test]
    fn a_disabled_config_does_nothing() {
        let c = cfg(false, ".c");
        assert!(!c.enabled);
        assert!(scan(&c).is_empty());
        assert!(document_symbols(r"E:\proj\src\main.c", &c).is_empty());
    }

    /// The extension list is the whole point: a file the user did not list is
    /// not a file ctags looks at.
    #[test]
    fn allows_matches_the_listed_extensions_only() {
        let c = cfg(true, ".c,.h");
        assert!(allows(r"E:\proj\src\main.c", &c));
        assert!(allows(r"E:\proj\src\MAIN.C", &c), "case-insensitive");
        assert!(allows(r"E:\proj\src\a.hpp", &cfg(true, ".hpp")));
        assert!(!allows(r"E:\proj\src\main.rs", &c));
        assert!(!allows(r"E:\proj\src\main.cpp", &c));
        assert!(!allows(r"E:\proj\src\main", &c), "no extension at all");
        // `.h` must not match a name that merely ends in `h`.
        assert!(!allows(r"E:\proj\src\leah", &c));
    }

    /// `ctags -f -` output: pseudo-tags and rows without `line:` are dropped,
    /// 1-based `line:` becomes 0-based, and the cap is honoured.
    #[test]
    fn parse_reads_ctags_output() {
        let text = "!_TAG_FILE_FORMAT\t2\t/extended format/\n\
                    main\t./src/main.rs\t/^fn main() {$/;\"\tfunction\tline:12\n\
                    no_line\t./src/x.rs\t/^x$/;\"\tvariable\n\
                    \n\
                    short\n\
                    Lib\t./src/lib.rs\t/^pub fn Lib()$/;\"\tfunction\tline:1\n";
        let tags = parse(text, 100);
        assert_eq!(tags.len(), 2, "the pseudo-tag and the line-less row are gone");
        assert_eq!(tags[0].name, "main");
        assert_eq!(tags[0].path, "./src/main.rs");
        assert_eq!(tags[0].line0, 11);
        assert_eq!(tags[1].name, "Lib");
        assert_eq!(tags[1].line0, 0, "line:1 is the first line");
        assert_eq!(parse(text, 1).len(), 1, "cap is respected");
    }

    /// `E:\r\./src\a.rs` and `E:\r\src\a.rs` must compare equal, or a tag is
    /// shown twice — once from the document's symbols, once from the index.
    #[test]
    fn norm_path_folds_separators() {
        assert_eq!(norm_path(r"E:\r\./src/main.rs"), r"E:\r\src\main.rs");
        assert_eq!(norm_path("E:/r/src/main.rs"), r"E:\r\src\main.rs");
    }

    /// The shipped default list is C-family plus the scripting languages 5.8
    /// knows; it is what an empty `Extensions=` means.
    #[test]
    fn the_default_list_covers_the_c_family() {
        let c = Config {
            enabled: true,
            dir: String::new(),
            extensions: DEFAULT_EXTENSIONS.iter().map(|e| e.to_string()).collect(),
        };
        assert!(allows(r"E:\p\a.c", &c));
        assert!(allows(r"E:\p\a.cpp", &c));
        assert!(allows(r"E:\p\a.py", &c));
        assert!(!allows(r"E:\p\a.rs", &c), "5.8 cannot parse Rust");
        assert!(!allows(r"E:\p\a.ts", &c));
    }
}
