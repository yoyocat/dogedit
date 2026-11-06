//! Find / replace in files.
//!
//! Two backends behind one entry point
//! (`run_search_with`):
//!
//! * `RgBackend` — shells out to `rg`
//! (`--vimgrep --no-heading --color=never`)
//! when a ripgrep binary is on the PATH.
//! * `WalkBackend` — built-in fallback: a
//! depth-limited `std::fs` walk that skips
//! hidden and binary files and scans the
//! remaining text with the same regex
//! engine the in-file search uses.
//!
//! Everything is plain `std` + `regex`; no
//! third-party process or glob crate.
//!
//! A search can report itself while it runs: [`run_search_with`] takes a
//! [`Progress`] callback that sees every match as it is found and can stop the
//! search, which is what the 查找中 dialog uses for its live 行数 counter and
//! its 停止 button.

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Instant;

use regex::Regex;

/// The live report of a running search: called with the path just scanned and
/// the number of matches found so far — once per file for the walker, once per
/// matching line for ripgrep. Returning `false` stops the search, and the
/// matches found up to that point are still returned.
pub type Progress<'a> = &'a mut dyn FnMut(&str, usize) -> bool;

/// A [`Progress`] that never stops anything.
fn keep_going() -> impl FnMut(&str, usize) -> bool {
    |_, _| true
}

/// The ceiling on one search's hit count. The walker has always stopped at
/// 100 000 matches; ripgrep has no such limit of its own, and a pattern like a
/// single letter over a whole tree reaches millions — which the caller would
/// then try to turn into a document. Both backends are capped here, in the one
/// place that sees every report.
pub const MAX_MATCHES: usize = 100_000;

/// Glob set (include / exclude patterns).
/// Supports `*`, `?`, `**` and a leading `!`
/// for negation, rg-style.
#[derive(Clone, Debug, Default)]
pub struct GlobSet {
    patterns: Vec<String>,
}

impl GlobSet {
    pub fn new(patterns: &[String]) -> Self {
        Self {
            patterns: patterns.to_vec(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
    /// Whether the name (file name or relative
    /// path) matches any pattern.
    pub fn matches(&self, name: &str) -> bool {
        self.patterns.iter().any(|p| glob_match(p, name))
    }
}

/// Minimal glob: `*` any run of non-separator
/// chars, `**` any run of chars, `?` one char.
fn glob_match(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = name.chars().collect();
    glob_rec(&pat, 0, &txt, 0)
}

fn glob_rec(pat: &[char], mut pi: usize, txt: &[char], mut ti: usize) -> bool {
    loop {
        if pi == pat.len() {
            return ti == txt.len();
        }
        match pat[pi] {
            '*' if pi + 1 < pat.len() && pat[pi + 1] == '*' => {
                // `**` matches anything (incl. separators)
                pi += 2;
                if pi == pat.len() {
                    return true;
                }
                while ti <= txt.len() {
                    if glob_rec(pat, pi, txt, ti) {
                        return true;
                    }
                    ti += 1;
                }
                return false;
            }
            '*' => {
                pi += 1;
                while ti <= txt.len() {
                    if glob_rec(pat, pi, txt, ti) {
                        return true;
                    }
                    if ti < txt.len() && txt[ti] == '/' {
                        break;
                    }
                    ti += 1;
                }
                return false;
            }
            '?' => {
                if ti == txt.len() || txt[ti] == '/' {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti == txt.len() {
                    return false;
                }
                let lc = c.to_ascii_lowercase();
                let tc = txt[ti].to_ascii_lowercase();
                if lc != tc {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
}

/// One search hit.
#[derive(Clone, Debug)]
pub struct FileMatch {
    pub path: String,
    pub line: usize,
    pub col: usize,
    pub text: String,
}

/// Search parameters.
#[derive(Clone, Debug)]
pub struct SearchQuery {
    pub pattern: String,
    /// When set, files are rewritten with this
    /// replacement (`$1` group references).
    pub replace: Option<String>,
    /// Root path (file or directory).
    pub path: String,
    pub includes: GlobSet,
    pub excludes: GlobSet,
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
    /// 查找不匹配的行: report the lines that do *not* match instead of the
    /// ones that do.
    pub invert: bool,
    /// The find-in-files dialog's encoding choice, as a code page: -1
    /// (自动探测) keeps the walker's own lossy-UTF-8 behaviour, 65001 is
    /// UTF-8, 1200/1201 the two UTF-16 forms; any other code page falls
    /// back to lossy UTF-8. The ripgrep backend maps the well-known pages
    /// onto its `--encoding` labels.
    pub encoding: i32,
    /// Safety caps.
    pub max_files: usize,
    pub max_depth: usize,
}

impl SearchQuery {
    pub fn new(pattern: &str, path: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            replace: None,
            path: path.to_string(),
            includes: GlobSet::default(),
            excludes: GlobSet::default(),
            case_sensitive: false,
            regex: false,
            whole_word: false,
            invert: false,
            encoding: -1,
            max_files: 20_000,
            max_depth: 16,
        }
    }
}

/// Backend that produced the outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchBackend {
    Rg,
    Walk,
}

#[derive(Clone, Debug)]
pub struct SearchStats {
    pub backend: SearchBackend,
    pub files_scanned: usize,
    pub match_count: usize,
    pub duration_ms: u64,
}

#[derive(Clone, Debug)]
pub struct SearchOutcome {
    pub matches: Vec<FileMatch>,
    pub stats: SearchStats,
}

/// Is a `rg` binary available on the PATH?
fn rg_available() -> bool {
    static RG: OnceLock<bool> = OnceLock::new();
    *RG.get_or_init(|| {
        let mut cmd = Command::new("rg");
        cmd.arg("--version");
        crate::process::no_console_window(&mut cmd);
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    })
}

/// Run the search: ripgrep when available,
/// built-in walker otherwise.
pub fn run_search(q: &SearchQuery) -> SearchOutcome {
    run_search_with(q, &mut keep_going())
}

/// As [`run_search`], but reporting every finished file to `progress` and
/// stopping as soon as that callback returns `false` — the partial result set is
/// still returned, which is what the 查找中 dialog's 停止 button relies on.
pub fn run_search_with(q: &SearchQuery, progress: Progress<'_>) -> SearchOutcome {
    let started = Instant::now();
    // Every backend's report goes through this, so [`MAX_MATCHES`] is one rule
    // rather than a per-backend detail.
    let mut report = |path: &str, count: usize| count < MAX_MATCHES && progress(path, count);
    if rg_available() {
        match rg_search(q, &mut report) {
            Some((matches, files)) => {
                let stats = SearchStats {
                    backend: SearchBackend::Rg,
                    files_scanned: files,
                    match_count: matches.len(),
                    duration_ms: started.elapsed().as_millis() as u64,
                };
                return SearchOutcome { matches, stats };
            }
            None => {} // fall through to the walker
        }
    }
    let (matches, files) = walk_search(q, &mut report);
    let stats = SearchStats {
        backend: SearchBackend::Walk,
        files_scanned: files,
        match_count: matches.len(),
        duration_ms: started.elapsed().as_millis() as u64,
    };
    SearchOutcome { matches, stats }
}

/// List the files that contain a match (used
/// by replace-in-files before rewriting).
pub fn list_matching_files(q: &SearchQuery) -> Vec<String> {
    let outcome = run_search(q);
    let mut paths: Vec<String> = Vec::new();
    for m in &outcome.matches {
        if !paths.contains(&m.path) {
            paths.push(m.path.clone());
        }
    }
    paths
}

/// Outcome of a cross-file replace.
#[derive(Debug, Default)]
pub struct ReplaceReport {
    /// Files that were rewritten.
    pub files_rewritten: Vec<String>,
    /// Total replacements applied.
    pub replacements: usize,
    /// `(path, reason)` for files that
    /// could not be processed.
    pub errors: Vec<(String, String)>,
    /// Original contents of every rewritten
    /// file, so the host can roll the
    /// replace back.
    pub originals: Vec<(String, String)>,
}

/// Applies `q.replace` to every file matching
/// `q` — plain substring when `regex` is off,
/// regex with `$1` group references when on.
/// Binary (NUL-containing) and non-UTF-8
/// files are skipped and reported as errors.
pub fn replace_in_files(q: &SearchQuery) -> ReplaceReport {
    let mut report = ReplaceReport::default();
    let Some(replace) = q.replace.clone() else {
        return report;
    };
    let re = if q.regex { build_regex(q) } else { None };
    if q.regex && re.is_none() {
        report.errors.push((
            q.path.clone(),
            "invalid regex".to_string(),
        ));
        return report;
    }
    for path in list_matching_files(q) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                report.errors.push((path, e.to_string()));
                continue;
            }
        };
        if text.contains('\u{0}') {
            report.errors.push((
                path,
                "binary file skipped".to_string(),
            ));
            continue;
        }
        let count = match &re {
            Some(re) => re.find_iter(&text).count(),
            None => text.matches(&q.pattern).count(),
        };
        if count == 0 {
            continue;
        }
        let replaced = match &re {
            Some(re) => re
                .replace_all(&text, replace.as_str())
                .into_owned(),
            None => text.replace(&q.pattern, &replace),
        };
        if let Err(e) = std::fs::write(&path, &replaced) {
            report.errors.push((path, e.to_string()));
            continue;
        }
        report.replacements += count;
        report.files_rewritten.push(path.clone());
        report.originals.push((path, text));
    }
    report
}

// ------------------------------------------------------------------
// ripgrep backend
// ------------------------------------------------------------------

fn rg_search(q: &SearchQuery, progress: Progress<'_>) -> Option<(Vec<FileMatch>, usize)> {
    let mut cmd = Command::new("rg");
    cmd.arg("--vimgrep").arg("--no-heading").arg("--color=never");
    cmd.arg("-S"); // smart case
    if !q.case_sensitive {
        cmd.arg("--ignore-case");
    }
    if q.whole_word {
        cmd.arg("--word-regexp");
    }
    if !q.regex {
        cmd.arg("--fixed-strings");
    }
    if q.invert {
        cmd.arg("--invert-match");
    }
    // An explicit encoding the combo chose: ripgrep's own auto-detection
    // covers -1 and 65001, the others go through their WHATWG label.
    if q.encoding != -1 && q.encoding != 65001 {
        if let Some(label) = rg_encoding_label(q.encoding) {
            cmd.arg(format!("--encoding={label}"));
        }
    }
    for g in &q.includes.patterns {
        cmd.arg(format!("--glob={g}"));
    }
    for g in &q.excludes.patterns {
        cmd.arg(format!("--glob=!{g}"));
    }
    cmd.arg(&q.pattern).arg(&q.path);
    // Streaming, not `output()`: the dialog has to repaint and answer 停止
    // while ripgrep is still working, and `output()` blocks until EOF.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::process::no_console_window(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let mut reader = std::io::BufReader::new(stdout);
    let mut matches = Vec::new();
    let mut files = std::collections::HashSet::new();
    let mut stopped = false;
    // Bytes, not `read_line`: rg prints the file's own bytes, so a search
    // through non-UTF-8 files would otherwise end the stream on `InvalidData`.
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&buf);
        let Some(m) = parse_vimgrep(text.trim_end_matches(['\r', '\n'])) else {
            continue;
        };
        let path = m.path.clone();
        files.insert(path.clone());
        let count = matches.len() + 1;
        matches.push(m);
        if !progress(&path, count) {
            stopped = true;
            break;
        }
    }
    if stopped {
        // 停止: the partial hits are the result, so reaping the child must not
        // turn this into a `None` (which would restart the scan in the walker).
        let _ = child.kill();
        let _ = child.wait();
        return Some((matches, files.len()));
    }
    let status = child.wait().ok()?;
    if !status.success() && status.code() != Some(1) {
        // 1 = no matches; anything else = real error
        return None;
    }
    Some((matches, files.len()))
}

/// Parse a `--vimgrep` line:
/// `file:line:col:text`.  Windows paths may
/// contain colons (`C:\...`), so the line/col
/// pair is located by scanning for the first
/// `:<digits>:<digits>:` run.
fn parse_vimgrep(line: &str) -> Option<FileMatch> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b':' {
            i += 1;
            continue;
        }
        // candidate: `digits:digits:`
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == i + 1 || j >= bytes.len() || bytes[j] != b':' {
            i += 1;
            continue;
        }
        let mut k = j + 1;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        if k == j + 1 || k >= bytes.len() || bytes[k] != b':' {
            i += 1;
            continue;
        }
        let line_no: usize = line[i + 1..j].parse().ok()?;
        let col: usize = line[j + 1..k].parse().ok()?;
        if line_no == 0 || col == 0 {
            i += 1;
            continue;
        }
        return Some(FileMatch {
            path: line[..i].to_string(),
            line: line_no,
            // `--vimgrep` counts the column from 1.
            col: col - 1,
            text: line[k + 1..].to_string(),
        });
    }
    None
}

// ------------------------------------------------------------------
// built-in walker fallback
// ------------------------------------------------------------------

fn walk_search(q: &SearchQuery, progress: Progress<'_>) -> (Vec<FileMatch>, usize) {
    let re = match build_regex(q) {
        Some(re) => re,
        None => return (Vec::new(), 0),
    };
    let mut matches = Vec::new();
    let mut files = 0usize;
    let root = std::path::Path::new(&q.path);
    let mut ticked = 0usize;
    let mut shown = root.to_string_lossy().to_string();
    let mut queue: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((path, depth)) = queue.pop() {
        if matches.len() > 100_000 {
            break;
        }
        if path.is_dir() {
            if depth >= q.max_depth {
                continue;
            }
            let entries = match std::fs::read_dir(&path) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                // The 查找中 dialog only hears from the walk through this hook,
                // and a tree of tens of thousands of entries can run for seconds
                // between the files that pass the type filter - long enough that
                // 停止 looks dead. Yield on the enumeration itself, naming the
                // last file actually scanned so the edit does not claim a path
                // the search never read.
                ticked += 1;
                if ticked % 16 == 0 && !progress(&shown, matches.len()) {
                    return (matches, files);
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue; // skip hidden
                }
                let child = entry.path();
                // `file_type` comes from the directory listing; `Path::is_dir`
                // would stat every entry all over again.
                if entry
                    .file_type()
                    .map(|t| t.is_dir())
                    .unwrap_or_else(|_| child.is_dir())
                {
                    queue.push((child, depth + 1));
                } else if accept_file(&name, q) {
                    if files >= q.max_files {
                        continue;
                    }
                    shown = child.to_string_lossy().to_string();
                    if !scan_hooked(&child, &re, q, &mut matches, &mut files, progress) {
                        return (matches, files);
                    }
                }
            }
        } else if accept_file(&root.file_name().unwrap_or_default().to_string_lossy(), q) {
            scan_hooked(&path, &re, q, &mut matches, &mut files, progress);
        }
    }
    (matches, files)
}

/// `scan_file` plus the per-file progress report; `false` means 停止 was
/// pressed and the walk must unwind with what it has.
fn scan_hooked(
    path: &std::path::Path,
    re: &Regex,
    q: &SearchQuery,
    matches: &mut Vec<FileMatch>,
    files: &mut usize,
    progress: Progress<'_>,
) -> bool {
    scan_file(path, re, q, matches, files);
    progress(&path.to_string_lossy(), matches.len())
}

fn accept_file(name: &str, q: &SearchQuery) -> bool {
    if !q.excludes.is_empty() && q.excludes.matches(name) {
        return false;
    }
    if !q.includes.is_empty() && !q.includes.matches(name) {
        return false;
    }
    true
}

fn build_regex(q: &SearchQuery) -> Option<Regex> {
    let pattern = if q.whole_word {
        format!("\\b{}\\b", regex::escape(&q.pattern))
    } else if q.regex {
        q.pattern.clone()
    } else {
        regex::escape(&q.pattern)
    };
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!q.case_sensitive)
        .build()
        .ok()
}

/// Decode a file's bytes the way the find-in-files encoding choice asked
/// for. `-1` (自动探测) and every code page beyond the four below fall back to
/// lossy UTF-8, which is what the walker has always done.
fn decode_text(bytes: &[u8], encoding: i32) -> Option<String> {
    match encoding {
        1200 => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        1201 => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => Some(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// The ripgrep `--encoding` label for the well-known Windows code pages;
/// ripgrep takes any WHATWG label, so only the names that differ are listed.
fn rg_encoding_label(cp: i32) -> Option<&'static str> {
    Some(match cp {
        1200 => "utf-16le",
        1201 => "utf-16be",
        936 => "gbk",
        950 => "big5",
        932 => "shift_jis",
        949 => "euc-kr",
        1250 => "windows-1250",
        1251 => "windows-1251",
        1252 => "windows-1252",
        1253 => "windows-1253",
        1254 => "windows-1254",
        1255 => "windows-1255",
        1256 => "windows-1256",
        1257 => "windows-1257",
        1258 => "windows-1258",
        874 => "windows-874",
        28591 => "iso-8859-1",
        28592 => "iso-8859-2",
        28594 => "iso-8859-4",
        28595 => "iso-8859-5",
        28596 => "iso-8859-6",
        28597 => "iso-8859-7",
        28598 => "iso-8859-8",
        28599 => "iso-8859-9",
        28605 => "iso-8859-15",
        20866 => "koi8-r",
        21866 => "koi8-u",
        20127 => "us-ascii",
        65000 => "utf-7",
        _ => return None,
    })
}

fn scan_file(
    path: &std::path::Path,
    re: &Regex,
    q: &SearchQuery,
    matches: &mut Vec<FileMatch>,
    files: &mut usize,
) {
    *files += 1;
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    // The two UTF-16 forms carry NULs in their own bytes, so their binary
    // check runs on the decoded text instead.
    let utf16 = q.encoding == 1200 || q.encoding == 1201;
    if !utf16 {
        // binary heuristic: NUL byte in the first 8 KB
        let head = &bytes[..bytes.len().min(8192)];
        if head.contains(&0) {
            return;
        }
    }
    let text = match decode_text(&bytes, q.encoding) {
        Some(t) => t,
        None => return,
    };
    if utf16 && text.contains('\u{0}') {
        return;
    }
    // line-start table for offset -> line/col
    let mut starts: Vec<usize> = vec![0];
    for (i, c) in text.char_indices() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    if q.invert {
        // 查找不匹配的行: every line the regex does not hit, reported whole
        // at column 1 — the results list shows it exactly as the file has it.
        for (idx, &start) in starts.iter().enumerate() {
            if matches.len() >= MAX_MATCHES {
                break;
            }
            // A final "\n" does not start another line: grep reports nothing
            // for it, and neither does the inverted scan.
            if idx > 0 && start >= text.len() {
                break;
            }
            let end = starts.get(idx + 1).copied().unwrap_or(text.len());
            let mut line = &text[start..end];
            if let Some(stripped) = line.strip_suffix('\n') {
                line = stripped;
            }
            if let Some(stripped) = line.strip_suffix('\r') {
                line = stripped;
            }
            if re.find(line).is_some() {
                continue;
            }
            matches.push(FileMatch {
                path: path.to_string_lossy().to_string(),
                line: idx + 1,
                col: 0,
                text: line.to_string(),
            });
        }
        return;
    }
    for hit in re.find_iter(&text) {
        if matches.len() >= MAX_MATCHES {
            break;
        }
        let off = hit.start();
        // binary search the line
        let line_idx = match starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_no = line_idx + 1;
        let col = text[starts[line_idx]..off].chars().count();
        // The line is already delimited by the table above: `text.lines()
        //.nth(line_idx)` would rescan the file from byte 0 for every hit,
        // which is quadratic in the match count and holds the 查找中 dialog's
        // thread for seconds on one file.
        let line_end = starts.get(line_idx + 1).copied().unwrap_or(text.len());
        let mut end = line_end;
        let bytes = text.as_bytes();
        while end > starts[line_idx] && (bytes[end - 1] == 10 || bytes[end - 1] == 13) {
            end -= 1; // the line, without its own terminator
        }
        let line_text = text[starts[line_idx]..end].to_string();
        matches.push(FileMatch {
            path: path.to_string_lossy().to_string(),
            line: line_no,
            col,
            text: line_text,
        });
    }
}

// ------------------------------------------------------------------
// the 查找结果 document
// ------------------------------------------------------------------

/// The results document buffer: one block per file — the path row `@~` + the path, then
/// every match as the `"\t(%d,%d):"` literal with the line's own text behind it, then
/// `\t共找到%d行.` (`zh-CN.lng` 15099) — and the folder total `共找到%d行,在文件夹%s中` (15049) last,
/// with no tab and no newline of its own. 仅显示文件路径 keeps only the path rows. A path
/// relative to the search root would resolve nowhere. It is really in the text: the
/// editor hides attribute class 30 (the caret-line painter's masked-class gate) on every
/// row but the caret's, so the two characters take neither colour nor width until the
/// caret lands on the line and the path still starts flush against the gutter.
pub fn results_document(q: &SearchQuery, matches: &[FileMatch], only_name: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut order: Vec<&str> = Vec::new();
    for m in matches {
        if !order.contains(&m.path.as_str()) {
            order.push(&m.path);
        }
    }
    for path in order {
        let rows: Vec<&FileMatch> = matches.iter().filter(|m| m.path == path).collect();
        let _ = writeln!(out, "@~{path}");
        if only_name {
            continue;
        }
        for m in rows.iter() {
            // No separator of its own.
            let _ = writeln!(out, "\t({},{}):{}", m.line, m.col, m.text);
        }
        let _ = writeln!(out, "\t共找到{}行.", rows.len());
    }
    let _ = write!(out, "共找到{}行,在文件夹{}中", matches.len(), q.path);
    out
}

/// The tab's title: `zh-CN.lng [STRING] 15024`, `查找:[%s]在[%s]中`.
pub fn results_title(pattern: &str, folder: &str) -> String {
    format!("查找:[{pattern}]在[{folder}]中")
}

/// What a double-click on row `row` of a [`results_document`] opens: the file the
/// row's block belongs to and the position the row names, as 0-based line and
/// column (the anchor resolve: `StrToIntW(line group) - 1`, `StrToIntW(col
/// group)`). Only a row that carries the `"\t(line,col):"` prefix names a target.
/// Jumps only if that row starts with `@~` — so a path row, the per-file totals
/// and the folder total answer `None` and the click falls through to the ordinary
/// word selection.
pub fn results_target(
    row: usize,
    mut line: impl FnMut(usize) -> Option<String>,
) -> Option<(String, usize, usize)> {
    let text = line(row)?;
    let (line_no, col) = row_position(&text)?;
    let mut idx = row.saturating_sub(1);
    loop {
        let t = line(idx)?;
        if !t.starts_with('\t') {
            return path_row(&t).map(|p| (p.to_string(), line_no - 1, col));
        }
        if idx == 0 {
            return None;
        }
        idx -= 1;
    }
}

/// The path a `@~` row names, or `None` for any other row.
fn path_row(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("@~")?;
    (!rest.is_empty()).then_some(rest)
}

/// The `(line,col):` the row starts with, if it has one.
fn row_position(text: &str) -> Option<(usize, usize)> {
    let rest = text.strip_prefix('\t')?.strip_prefix('(')?;
    let (digits, rest) = rest.split_once(',')?;
    let line_no: usize = digits.parse().ok()?;
    let end = rest.find(')')?;
    let col: usize = rest[..end].parse().ok()?;
    if rest[end + 1..].starts_with(':') && line_no > 0 {
        Some((line_no, col))
    } else {
        None
    }
}

/// The target 下一个结果 / 上一个结果 walks to. The leading `.*` is greedy, so the row's *last*
/// `(line,col):` is the one that matches, and the path is everything before its opening
/// parenthesis — the group the anchor resolve opens. The position is a 0-based line and
/// column, resolved exactly as [`results_target`] resolves the results document's own
/// `(line,col)`.
pub fn output_target(text: &str) -> Option<(String, usize, usize)> {
    let bytes = text.as_bytes();
    let mut at = bytes.len();
    while at > 0 {
        at -= 1;
        if bytes[at] != b'(' {
            continue;
        }
        let Some((line, col)) = inline_position(&bytes[at + 1..]) else {
            continue;
        };
        let path = &text[..at];
        if path.is_empty() {
            continue;
        }
        return Some((path.to_string(), line - 1, col));
    }
    None
}

fn inline_position(rest: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    let line = digits(rest, &mut i)?;
    if line == 0 || rest.get(i) != Some(&b',') {
        return None;
    }
    i += 1;
    let col = digits(rest, &mut i)?;
    if rest.get(i) != Some(&b')') || rest.get(i + 1) != Some(&b':') {
        return None;
    }
    Some((line, col))
}

/// One run of decimal digits, and where the scan left off.
fn digits(rest: &[u8], i: &mut usize) -> Option<usize> {
    let start = *i;
    while matches!(rest.get(*i), Some(c) if c.is_ascii_digit()) {
        *i += 1;
    }
    if *i == start {
        return None;
    }
    std::str::from_utf8(&rest[start..*i]).ok()?.parse().ok()
}

/// The attribute classes the tokenizer writes into
/// the editor's per-character attribute array for one row of the results
/// document. The colour resolve resolves the id against the theme's colour pairs: 7
/// is the `Url` key, 10 `Found`, 17 `Word2`; 30 has no key of its own, so it
/// takes the `Default` pair — and the line painter skips it off the caret row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowClass {
    /// The `@~` marker of a path row: class 30.
    Marker,
    /// The path itself: class 7.
    Path,
    /// The whole `"\t(line,col):"` prefix: class 17.
    Position,
    /// One occurrence of the pattern the run searched for: class 10.
    Found,
}

/// The pattern the rows are re-scanned with. Every row answers to that again as it is
/// painted.
#[derive(Clone, Debug)]
pub struct RowHighlighter {
    pattern: String,
    /// Compiled once for the document: 正则 mode searches with the user's own
    /// expression, plain mode with the substring search below.
    re: Option<Regex>,
    case_sensitive: bool,
    whole_word: bool,
}

impl RowHighlighter {
    pub fn from_query(q: &SearchQuery) -> Self {
        Self {
            pattern: q.pattern.clone(),
            re: if q.regex { build_regex(q) } else { None },
            case_sensitive: q.case_sensitive,
            whole_word: q.whole_word,
        }
    }
}

/// One row of a [`results_document`], classified into character ranges, in the
/// order the painter consumes them.
pub fn style_row(row: &str, hl: Option<&RowHighlighter>) -> Vec<(usize, usize, RowClass)> {
    let chars = row.chars().count();
    if chars < 3 {
        return Vec::new();
    }
    if let Some(path) = path_row(row) {
        let marker = chars - path.chars().count();
        let mut out = vec![(0usize, marker, RowClass::Marker)];
        if chars > marker {
            out.push((marker, chars, RowClass::Path));
        }
        return out;
    }
    if row_position(row).is_none() {
        return Vec::new();
    }
    let end = position_len(row);
    let mut out = vec![(0usize, end, RowClass::Position)];
    let Some(hl) = hl else { return out };
    let tail = &row[end..];
    if let Some(re) = hl.re.as_ref() {
        // the offsets shift back by the prefix it skipped.
        let mut byte = 0usize;
        let mut chr = end;
        for hit in re.find_iter(tail) {
            chr += tail[byte..hit.start()].chars().count();
            let len = tail[hit.start()..hit.end()].chars().count();
            if len > 0 {
                out.push((chr, chr + len, RowClass::Found));
            }
            byte = hit.end();
            chr += len;
        }
        return out;
    }
    if hl.pattern.is_empty() {
        return out;
    }
    // The substring branch steps the scan start one character past each hit's
    // start, so an occurrence that overlaps the last one is reported too.
    let hay: Vec<char> = tail.chars().collect();
    let nee: Vec<char> = hl.pattern.chars().collect();
    let mut i = 0usize;
    while i + nee.len() <= hay.len() {
        if plain_hit(&hay, i, &nee, hl.case_sensitive)
            && (!hl.whole_word || word_edge(&hay, i, i + nee.len()))
        {
            out.push((end + i, end + i + nee.len(), RowClass::Found));
            i += 1;
            continue;
        }
        i += 1;
    }
    out
}

/// One substring hit at `from` do: the exact characters when 区分大小写, simple case folding when
/// it does not.
fn plain_hit(hay: &[char], from: usize, nee: &[char], case_sensitive: bool) -> bool {
    hay[from..from + nee.len()]
        .iter()
        .zip(nee)
        .all(|(h, n)| *h == *n || (!case_sensitive && h.to_lowercase().eq(n.to_lowercase())))
}

/// Any neighbour that is a word character or `_` disqualifies it.
fn word_edge(hay: &[char], from: usize, to: usize) -> bool {
    let free = |c: Option<&char>| !c.is_some_and(|c| c.is_alphanumeric() || *c == '_');
    free(from.checked_sub(1).and_then(|i| hay.get(i))) && free(hay.get(to))
}

/// The character length of the `"\t(line,col):"` a position row starts with.
fn position_len(row: &str) -> usize {
    // The prefix is pure ASCII, so the byte index of its `:` is its character
    // count too, and `row_position` has already checked the shape.
    row.find(':').map_or_else(|| row.chars().count(), |b| b + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_basics() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("*.rs", "src/main.rs") == false, "single * does not cross /");
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("main.?", "main.r"));
        assert!(!glob_match("main.?", "main.rs"), "? is exactly one char");
        assert!(glob_match("main.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.txt"));
    }

    #[test]
    fn globset_include_exclude() {
        let inc = GlobSet::new(&["*.rs".to_string()]);
        assert!(inc.matches("main.rs"));
        assert!(!inc.matches("main.txt"));
        let exc = GlobSet::new(&["!*.tmp".to_string()]);
        // leading ! handled by the caller as exclusion;
        // raw match still works on the name
        assert!(exc.matches("!*.tmp"));
    }

    #[test]
    fn parse_vimgrep_plain() {
        let m = parse_vimgrep("src/main.rs:12:3:let x = 1;").unwrap();
        assert_eq!(m.path, "src/main.rs");
        assert_eq!(m.line, 12);
        // `--vimgrep` counts from 1; the results row counts from 0.
        assert_eq!(m.col, 2);
        assert_eq!(m.text, "let x = 1;");
    }

    #[test]
    fn parse_vimgrep_windows_drive() {
        let m = parse_vimgrep("C:\\proj\\a.txt:5:10:hello: world").unwrap();
        assert_eq!(m.path, "C:\\proj\\a.txt");
        assert_eq!(m.line, 5);
        assert_eq!(m.col, 9);
        assert_eq!(m.text, "hello: world");
    }

    #[test]
    fn parse_vimgrep_invalid() {
        assert!(parse_vimgrep("not a vimgrep line").is_none());
        assert!(parse_vimgrep("a.txt:0:3:x").is_none());
    }

    #[test]
    fn walk_search_finds_text() {
        // search the crate's own source for a known symbol
        let q = SearchQuery {
            pattern: "run_search".to_string(),
            path: ".".to_string(),
            includes: GlobSet::new(&["*.rs".to_string()]),
            excludes: GlobSet::default(),
            case_sensitive: true,
            regex: false,
            whole_word: false,
            invert: false,
            encoding: -1,
            replace: None,
            max_files: 20_000,
            max_depth: 16,
        };
        let (matches, _files) = walk_search(&q, &mut keep_going());
        assert!(!matches.is_empty());
        assert!(matches.iter().all(|m| m.path.ends_with(".rs")));
        assert!(matches.iter().any(|m| m.text.contains("run_search")));
    }

    /// A hit carries the line exactly as the file has it, minus its own
    /// terminator - and the line is sliced out of the offset table, so a CRLF
    /// file must not leak the CR into the reported text.
    #[test]
    fn scan_file_reports_the_line_without_its_terminator() {
        let dir = temp_replace_dir("crlf");
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"one alpha");
        body.push(13);
        body.push(10);
        body.extend_from_slice(b"two beta");
        body.push(10);
        std::fs::write(dir.join("a.txt"), &body).unwrap();
        let q = SearchQuery {
            path: dir.to_string_lossy().into_owned(),
            includes: GlobSet::new(&["*.txt".to_string()]),
            ..SearchQuery::new("alpha", ".")
        };
        let (matches, files) = walk_search(&q, &mut keep_going());
        assert_eq!(files, 1);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "one alpha");
        assert_eq!((matches[0].line, matches[0].col), (1, 4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Their own bytes are full of NULs.
    #[test]
    fn decode_text_handles_utf16() {
        let le: Vec<u8> = "alpha"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(decode_text(&le, 1200).unwrap(), "alpha");
        let be: Vec<u8> = "alpha"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(decode_text(&be, 1201).unwrap(), "alpha");
        assert_eq!(decode_text(b"alpha", -1).unwrap(), "alpha");
        // 自动探测 and any unlisted page keep the walker's lossy behaviour.
        assert_eq!(decode_text(b"alpha", 936).unwrap(), "alpha");
    }

    #[test]
    fn walk_search_reads_utf16le_files() {
        let dir = std::env::temp_dir().join("dg_fs_utf16");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("u16.txt");
        let bytes: Vec<u8> = "alpha line
beta line
"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        std::fs::write(&path, &bytes).unwrap();
        let q = SearchQuery {
            pattern: "alpha".to_string(),
            path: dir.to_string_lossy().into_owned(),
            includes: GlobSet::default(),
            excludes: GlobSet::default(),
            case_sensitive: false,
            regex: false,
            whole_word: false,
            invert: false,
            encoding: 1200,
            replace: None,
            max_files: 100,
            max_depth: 2,
        };
        let (matches, files) = walk_search(&q, &mut keep_going());
        assert_eq!(files, 1);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].text.contains("alpha"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_search_whole_word_and_case() {
        let q = SearchQuery {
            pattern: format!("Run_{}", "search"),
            path: ".".to_string(),
            includes: GlobSet::new(&["*.rs".to_string()]),
            excludes: GlobSet::default(),
            case_sensitive: true,
            regex: false,
            whole_word: false,
            invert: false,
            encoding: -1,
            replace: None,
            max_files: 20_000,
            max_depth: 16,
        };
        let (matches, _) = walk_search(&q, &mut keep_going());
        assert!(matches.is_empty(), "case-sensitive must not match run_search");
    }

    #[test]
    fn walk_search_invert_reports_none_matched_lines() {
        let dir = temp_replace_dir("invert");
        std::fs::write(
            dir.join("a.txt"),
            b"alpha line\nbeta line\nalpha again\ngamma\n",
        )
        .unwrap();
        let q = SearchQuery {
            pattern: "alpha".to_string(),
            path: dir.to_str().unwrap().to_string(),
            includes: GlobSet::new(&["*.txt".to_string()]),
            invert: true,
            ..SearchQuery::new("alpha", ".")
        };
        let (matches, _) = walk_search(&q, &mut keep_going());
        // lines 2 and 4 do not contain "alpha"
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].text, "beta line");
        assert_eq!(matches[1].line, 4);
        assert_eq!(matches[1].text, "gamma");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_search_reports_backend() {
        let q = SearchQuery::new("run_search", ".");
        let out = run_search(&q);
        assert!(!out.matches.is_empty());
        assert!(out.stats.files_scanned > 0);
        let _ms = out.stats.duration_ms;
    }

    /// The 查找中 dialog drives both its live 行数 counter and its 停止 button
    /// off one callback, so it must see every file and be able to end the walk
    /// with the matches found so far.
    #[test]
    fn progress_sees_files_and_can_stop_the_walk() {
        let dir = temp_replace_dir("progress");
        for n in 0..4 {
            std::fs::write(dir.join(format!("f{n}.txt")), b"alpha\n").unwrap();
        }
        let q = SearchQuery {
            path: dir.to_str().unwrap().to_string(),
            includes: GlobSet::new(&["*.txt".to_string()]),
            ..SearchQuery::new("alpha", ".")
        };
        let mut seen: Vec<String> = Vec::new();
        let dir_str = dir.to_string_lossy().to_string();
        let all = {
            let mut hook = |path: &str, count: usize| {
                assert_eq!(count, seen.len() + 1, "count tracks the matches so far");
                assert!(path.starts_with(&dir_str), "{path}");
                seen.push(path.to_string());
                true
            };
            walk_search(&q, &mut hook)
        };
        assert_eq!(all.0.len(), 4);
        assert_eq!(seen.len(), 4);
        let mut stops = 0usize;
        let stopped = {
            let mut hook = |_: &str, _: usize| {
                stops += 1;
                stops < 2
            };
            walk_search(&q, &mut hook)
        };
        assert_eq!(stops, 2, "the hook is asked once per file");
        assert_eq!(stopped.0.len(), 2, "partial results are kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn results_document_writes_marker_path_then_line_column() {
        let q = SearchQuery::new("alpha", "E:\\dogedit");
        let matches = vec![
            FileMatch {
                path: "a.txt".to_string(),
                line: 3,
                col: 1,
                text: "alpha one".to_string(),
            },
            FileMatch {
                path: "b.txt".to_string(),
                line: 7,
                col: 4,
                text: "x alpha y".to_string(),
            },
            FileMatch {
                path: "a.txt".to_string(),
                line: 9,
                col: 2,
                text: "z alpha".to_string(),
            },
        ];
        let doc = results_document(&q, &matches, false);
        let lines: Vec<&str> = doc.split('\n').collect();
        assert_eq!(lines[0], "@~a.txt");
        assert_eq!(lines[1], "\t(3,1):alpha one");
        assert_eq!(lines[2], "\t(9,2):z alpha");
        assert_eq!(lines[3], "\t共找到2行.");
        assert_eq!(lines[4], "@~b.txt");
        assert_eq!(lines[6], "\t共找到1行.");
        assert_eq!(lines[7], "共找到3行,在文件夹E:\\dogedit中");
        // 仅显示文件路径: the path lines, then the folder total.
        let only = results_document(&q, &matches, true);
        let only: Vec<&str> = only.split('\n').collect();
        assert_eq!(only[0], "@~a.txt");
        assert_eq!(only[1], "@~b.txt");
        assert_eq!(only[2], "共找到3行,在文件夹E:\\dogedit中");
    }

    #[test]
    fn results_title_is_the_resources_own_format() {
        assert_eq!(
            results_title("ok", "E:\\dogedit"),
            "查找:[ok]在[E:\\dogedit]中"
        );
    }

    #[test]
    fn results_target_walks_up_to_its_path_row() {
        let doc = "@~E:\\x\\a.txt\n\t(12,5):hello\n\t共找到1行.";
        let lines: Vec<String> = doc.lines().map(|s| s.to_string()).collect();
        let get = |i: usize| lines.get(i).cloned();
        // Takes the path from the first non-tab row above, marker stripped.
        assert_eq!(
            results_target(1, get),
            Some(("E:\\x\\a.txt".to_string(), 11, 5))
        );
        // The total row names no position: nothing to open, so the click falls
        // through to the ordinary word selection
        let lines2 = lines.clone();
        assert_eq!(results_target(2, |i| lines2.get(i).cloned()), None);
        assert_eq!(
            results_target(0, |i| lines.get(i).cloned()),
            None
        );
    }

    /// The 输出窗口's own grammar, `^(.*)\((\d+),(\d+)\):`: the path and the
    /// position live in one row, and the greedy `.*` makes the row's *last*
    /// `(line,col):` the match — which is what 下一个结果 walks between.
    #[test]
    fn output_target_takes_the_row_s_last_position() {
        assert_eq!(
            output_target("E:\\x\\a.txt(12,5): matched here"),
            Some(("E:\\x\\a.txt".to_string(), 11, 5))
        );
        // Greedy: the earlier candidate is left to the path.
        assert_eq!(
            output_target("log(1,0).txt(7,3): two candidates"),
            Some(("log(1,0).txt".to_string(), 6, 3))
        );
        // A position without its colon, a zero line, or an empty path group is
        // not an anchor at all.
        assert_eq!(output_target("a.txt(12,5) no colon"), None);
        assert_eq!(output_target("a.txt(0,5):"), None);
        assert_eq!(output_target("(12,5):"), None);
        // Non-ASCII paths keep their bytes: the split happens at an ASCII '('.
        assert_eq!(
            output_target("E:\\文档\\说明.md(3,0):"),
            Some(("E:\\文档\\说明.md".to_string(), 2, 0))
        );
    }

    /// the tokenizer writing the attribute array for one row: the marker and
    /// the path on a `@~` row, the whole `"\t(line,col):"` match plus the
    /// pattern's own occurrences on a position row, nothing anywhere else.
    #[test]
    fn style_row_classes_the_marker_and_the_path() {
        assert_eq!(
            style_row("@~E:\\x\\a.txt", None),
            vec![(0, 2, RowClass::Marker), (2, 12, RowClass::Path)]
        );
        // A path whose text is not ASCII still classifies by characters.
        assert_eq!(
            style_row("@~E:\\中文\\a.txt", None),
            vec![(0, 2, RowClass::Marker), (2, 13, RowClass::Path)]
        );
        let hl = RowHighlighter::from_query(&{
            let mut q = SearchQuery::new("alpha", "E:\\x");
            q.case_sensitive = true;
            q
        });
        assert_eq!(
            style_row("\t(12,5):hello alpha", Some(&hl)),
            vec![
                (0, 8, RowClass::Position),
                (14, 19, RowClass::Found),
            ]
        );
        // Rows the two rules do not name keep the theme's plain colours.
        assert!(style_row("\t共找到1行.", Some(&hl)).is_empty());
        assert!(style_row("共找到3行,在文件夹E:\\x中", Some(&hl)).is_empty());
    }

    /// The pattern scan starts at the position prefix's end and 忽略大小写 /
    /// 全字匹配 / 正则表达式 work the way the highlighter's three finders do.
    #[test]
    fn style_row_scans_the_pattern_with_its_own_options() {
        let mut q = SearchQuery::new("ab", "E:\\x");
        q.case_sensitive = false;
        let hl = RowHighlighter::from_query(&q);
        assert_eq!(
            found_spans("\t(1,0):x abaY ab", Some(&hl)),
            vec![(9, 11), (14, 16)]
        );
        // Overlapping hits are all reported: the substring branch steps the scan
        // start one character past the hit it just coloured.
        q.pattern = "aa".to_string();
        let hl = RowHighlighter::from_query(&q);
        assert_eq!(found_spans("\t(1,0):aaa", Some(&hl)), vec![(7, 9), (8, 10)]);
        q.pattern = "ab".to_string();
        q.whole_word = true;
        let hl = RowHighlighter::from_query(&q);
        assert_eq!(
            found_spans("\t(1,0):ab abc", Some(&hl)).len(),
            1,
            "the hit inside `abc` has a word character on its right"
        );
        // 正则 mode takes the other branch: hits never overlap, and the
        // expression is the user's.
        let mut q = SearchQuery::new("a+b", "E:\\x");
        q.regex = true;
        q.case_sensitive = true;
        let hl = RowHighlighter::from_query(&q);
        assert_eq!(found_spans("\t(1,0):aabb aab", Some(&hl)), vec![(7, 10), (12, 15)]);
    }

    /// Only the `Found` ranges of [`style_row`], as `(start, end)`.
    fn found_spans(row: &str, hl: Option<&RowHighlighter>) -> Vec<(usize, usize)> {
        style_row(row, hl)
            .into_iter()
            .filter(|(_, _, c)| *c == RowClass::Found)
            .map(|(s, e, _)| (s, e))
            .collect()
    }

    /// A pattern that would match millions of lines stops at [`MAX_MATCHES`]
    /// instead of handing the caller an unbounded hit set.
    #[test]
    fn run_search_stops_at_the_match_cap() {
        let dir = temp_replace_dir("cap");
        for n in 0..130 {
            let body = "a\n".repeat(1000);
            std::fs::write(dir.join(format!("f{n}.txt")), body.as_bytes()).unwrap();
        }
        let q = SearchQuery {
            path: dir.to_str().unwrap().to_string(),
            includes: GlobSet::new(&["*.txt".to_string()]),
            ..SearchQuery::new("a", ".")
        };
        let out = run_search(&q);
        assert!(
            out.matches.len() >= MAX_MATCHES && out.matches.len() <= MAX_MATCHES + 2_000,
            "{} hits for a 130 000-line tree",
            out.matches.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_replace_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dg_filesearch_replace_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replace_in_files_plain() {
        let dir = temp_replace_dir("plain");
        std::fs::write(dir.join("a.txt"), b"foo bar foo").unwrap();
        std::fs::write(dir.join("b.txt"), b"no match here").unwrap();
        let q = SearchQuery {
            pattern: "foo".to_string(),
            replace: Some("baz".to_string()),
            path: dir.to_str().unwrap().to_string(),
            ..SearchQuery::new("foo", ".")
        };
        let report = replace_in_files(&q);
        assert_eq!(report.files_rewritten.len(), 1);
        assert_eq!(report.replacements, 2);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "baz bar baz"
        );
        // the untouched file keeps its content
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "no match here"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_in_files_regex_groups() {
        let dir = temp_replace_dir("regex");
        std::fs::write(
            dir.join("a.txt"),
            b"let x = 1; let y = 2;",
        )
        .unwrap();
        let q = SearchQuery {
            pattern: r"let (\w+) =".to_string(),
            replace: Some("var $1 =".to_string()),
            path: dir.to_str().unwrap().to_string(),
            regex: true,
            ..SearchQuery::new("let", ".")
        };
        let report = replace_in_files(&q);
        assert_eq!(report.replacements, 2);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "var x = 1; var y = 2;"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_in_files_keeps_originals_for_rollback() {
        let dir = temp_replace_dir("rollback");
        std::fs::write(dir.join("a.txt"), b"one two one").unwrap();
        let q = SearchQuery {
            pattern: "one".to_string(),
            replace: Some("1".to_string()),
            path: dir.to_str().unwrap().to_string(),
            ..SearchQuery::new("one", ".")
        };
        let report = replace_in_files(&q);
        assert_eq!(report.originals.len(), 1);
        assert_eq!(report.originals[0].1, "one two one");
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "1 two 1"
        );
        // rollback: restore the originals
        for (path, original) in &report.originals {
            std::fs::write(path, original).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "one two one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
