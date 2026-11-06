//! In-memory file index for Goto Anything (`cm_goto_anything`).
//!
//! A worker thread walks the tree into `Vec<FileEntry>`; [`query`] runs the
//! fuzzy subsequence matcher over it and ranks by score, then shorter name,
//! then name. The walker records one directory stamp per listed directory, so
//! [`revalidate`] re-stats directories instead of files.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Directory names that never belong in a code index. A Rust `target\` or a
/// `node_modules\` can eat the whole twenty-thousand budget on its own,
/// pushing the files the user actually wants out of the index. Matched
/// case-insensitively, directories only. Deliberately *not* in the list:
/// `src\bin` (Rust's extra binaries live there), `test`/`tests`, `docs`,
/// `examples` — real sources, and a file named `target.rs` is a file, so only
/// directories are filtered.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "target",
    "obj",
    "__pycache__",
    ".vs",
    ".idea",
    ".git",
    ".hg",
    ".svn",
    "vendor",
];

fn is_skipped_dir(name: &str) -> bool {
    SKIP_DIRS
        .iter()
        .any(|s| s.eq_ignore_ascii_case(name))
}

/// The skip list, for callers that run their own walker over the same tree —
/// the workspace `ctags -R` index passes each of these as `--exclude`, so the
/// two indexes cover the same files.
pub fn skip_dirs() -> &'static [&'static str] {
    SKIP_DIRS
}

/// One indexed file: the name is what matching runs against, the path is
/// what gets opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
}

/// A directory the walker listed, and the mtime it had when it was listed.
/// [`revalidate`] compares that mtime against the disk to decide whether the
/// listing can still be trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirStamp {
    pub path: String,
    pub mtime: Option<SystemTime>,
}

/// What [`walk_tree`] produces: the files, and the stamps that let the next
/// open skip the walk.
#[derive(Clone, Debug, Default)]
pub struct WalkResult {
    pub entries: Vec<FileEntry>,
    pub dirs: Vec<DirStamp>,
}

/// Walk caps. The same shape as `filesearch::SearchQuery`'s limits: a deep or
/// huge tree must not turn one Ctrl+P into a whole-drive scan.
#[derive(Clone, Copy, Debug)]
pub struct WalkOptions {
    /// Files collected before the walk stops early.
    pub max_files: usize,
    /// Directory levels below `root`.
    pub max_depth: usize,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            max_files: 20_000,
            max_depth: 16,
        }
    }
}

fn mtime_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// One directory's plain files and immediate subdirectories, with the stamps
/// the incremental repair needs. Directories whose name is skipped or hidden
/// never appear in `subdirs`.
#[derive(Clone, Debug, Default)]
pub struct DirListing {
    pub files: Vec<FileEntry>,
    pub subdirs: Vec<DirStamp>,
}

/// Lists `dir` (one level, no recursion). This is what the incremental repair
/// calls for the directories whose mtime moved.
pub fn list_dir(dir: &str) -> DirListing {
    let mut out = DirListing::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let child = entry.path();
        let is_dir = entry
            .file_type()
            .map(|t| t.is_dir())
            .unwrap_or_else(|_| child.is_dir());
        if is_dir {
            if !is_skipped_dir(&name) {
                out.subdirs.push(DirStamp {
                    path: child.to_string_lossy().into_owned(),
                    mtime: mtime_of(&child),
                });
            }
        } else if let Some(path) = child.to_str() {
            out.files.push(FileEntry {
                name,
                path: path.to_string(),
            });
        }
    }
    out
}

/// Collects the files under `root`, breadth-first, skipping hidden entries —
/// the same rule the find-in-files walker applies (`filesearch.rs`:
/// `name.starts_with('.')`). Directories recurse; every plain file becomes an
/// entry with its full path. Returns entries in walk order, capped at
/// `opts.max_files`, plus one stamp per directory that was listed.
pub fn walk_tree(root: &str, opts: &WalkOptions) -> WalkResult {
    let mut out = WalkResult::default();
    let root_path = Path::new(root);
    if !root_path.is_dir() {
        return out;
    }
    // Breadth-first queue of (path, depth), same shape as the WalkBackend.
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((root_path.to_path_buf(), 0));
    while let Some((dir, depth)) = queue.pop_front() {
        if out.entries.len() >= opts.max_files {
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Stamp *before* listing: a change that lands between the stamp and the
        // read only makes us look stale, never the other way round.
        out.dirs.push(DirStamp {
            path: dir.to_string_lossy().into_owned(),
            mtime: mtime_of(&dir),
        });
        // Names sorted per directory so the walk order is stable for tests
        // and for the tie-break ordering of an empty query.
        let mut kids: Vec<_> = read.flatten().collect();
        kids.sort_by_key(|e| e.file_name());
        for entry in kids {
            if out.entries.len() >= opts.max_files {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue; // skip hidden
            }
            let child = entry.path();
            let is_dir = entry
                .file_type()
                .map(|t| t.is_dir())
                .unwrap_or_else(|_| child.is_dir());
            if is_dir {
                if depth < opts.max_depth && !is_skipped_dir(&name) {
                    queue.push_back((child, depth + 1));
                }
            } else if let Some(path) = child.to_str() {
                out.entries.push(FileEntry {
                    name,
                    path: path.to_string(),
                });
            }
        }
    }
    out
}

/// The entries only — the shape the first version of this module exposed.
pub fn walk_files(root: &str, opts: &WalkOptions) -> Vec<FileEntry> {
    walk_tree(root, opts).entries
}

/// Re-stats every recorded directory and returns the **indices** whose listing
/// may have changed (mtime moved, or the directory is gone).
///
/// This is the cheap half of "is the index still good": it never enumerates a
/// directory, so its cost is one `stat` per directory rather than one `stat`
/// per file. A create/delete/rename always bumps the parent's mtime on NTFS,
/// which is what makes it sound for a *file list* (content edits do not move it,
/// but they cannot change which files exist).
///
/// Caveats the caller has to cover: NTFS may delay a directory's timestamp
/// while a handle is open, and FAT/network shares may not maintain it at all.
/// That is why the dialog also forces a full walk now and then.
pub fn revalidate(dirs: &[DirStamp]) -> Vec<usize> {
    let mut changed = Vec::new();
    for (i, d) in dirs.iter().enumerate() {
        let now = mtime_of(Path::new(&d.path));
        if now != d.mtime {
            changed.push(i);
        }
    }
    changed
}

/// One query hit: which entry matched, how, and where the matched characters
/// sit in the name (for the row's highlight rendering).
#[derive(Clone, Debug)]
pub struct Hit {
    /// Index into the `entries` slice that was queried.
    pub entry: usize,
    /// Score and matched character positions from the fuzzy matcher.
    pub m: crate::fuzzy::FuzzyMatch,
}

/// Fuzzy-matches `pattern` against every entry's name and returns the best
/// `limit` hits, best first. An empty pattern matches nothing here: the
/// caller shows its own "open documents" rows for that case instead of a wall
/// of twenty thousand names.
pub fn query(entries: &[FileEntry], pattern: &str, limit: usize) -> Vec<Hit> {
    if pattern.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut hits: Vec<Hit> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if let Some(m) = crate::fuzzy::fuzzy_match(pattern, &e.name, crate::fuzzy::FuzzyMode::Fuzzy)
        {
            hits.push(Hit { entry: i, m });
        }
    }
    hits.sort_by(|a, b| {
        let (ea, eb) = (&entries[a.entry], &entries[b.entry]);
        b.m
            .score
            .cmp(&a.m.score)
            .then(ea.name.len().cmp(&eb.name.len()))
            .then(ea.name.cmp(&eb.name))
    });
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            path: format!(r"E:\tmp\{name}"),
        }
    }

    /// A scratch directory that cleans itself up.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "evidx_{tag}_{}_{:?}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&base).unwrap();
            Self(base)
        }

        fn mk(&self, rel: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn query_ranks_prefix_over_scatter_and_prefers_short_names() {
        let entries = vec![
            entry("zzz_myfile_long_name.txt"),
            entry("myfile.txt"),
            entry("unrelated.doc"),
        ];
        let hits = query(&entries, "myfile", 10);
        assert_eq!(hits.len(), 2);
        // The exact-substring hit scores higher than the scattered one.
        assert_eq!(entries[hits[0].entry].name, "myfile.txt");
        assert_eq!(entries[hits[1].entry].name, "zzz_myfile_long_name.txt");
    }

    #[test]
    fn query_matches_letters_in_order_as_a_subsequence() {
        // "chg" must find "changelog.txt" the way LIKE '%c%h%g%' does.
        let entries = vec![entry("changelog.txt"), entry("readme.md")];
        let hits = query(&entries, "chg", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(entries[hits[0].entry].name, "changelog.txt");
        // Positions are character offsets of c, h, g in the name.
        assert_eq!(hits[0].m.positions, vec![0, 1, 4]);
    }

    #[test]
    fn query_respects_the_limit() {
        let entries: Vec<FileEntry> = (0..50).map(|i| entry(&format!("file{i}.txt"))).collect();
        let hits = query(&entries, "file", 7);
        assert_eq!(hits.len(), 7);
    }

    #[test]
    fn empty_pattern_yields_nothing() {
        let entries = vec![entry("a.txt")];
        assert!(query(&entries, "", 10).is_empty());
    }

    #[test]
    fn walk_skips_hidden_and_lists_files_with_full_paths() {
        let s = Scratch::new("hidden");
        s.mk("root.txt");
        s.mk("sub/inner.txt");
        s.mk(".hidden.txt");
        s.mk(".hiddendir/no.txt");
        let mut found = walk_files(s.path(), &WalkOptions::default());
        found.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["inner.txt", "root.txt"]);
        assert!(found.iter().all(|e| e.path.contains("sub") || e.path.contains("evidx_")));
    }

    #[test]
    fn walk_skips_build_and_dependency_dirs() {
        let s = Scratch::new("skip");
        s.mk("src/main.rs");
        s.mk("src/bin/tool.rs"); // `bin` is a real source directory
        s.mk("node_modules/pkg/index.js");
        s.mk("target/debug/out.txt");
        s.mk("TARGET/x.txt"); // case-insensitive
        s.mk("__pycache__/a.pyc");
        s.mk("vendor/lib/thing.c");
        let mut found = walk_files(s.path(), &WalkOptions::default());
        found.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["main.rs", "tool.rs"]);
    }

    #[test]
    fn walk_stops_at_max_files() {
        let s = Scratch::new("cap");
        for i in 0..10 {
            s.mk(&format!("f{i}.txt"));
        }
        let found = walk_files(
            s.path(),
            &WalkOptions {
                max_files: 3,
                max_depth: 4,
            },
        );
        assert_eq!(found.len(), 3);
    }

    /// Every listed directory leaves a stamp, and an untouched tree revalidates
    /// to "nothing changed" — that is the whole point of the stamps.
    #[test]
    fn walk_stamps_every_directory_and_a_quiet_tree_revalidates_clean() {
        let s = Scratch::new("stamp");
        s.mk("a.txt");
        s.mk("sub/b.txt");
        s.mk("sub/deep/c.txt");
        let r = walk_tree(s.path(), &WalkOptions::default());
        assert_eq!(r.entries.len(), 3);
        assert_eq!(r.dirs.len(), 3, "root + sub + sub/deep");
        assert!(r.dirs.iter().all(|d| d.mtime.is_some()));
        assert!(revalidate(&r.dirs).is_empty());
    }

    /// Adding a file bumps its parent's mtime and nothing else, so revalidate
    /// points at exactly that directory.
    #[test]
    fn revalidate_finds_the_directory_that_changed() {
        let s = Scratch::new("change");
        s.mk("a.txt");
        s.mk("sub/b.txt");
        let r = walk_tree(s.path(), &WalkOptions::default());
        let root_idx = r
            .dirs
            .iter()
            .position(|d| d.path == s.path())
            .expect("the root is stamped");
        // Directory timestamps have coarse granularity on some filesystems;
        // wait so the write cannot land in the same tick.
        std::thread::sleep(std::time::Duration::from_millis(20));
        s.mk("brand_new.txt");
        let changed = revalidate(&r.dirs);
        assert_eq!(changed, vec![root_idx], "only the root's listing moved");
    }

    /// A vanished directory is stale too — its files must leave the index.
    #[test]
    fn revalidate_reports_a_removed_directory() {
        let s = Scratch::new("gone");
        s.mk("keep/a.txt");
        s.mk("doomed/b.txt");
        let r = walk_tree(s.path(), &WalkOptions::default());
        let doomed = r
            .dirs
            .iter()
            .position(|d| d.path.ends_with("doomed"))
            .expect("stamped");
        std::fs::remove_dir_all(s.0.join("doomed")).unwrap();
        assert!(revalidate(&r.dirs).contains(&doomed));
    }

    /// `list_dir` is the repair primitive: one level, skip rules applied, and
    /// it reports the subdirectories so the caller can keep descending.
    #[test]
    fn list_dir_is_one_level_with_the_skip_rules() {
        let s = Scratch::new("list");
        s.mk("here.txt");
        s.mk("sub/inner.txt");
        s.mk("node_modules/x.js");
        s.mk(".hidden");
        let l = list_dir(s.path());
        assert_eq!(l.files.len(), 1);
        assert_eq!(l.files[0].name, "here.txt");
        assert_eq!(l.subdirs.len(), 1, "only `sub`; node_modules is skipped");
        assert!(l.subdirs[0].path.ends_with("sub"));
    }
}
