//! Directory tree model and file
//! watching for the left file panel.
//!
//! `DirList` is a flat snapshot of one
//! directory, sorted folders-first: directories
//! before files. `DirTree` keeps the whole
//! tree in a flat row list so the view
//! can paint and hit-test rows without
//! recursion; `toggle` splices a
//! directory's children in or out.
//! `PollingWatcher` detects changes by
//! comparing mtimes, which works on
//! every platform without OS
//! notification APIs.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

/// One entry of a directory listing.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Flat snapshot of one directory.
#[derive(Debug, Clone)]
pub struct DirList {
    pub path: String,
    pub entries: Vec<DirEntry>,
}

impl DirList {
    /// Reads `path`, sorting directories
    /// before files and each group
    /// case-insensitively by name.
    pub fn load(path: &str) -> std::io::Result<Self> {
        let mut entries = Vec::new();
        for ent in fs::read_dir(path)? {
            let ent = ent?;
            let name = ent.file_name().to_string_lossy().into_owned();
            let is_dir = ent.file_type()?.is_dir();
            let size = if is_dir {
                0
            } else {
                ent.metadata().map(|m| m.len()).unwrap_or(0)
            };
            let child = Path::new(path).join(&name);
            entries.push(DirEntry {
                name,
                path: child.to_string_lossy().into_owned(),
                is_dir,
                size,
            });
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(DirList {
            path: path.to_string(),
            entries,
        })
    }
}

/// Recursive tree node, used to build
/// a `DirTree` from a real directory.
/// `children` is `None` when a
/// directory has not been loaded yet.
#[derive(Debug, Clone)]
pub struct DirNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<DirNode>>,
}

impl DirNode {
    /// Builds a node for `path` with one
    /// level of children loaded.
    pub fn load(path: &str) -> std::io::Result<Self> {
        let list = DirList::load(path)?;
        let name = Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        Ok(DirNode {
            name,
            path: path.to_string(),
            is_dir: true,
            expanded: false,
            children: Some(
                list.entries
                    .into_iter()
                    .map(|e| DirNode {
                        name: e.name,
                        path: e.path,
                        is_dir: e.is_dir,
                        expanded: false,
                        children: if e.is_dir {
                            Some(Vec::new())
                        } else {
                            None
                        },
                    })
                    .collect(),
            ),
        })
    }
}

/// One visible row of a `DirTree`.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub has_children: bool,
    pub depth: usize,
}

/// Flat directory tree. Rows are kept
/// in display order; expanding a
/// directory splices its children in
/// right after it, collapsing removes
/// the whole subtree span.
#[derive(Debug)]
pub struct DirTree {
    entries: Vec<TreeEntry>,
}

impl DirTree {
    /// Builds a tree rooted at `root`,
    /// expanded one level deep.
    pub fn new(root: &str) -> std::io::Result<Self> {
        fs::metadata(root)?;
        let mut tree = Self::from_roots(&[root.to_string()]);
        tree.expand_roots_one_level();
        Ok(tree)
    }

    /// Builds a tree with one top-level row per root, none of them expanded.
    /// That is what 快捷目录 is: a list of folders the user put there, not a
    /// single root. Roots that are gone are skipped, so a stale list still opens
    /// with the rows it can show.
    ///
    /// Expansion is left to the caller on purpose: adding a row must not fold or
    /// unfold the folders already on the list, so who opens what cannot be
    /// decided here.
    pub fn from_roots(roots: &[String]) -> Self {
        let mut tree = DirTree {
            entries: Vec::new(),
        };
        for root in roots {
            let Ok(meta) = fs::metadata(root) else {
                continue;
            };
            let is_dir = meta.is_dir();
            let name = Path::new(root)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string());
            tree.entries.push(TreeEntry {
                name,
                path: root.to_string(),
                is_dir,
                expanded: false,
                has_children: is_dir && dir_has_children(root),
                depth: 0,
            });
        }
        tree
    }

    /// Opens every top-level root one level deep — the shape the panel starts in.
    pub fn expand_roots_one_level(&mut self) {
        let mut i = 0usize;
        while i < self.entries.len() {
            if self.entries[i].depth == 0 && self.entries[i].is_dir && !self.entries[i].expanded {
                self.expand_row(i);
            }
            // 展开把子行插在这一行后面：跳过它们，免得把子目录也当成根再开一层。
            i += 1;
            while i < self.entries.len() && self.entries[i].depth > 0 {
                i += 1;
            }
        }
    }

    /// Every directory the tree currently has open — what a rebuild has to put
    /// back, so refreshing the list does not fold what the user unfolded.
    pub fn expanded_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.is_dir && e.expanded)
            .map(|e| e.path.clone())
            .collect()
    }

    /// Re-opens the directories named in `paths`. A path that is no longer in the
    /// tree, or that has nothing under it, is skipped; one already open is left
    /// alone.
    pub fn restore_expanded(&mut self, paths: &[String]) {
        let mut want: Vec<&str> = paths.iter().map(String::as_str).collect();
        while !want.is_empty() {
            let hit = self.entries.iter().enumerate().find_map(|(i, e)| {
                if !e.is_dir || e.expanded || !e.has_children {
                    return None;
                }
                let k = want.iter().position(|p| *p == e.path.as_str())?;
                Some((i, k))
            });
            // 每一轮都摘掉一个路径，摘完就停：展开会把子行插进来，行号得重扫。
            let Some((row, k)) = hit else { break };
            want.swap_remove(k);
            self.expand_row(row);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All visible rows, in display
    /// order. Row index == draw order.
    pub fn rows(&self) -> &[TreeEntry] {
        &self.entries
    }

    pub fn path_at(&self, row: usize) -> Option<&str> {
        self.entries.get(row).map(|e| e.path.as_str())
    }

    /// Toggles the directory at `row`.
    /// Returns the new expanded state,
    /// or `false` when the row is not a
    /// directory.
    pub fn toggle(&mut self, row: usize) -> bool {
        if row >= self.entries.len() || !self.entries[row].is_dir {
            return false;
        }
        if self.entries[row].expanded {
            self.collapse_row(row);
            false
        } else {
            self.expand_row(row);
            true
        }
    }

    fn expand_row(&mut self, row: usize) {
        let depth = self.entries[row].depth;
        let path = self.entries[row].path.clone();
        let children = match DirList::load(&path) {
            Ok(list) => list.entries,
            Err(_) => Vec::new(),
        };
        let inserted = children
            .into_iter()
            .map(|e| TreeEntry {
                has_children: e.is_dir && dir_has_children(&e.path),
                expanded: false,
                depth: depth + 1,
                name: e.name,
                path: e.path,
                is_dir: e.is_dir,
            })
            .collect::<Vec<_>>();
        let at = row + 1;
        self.entries.splice(at..at, inserted);
        self.entries[row].expanded = true;
    }

    fn collapse_row(&mut self, row: usize) {
        let depth = self.entries[row].depth;
        let mut end = row + 1;
        while end < self.entries.len() && self.entries[end].depth > depth {
            end += 1;
        }
        self.entries.drain(row + 1..end);
        self.entries[row].expanded = false;
    }
}

fn dir_has_children(path: &str) -> bool {
    match fs::read_dir(path) {
        Ok(mut it) => it.next().is_some(),
        Err(_) => false,
    }
}

/// Kind of change seen by a watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchKind {
    Created,
    Modified,
    Deleted,
    /// The file was renamed; the
    /// payload is the old path.
    Renamed(String),
}

/// A single observed change.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub path: String,
    pub kind: WatchKind,
}

/// Watcher interface. `poll` is called
/// periodically and returns every
/// change seen since the last call.
pub trait FileWatcher {
    fn watch(&mut self, path: &str);
    fn unwatch(&mut self, path: &str);
    fn poll(&mut self) -> Vec<WatchEvent>;
}

#[derive(Debug, Clone)]
struct FileState {
    exists: bool,
    mtime: Option<SystemTime>,
    is_dir: bool,
    /// For watched directories: child
    /// name -> mtime seen last time.
    children: Option<HashMap<String, SystemTime>>,
}

/// mtime polling watcher. Works on
/// every platform without OS
/// notification APIs.
pub struct PollingWatcher {
    paths: HashMap<String, FileState>,
}

impl PollingWatcher {
    pub fn new() -> Self {
        Self {
            paths: HashMap::new(),
        }
    }
}

impl Default for PollingWatcher {
    fn default() -> Self {
        Self::new()
    }
}

fn child_mtimes(path: &str) -> HashMap<String, SystemTime> {
    let mut map = HashMap::new();
    if let Ok(rd) = fs::read_dir(path) {
        for ent in rd.flatten() {
            if let Ok(meta) = ent.metadata() {
                if let Ok(mt) = meta.modified() {
                    map.insert(ent.file_name().to_string_lossy().into_owned(), mt);
                }
            }
        }
    }
    map
}

impl FileWatcher for PollingWatcher {
    fn watch(&mut self, path: &str) {
        let (exists, mtime, is_dir, children) = match fs::metadata(path) {
            Ok(meta) => (
                true,
                meta.modified().ok(),
                meta.is_dir(),
                if meta.is_dir() {
                    Some(child_mtimes(path))
                } else {
                    None
                },
            ),
            Err(_) => (false, None, false, None),
        };
        self.paths.insert(
            path.to_string(),
            FileState {
                exists,
                mtime,
                is_dir,
                children,
            },
        );
    }

    fn unwatch(&mut self, path: &str) {
        self.paths.remove(path);
    }

    fn poll(&mut self) -> Vec<WatchEvent> {
        let mut events = Vec::new();
        for (path, state) in &mut self.paths {
            match fs::metadata(path) {
                Ok(meta) => {
                    let mtime = meta.modified().ok();
                    let is_dir = meta.is_dir();
                    if !state.exists {
                        events.push(WatchEvent {
                            path: path.clone(),
                            kind: WatchKind::Created,
                        });
                    } else if state.is_dir != is_dir {
                        events.push(WatchEvent {
                            path: path.clone(),
                            kind: WatchKind::Modified,
                        });
                    } else if !is_dir && state.mtime != mtime {
                        // Only files get a self Modified event. A watched
                        // directory's own mtime is derivative of its child set,
                        // which the diff below already reports; emitting both made
                        // every child add/remove yield 2 events whose count then
                        // depended on directory-mtime clock granularity.
                        events.push(WatchEvent {
                            path: path.clone(),
                            kind: WatchKind::Modified,
                        });
                    }
                    state.exists = true;
                    state.mtime = mtime;
                    state.is_dir = is_dir;
                    if is_dir {
                        let now_children = child_mtimes(path);
                        if let Some(old) = state.children.take() {
                            for (name, mt) in &now_children {
                                match old.get(name) {
                                    None => events.push(WatchEvent {
                                        path: Path::new(path).join(name).to_string_lossy().into_owned(),
                                        kind: WatchKind::Created,
                                    }),
                                    Some(old_mt) if *old_mt != *mt => events.push(WatchEvent {
                                        path: Path::new(path).join(name).to_string_lossy().into_owned(),
                                        kind: WatchKind::Modified,
                                    }),
                                    _ => {}
                                }
                            }
                            for name in old.keys() {
                                if !now_children.contains_key(name) {
                                    events.push(WatchEvent {
                                        path: Path::new(path).join(name).to_string_lossy().into_owned(),
                                        kind: WatchKind::Deleted,
                                    });
                                }
                            }
                        }
                        state.children = Some(now_children);
                    } else {
                        state.children = None;
                    }
                }
                Err(_) => {
                    if state.exists {
                        events.push(WatchEvent {
                            path: path.clone(),
                            kind: WatchKind::Deleted,
                        });
                    }
                    state.exists = false;
                    state.mtime = None;
                    state.children = None;
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::thread::sleep;
    use std::time::Duration;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dg_dirwatch_{}_{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dir_list_sorts_dirs_first() {
        let root = temp_root("sort");
        fs::write(root.join("b.txt"), b"x").unwrap();
        fs::write(root.join("a.txt"), b"x").unwrap();
        fs::create_dir(root.join("Zdir")).unwrap();
        fs::create_dir(root.join("adir")).unwrap();
        let list = DirList::load(root.to_str().unwrap()).unwrap();
        let names: Vec<&str> = list.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["adir", "Zdir", "a.txt", "b.txt"]);
        assert!(list.entries[0].is_dir);
        assert_eq!(list.entries[2].size, 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tree_from_roots_lists_each_root() {
        let a = temp_root("roots_a");
        let b = temp_root("roots_b");
        fs::write(a.join("one.txt"), b"x").unwrap();
        fs::create_dir(b.join("sub")).unwrap();
        let roots = vec![
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
            // a root that is gone is skipped, not fatal
            a.join("missing").to_string_lossy().into_owned(),
        ];
        let mut tree = DirTree::from_roots(&roots);
        // 一个根一行，默认不展开
        assert_eq!(tree.len(), 2);
        tree.expand_roots_one_level();
        // a, one.txt, b, sub
        assert_eq!(tree.len(), 4);
        assert_eq!(tree.rows()[0].depth, 0);
        assert_eq!(tree.rows()[0].path, roots[0]);
        assert_eq!(tree.rows()[1].name, "one.txt");
        assert_eq!(tree.rows()[1].depth, 1);
        assert_eq!(tree.rows()[2].depth, 0);
        assert_eq!(tree.rows()[2].path, roots[1]);
        assert_eq!(tree.rows()[3].name, "sub");
        assert_eq!(tree.rows()[3].depth, 1);
        // collapsing the first root leaves the second one whole
        assert!(!tree.toggle(0));
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.rows()[1].path, roots[1]);
        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    /// A rebuild that puts the open folders back — what keeps a watcher tick, or
    /// a second folder dropped on the panel, from folding what the user opened.
    #[test]
    fn restore_expanded_reopens_only_what_was_open() {
        let a = temp_root("restore_a");
        let b = temp_root("restore_b");
        fs::write(a.join("one.txt"), b"x").unwrap();
        fs::create_dir(b.join("sub")).unwrap();
        let roots = vec![
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        ];
        let mut tree = DirTree::from_roots(&roots);
        tree.expand_roots_one_level();
        assert_eq!(tree.len(), 4, "a, one.txt, b, sub");
        // 两个根都开着
        let open = tree.expanded_paths();
        assert_eq!(open.len(), 2);
        assert!(!tree.toggle(0));
        let open = tree.expanded_paths();
        assert_eq!(open, vec![roots[1].clone()]);

        // 重建后把开着的放回去：第一个根仍然关着，第二个仍然开着
        let mut rebuilt = DirTree::from_roots(&roots);
        rebuilt.restore_expanded(&open);
        assert_eq!(rebuilt.len(), tree.len());
        assert!(!rebuilt.rows()[0].expanded);
        assert!(rebuilt.rows()[1].expanded);
        // 路径已经不在树里的，不算错
        rebuilt.restore_expanded(&[a.join("gone").to_string_lossy().into_owned()]);
        assert_eq!(rebuilt.len(), tree.len());
        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn tree_toggle_splices_rows() {
        let root = temp_root("tree");
        fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        fs::write(root.join("f.txt"), b"x").unwrap();
        let mut tree = DirTree::new(root.to_str().unwrap()).unwrap();
        // root + sub + f.txt
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.rows()[1].name, "sub");
        assert!(tree.rows()[1].has_children);
        // collapse root: only the root row left
        assert!(!tree.toggle(0));
        assert_eq!(tree.len(), 1);
        // expand again
        assert!(tree.toggle(0));
        assert_eq!(tree.len(), 3);
        // expand sub: root, sub, deep, f.txt
        assert!(tree.toggle(1));
        assert_eq!(tree.len(), 4);
        assert_eq!(tree.rows()[2].name, "deep");
        assert_eq!(tree.rows()[2].depth, 2);
        // collapse sub removes only its subtree
        assert!(!tree.toggle(1));
        assert_eq!(tree.len(), 3);
        // paths resolve for painting
        assert!(tree.path_at(1).unwrap().ends_with("sub"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn polling_watcher_reports_file_changes() {
        let root = temp_root("watch");
        let file = root.join("a.txt");
        fs::write(&file, b"one").unwrap();
        let mut watcher = PollingWatcher::new();
        watcher.watch(file.to_str().unwrap());
        // no change since watch() snapshotted state
        assert!(watcher.poll().is_empty());
        // modify
        sleep(Duration::from_millis(20));
        fs::write(&file, b"two").unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, WatchKind::Modified);
        // delete
        fs::remove_file(&file).unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, WatchKind::Deleted);
        // re-create
        fs::write(&file, b"three").unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, WatchKind::Created);
        // unwatch stops events
        watcher.unwatch(file.to_str().unwrap());
        fs::write(&file, b"four").unwrap();
        assert!(watcher.poll().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn polling_watcher_tracks_dir_children() {
        let root = temp_root("watchdir");
        let mut watcher = PollingWatcher::new();
        watcher.watch(root.to_str().unwrap());
        assert!(watcher.poll().is_empty());
        let child = root.join("new.txt");
        fs::write(&child, b"x").unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, WatchKind::Created);
        assert!(evs[0].path.ends_with("new.txt"));
        fs::remove_file(&child).unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, WatchKind::Deleted);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dir_watch_yields_one_event_per_child_change() {
        let root = temp_root("watchdup");
        let mut watcher = PollingWatcher::new();
        watcher.watch(root.to_str().unwrap());
        assert!(watcher.poll().is_empty());

        let child = root.join("a.txt");
        fs::write(&child, b"x").unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1, "adding a child must report the child only");
        assert_eq!(evs[0].kind, WatchKind::Created);

        fs::remove_file(&child).unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1, "removing a child must report the child only");
        assert_eq!(evs[0].kind, WatchKind::Deleted);

        fs::write(&child, b"one").unwrap();
        assert_eq!(watcher.poll().len(), 1);
        sleep(Duration::from_millis(20));
        fs::write(&child, b"two").unwrap();
        let evs = watcher.poll();
        assert_eq!(evs.len(), 1, "a child content change is a single Modified");
        assert_eq!(evs[0].kind, WatchKind::Modified);
        assert_eq!(evs[0].path, root.join("a.txt").to_string_lossy());
        fs::remove_dir_all(&root).unwrap();
    }
}
