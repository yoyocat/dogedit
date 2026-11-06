pub mod bookmarks;
pub mod document;
pub mod index;
pub mod linedoc;
pub mod linetips;
pub mod codepage;
pub mod encoding;
pub mod find;
pub mod dirwatch;
pub mod filesearch;
pub mod folding;
pub mod fuzzy;
pub mod fileindex;
pub mod ini;
pub mod loader;
pub mod selection;
pub mod snippet;
pub mod textops;
pub mod macrofile;
pub mod process;
pub mod tools;
pub mod undo;

pub use bookmarks::Bookmarks;
pub use linetips::{LineTip, LineTips, ICON_COUNT, ICON_ERROR, ICON_FLAG, ICON_INFO, ICON_WARNING};
pub use document::{Document, Eol, ScanEvent, SearchEvent, SearchHandle, SearchOptions};
pub use encoding::{detect_encoding, encode_text};
pub use folding::{Folding, FoldingMethod};
pub use ini::Ini;
pub use selection::{CharFlags, FlagBuffer, Range, SelectionSet, Point};
pub use snippet::{
    ExpandedSnippet, Snippet, SnippetFile,
    TabStop, expand,
};
pub use loader::{LoadEvent, ProgressiveLoader, decode_bytes};
pub use tools::{
    CAPTURE_COLUMN, CAPTURE_FILE_PATH, CAPTURE_GROUPS, CAPTURE_LINE,
    CaptureMode, SaveMode, Tool, ToolContext, ToolGroup,
    load_dir, load_file, load_groups, next_uuid, parse_ini,
    parse_sections, save_file, tool_dir,
};
pub use tools::{expand as expand_tool_args, group_path};
pub use undo::{
    Command, DeleteCommand, EditOp, InsertCommand,
    MacroCommand, ReplaceCommand, UndoStack,
};
pub use find::{
    count, find_all, find_from, pick_match, replace_all, replace_all_within,
    FindOptions, Match,
};
pub use fuzzy::{fuzzy_match, FuzzyMatch, FuzzyMode};
pub use filesearch::{
    results_document, results_target, results_title, run_search, run_search_with, FileMatch,
    GlobSet, Progress, SearchBackend, SearchOutcome, SearchQuery, SearchStats,
};
pub use dirwatch::{DirEntry, DirList, DirTree, DirNode, FileWatcher, PollingWatcher, TreeEntry, WatchEvent, WatchKind};
