//! key.ini shortcut parsing. Users edit key.ini to rebind; the map is
//! consulted before the hardcoded defaults in the window proc.

use std::collections::HashMap;

use crate::app::MoveDir;
use dg_render::ViewFlags;

/// Modifier bits packed into the
/// key hash: Ctrl, Shift, Alt.
pub const MOD_CTRL: u8 = 1;
pub const MOD_SHIFT: u8 = 2;
pub const MOD_ALT: u8 = 4;

/// Pack (mods, vk) into one hash.
pub fn pack(mods: u8, vk: u32) -> u64 {
    ((mods as u64) << 32) | (vk as u64)
}

/// A command from the key map.
/// `Ignore` marks names this implementation
/// has no equivalent for yet — the
/// binding is kept (so it does not
/// fall through to a hardcoded
/// default) but performs nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Save,
    SaveAs,
    Open,
    New,
    /// 新建二进制 (cm_hex_new, 1233): a fresh document shown in the hex view.
    NewHex,
    /// 其它文件类型 (cm_file_other_templates, 1238): a fresh document, then
    /// the 切换文件类型 dialog to pick its syntax.
    NewFromType,
    /// 新建实例 (cm_new_instance, 2012): start another copy of this program
    /// without handing over a file; the current window keeps its tabs.
    NewInstance,
    Exit,
    Undo,
    Redo,
    Copy,
    CopyAppend,
    Cut,
    Paste,
    PasteColumn,
    /// 从历史粘贴, "Paste from History": the clipboard history list opens
    /// at the caret and the chosen entry is pasted.
    PasteHistory,
    /// 粘贴到行尾, "Paste Clipboard at end of line".
    PasteAtLineTail,
    /// 粘贴并缩进, "Indent all lines of clipboard applied with the first line's
    /// indentation".
    PasteAndIndent,
    /// 粘贴带\0, "Insert Clipboard contents which include \0".
    PasteWithNul,
    SelectAll,
    SelectWord,
    Find,
    FindNext,
    FindPrev,
    Replace,
    /// The modeless find bar (`cm_find_mini`, 1470).
    FindMini,
    /// Select every match of the query (`cm_find_all`, 1403).
    FindAll,
    /// 查找下一个单词 (`cm_find_nextword`, 1406): the
    /// word under the caret becomes the query and the
    /// next match is selected.
    FindNextWord,
    /// 查找上一个单词 (`cm_find_prevword`, 1407).
    FindPrevWord,
    /// 激活选区锚点 (`cm_select_anchor`, 1487): arms
    /// the caret as a selection anchor.
    SelectAnchor,
    /// 字符统计 (`cm_doc_statistics`, 1667).
    Statistics,
    /// 显示颜文字 (`cm_emoji_show`, 1334): toggles whether emoji in the
    /// document draw as colour glyphs.
    ShowEmoji,
    /// 加大字号 / 减小字号 (`cm_view_zoominfont`, `cm_view_zoomoutfont`):
    /// one point, in the named direction, on the current document only.
    ZoomFont(bool),
    /// 原始字号 (`cm_view_original_fontsize`, 1469): "使用字体的原始大小" — drop
    /// the document's own size and go back to the configured one.
    FontOriginal,
    /// 设置行高 (`cm_view_linespacing`, 1323): the prompt asks for a multiple
    /// of the standard row pitch and the answer becomes the current
    /// document's extra line spacing.
    ViewLineSpacing,
    GotoLine,
    /// 跳转到文件 (cm_goto_anything, 1760, Ctrl+P): the modeless 跳转
    /// dialog over the in-memory file index.
    GotoAnything,
    /// 跳转到文件符号 (`cm_move_goto_tags`, Ctrl+F12): the same 跳转 dialog with
    /// its query already holding the symbol prefix, so the rows are the
    /// document's symbols rather than its files.
    GotoTags,
    /// 外部浏览器预览 (`cm_view_webpreview2`, Ctrl+Shift+B): hand the document's
    /// address to the browser named in `[Browser]`, or to the system default.
    WebPreviewExternal,
    /// 打印 (`cm_file_print`, 57607): the print dialog, then the spooler job
    /// for the whole document.
    Print,
    /// 打印预览 (`cm_file_printpreview`, 57609): the preview frame the document
    /// pages into, the same layout the print job draws.
    PrintPreview,
    /// One caret move. The pair is one command with one argument here too.
    Caret(MoveDir, bool),
    /// 上移一行/下移一行. One arm here covers both.
    MoveLine(bool),
    /// Shift+Alt+Up/Down, `cm_select_toprevline` (55) and
    /// `cm_select_tonextline` (56): grow the selection by one whole line.
    ExtendLineSelection(bool),
    /// `cm_move_brace`, Ctrl+].
    GotoMatchingBracket,
    /// `cm_select_topair` ( Ctrl+Shift+]) and `cm_select_topair2` (
    /// Ctrl+Shift+[). The flag includes the two brackets or leaves them out.
    SelectBracketPair(bool),
    /// `cm_select_line`: the caret line, or every selected line whole.
    SelectLine,
    /// `cm_move_nextpara` / `cm_move_prevpara`. The flag is the direction.
    Paragraph(bool),
    /// `cm_move_jumpoutofpair`, Ctrl+=: forward to one past the next closer.
    JumpOutOfPair,
    /// `cm_move_nextcaretpos` ( Ctrl+Shift+\) and `cm_move_prevcaretpos` (
    /// Ctrl+\): walk the caret-position ring.
    CaretHistory(bool),
    /// `cm_move_nexteditpos` and `cm_move_preveditpos`: walk the
    /// edit-position ring — the places edits landed, one entry per edited
    /// line, a later edit on that line refreshing the entry.
    EditHistory(bool),
    /// `cm_move_middlecaret`: the caret goes to the row in the middle of the
    /// viewport; the view does not move.
    CenterCaret,
    /// `cm_move_middleline`: the caret's row goes to the middle of the
    /// viewport; the caret does not move.
    CenterLine,
    /// `cm_select_restore`: swap back the selection the last collapse
    /// discarded.
    RestoreSelection,
    MoveScrollUp,
    MoveScrollDown,
    DeletePrev,
    DeleteNext,
    DeletePrevWord,
    DeleteNextWord,
    DeleteLine,
    InsertNewlineAfter,
    InsertNewlineBefore,
    Transpose,
    ToLower,
    ToUpper,
    /// 单词首字母大写.
    CapitalizeWords,
    /// 反转大小写.
    InvertCase,
    /// 删除行头空白.
    TrimLeadingSpace,
    /// 删除行尾空白.
    TrimTailingSpace,
    /// 制表符转为空格.
    TabsToSpaces,
    /// 空格转为制表符.
    SpacesToTabs,
    /// 行头制表符转为空格.
    LeadingTabsToSpaces,
    /// 行头空格转为制表符.
    LeadingSpacesToTabs,
    /// 删除空白行.
    DeleteBlankLines,
    /// 删除重复行( 从上往下).
    DeleteDupLinesTop,
    /// 删除重复行( 从下往上).
    DeleteDupLinesBottom,
    /// 删除到行头.
    DeleteToHeadOfLine,
    /// 删除到行尾.
    DeleteToEndOfLine,
    /// 转为全角.
    ToFullWidth,
    /// 转为半角.
    ToHalfWidth,
    /// 日期.
    InsertDate,
    /// 时间.
    InsertTime,
    /// 自定义日期/时间: the dialog that writes the two `[DateTime]` values
    /// 1417 / 1418 read.
    CustomDateTime,
    /// 使用标记N (ids 1461..1468): put the selection text into slot N-1 of the
    /// process-wide marker set.
    MarkUse(usize),
    /// 开关自定义标记: flip the whole set on or off.
    MarkToggle,
    /// 自定义标记( &C): the manager dialog.
    MarkDialog,
    DuplicateLine,
    /// 复制行 ( the copy-lines command): the touched lines onto the clipboard, caret
    /// and selection left where they were.
    CopyLine,
    /// 复制标记行 ( the copy-marked-lines command).
    CopyMarkedLines,
    /// 剪切标记行 ( the cut-marked-lines command): the copy of 89, then the lines go.
    CutMarkedLines,
    /// 重复选区 ( the duplicate-selection command): each selection is re-inserted after itself
    /// and every caret moves onto its copy.
    DuplicateSelection,
    /// 格式化缩进 ( the indent-format command): pad a ragged indentation up to the next
    /// tab stop. Nothing is removed.
    FormatIndent,
    /// 向左对齐 ( the align-left command mode 1).
    AlignLeft,
    /// 向右对齐 ( mode 2).
    AlignRight,
    /// 居中对齐 ( mode 3).
    AlignCenter,
    /// 升序排序 ( the sort command).
    SortAsc,
    /// 高级排序 — the sort that asks which comparison to use and whether the answer
    /// goes to a new document (cm_tool_sort, 1652, Ctrl+F9).
    SortAdvanced,
    /// 在线帮助 — opens the documentation that ships with the program
    /// (cm_misc_apphelp, 2010, F1).
    ShowHelp,
    /// 降序排序.
    SortDesc,
    /// 转Base64编码 ( the Base64 command).
    Base64Encode,
    /// 转Base64解码.
    Base64Decode,
    JoinLines,
    IncreaseIndent,
    DecreaseIndent,
    LineComment,
    /// 删除行注释符, the inverse of `cm_edit_linecomment`.
    LineUncomment,
    BlockComment,
    /// 删除块注释符, the inverse of `cm_edit_blockcomment`.
    BlockUncomment,
    BookmarkToggle,
    BookmarkNext,
    BookmarkPrev,
    /// 清除所有书签 (编辑器弹出菜单的 `cm_marker_deleteall`, Ctrl+Shift+F2):
    /// the third row of the bookmark group.
    BookmarkClearAll,
    /// 下一个提示 (`cm_tooltip_next`, 1280): the caret
    /// jumps to the next 行注记 line, wrapping.
    LineTipNext,
    /// 上一个提示 (`cm_tooltip_prev`, 1281).
    LineTipPrev,
    /// 清除所有提示 (`cm_tooltip_clearall`, 1282): every
    /// 行注记 goes, and the icon strip hides with them.
    LineTipClearAll,
    FoldToggle,
    /// 折叠当前级别.
    FoldCurrent,
    /// 展开当前级别.
    UnfoldCurrent,
    /// Cancel Folding: folding method 0, no regions.
    FoldCancel,
    /// &Syntax: folding method 1.
    FoldBySyntax,
    /// &Indent: folding method 2.
    FoldByIndent,
    /// Fold Level N ( –1629, `cm_folding_levelN`).
    FoldToLevel(usize),
    FoldFoldAll,
    FoldExpandAll,
    ColumnLeft,
    /// 列块选择: latches the rectangle selection that `cm_select_col*`
    /// produces transiently.
    ColumnMode,
    /// HEX模式 ( `cm_edit_hexmode`): the active document's view toggle.
    /// Choosing it shows the document in the HexView32 window and
    /// choosing it again hands the surface back to the text editor — the
    /// menu row carries the check while the document shows hex.
    HexMode,
    ColumnRight,
    ColumnUp,
    ColumnDown,
    SplitSelectionIntoLines,
    AddAllSelections,
    QuickSelect,
    /// 覆写\编辑模式 ( `cm_edit_ovrins`): insert ⇄ overwrite. The same toggle the
    /// edit-mode pane shows; the menu row and the Insert key both reach it here.
    ToggleOverwrite,
    /// 部分编辑 (`cm_edit_partially`, Alt+]): the selection becomes the
    /// only span the buffer takes edits in, until the same chord lifts it.
    PartialEdit,
    Ignore,
    /// 设置 (cm_config_options, 1336): the 选项 dialog, which opens on the
    /// General page.
    Options,
    /// 设置字体 (cm_config_setfont, 1321): the 设置字体 dialog, reached from
    /// 工具 → 设置 → 设置字体. It edits the
    /// `[Font0]`..`[Font9]` slots, so the name has to resolve for `key.ini`
    /// as well as for the menu row's enabled state.
    ConfigFont,
    /// Toggle the left dock's directory tree (cm_dock_dirview, 1292). The 目录树
    /// row (cm_dock_filemanager, 1285) reaches the same pane: one tree window
    /// serves both captions, so both names resolve to this command.
    DockDirView,
    /// Show or hide the 文件列表 pane — the open documents, in the left dock
    /// (cm_dock_openfiles, 1291).
    DockOpenFiles,
    /// Show or hide the 大纲视图 pane — the symbols of the document on screen, in
    /// the left dock (cm_dock_outline, 1312).
    DockOutline,
    /// Show or hide the 代码片段 pane — the snippets the document's language ships,
    /// in the left dock (cm_dock_snippet, 1329).
    DockSnippet,
    /// Show or hide the 书签管理器 pane — the bookmarks of every open document, in the
    /// left dock (cm_dock_bookmarkmanager, 1289).
    DockBookmark,
    /// Show or hide the 输出窗口 dock (cm_dock_output, 1325; `zh-CN.lng`, 1325
    /// "显示或隐藏输出窗口"). The menu bar and toolbar row already carry it.
    DockOutput,
    /// Find in files (cm_find_infiles).
    FindInFiles,
    /// 命令表到此为止），id 落在 36000+ 的本实现的扩展段：
    /// 导出选区（36001）：把选区覆盖的行区间按原编码流式写成新文件。
    LargeExportSel,
    /// 转为普通模式（36002）：守卫通过后把大文件物化为普通内存文档。
    LargeToNormal,
    /// 替换结果导出（36003）：查找/替换对流式应用到新文件（只读期的
    /// 替换逃生门）。
    LargeReplaceExport,
    /// 尾随日志（36004，M6）：tail -f —— 文件追加即增量续扫上屏。
    LargeTail,
    /// Save every dirty document (cm_file_saveall).
    SaveAll,
    /// Re-read the document from disk, discarding the buffer (cm_file_reload).
    ReloadDoc,
    /// 复制文件名 (cm_doc_copyfilename): the tab's text on the clipboard.
    CopyFileName,
    /// 复制路径 (cm_doc_copypath): the full path on the clipboard.
    CopyFilePath,
    /// 打开所在文夹 (cm_doc_openfilefolder): Explorer, with the file selected.
    OpenInFolder,
    /// 重命名当前文档 (cm_doc_rename): a name asked through the prompt box, then
    /// the file renamed under it.
    RenameDoc,
    /// 禁止编辑 (cm_doc_donotallowedit): the per-document read-only latch.
    ForbidEdit,
    /// 清空剪贴板 (cm_edit_eraseclipboard): the clipboard and its history.
    EraseClipboard,
    /// 总在最前 (cm_window_topmost).
    Topmost,
    /// 选区求和 (cm_tool_sumsel): the numbers inside the selection, added up.
    SumSelection,
    /// EOL mode commands. They are menu-only and never key-bindable.
    EolPc,
    EolUnix,
    EolMac,
    /// Convert the document encoding
    /// (cm_doc_convertencoding).
    ConvertEncoding,
    /// Re-read the active file with a chosen encoding
    /// (cm_file_reloadwithencoding).
    ReloadWithEncoding,
    /// Word Wrap modes (the cm_wrap_* family:
    /// cancel 1294, smart 1297, winedge 1296,
    /// fixedcol 1295, fixedcolwithtab 1293,
    /// pixelwidth 1283).
    WrapCancel,
    WrapSmart,
    WrapWindow,
    WrapFixedColumn,
    WrapFixedColumnTabs,
    WrapFixedPixel,
    /// Toggle auto-pair (cm_doc_autopair).
    AutoPairToggle,
    /// Toggle auto-indent (cm_doc_autoindent).
    AutoIndentToggle,
    /// Toggle word completion (cm_doc_autocomplete).
    WordCompleteToggle,
    /// Show the word completion list (cm_doc_showcomplete).
    ShowCompleteList,
    /// Close the active tab (cm_file_close, the first row of the tab
    /// strip's right-click menu).
    CloseTab,
    /// Close every tab but the active one
    /// (cm_file_closeothers).
    CloseOtherTabs,
    /// Close the tabs left of the active one
    /// (cm_file_closeleft).
    CloseTabsLeft,
    /// Close the tabs right of the active one
    /// (cm_file_closeright).
    CloseTabsRight,
    /// Close every tab (cm_file_closeall).
    CloseAllTabs,
    /// Select the next tab (cm_window_next, Ctrl+Tab).
    NextTab,
    /// Select the previous tab (cm_window_prev,
    /// Ctrl+Shift+Tab).
    PrevTab,
    /// 下一个窗口(前后顺序) (cm_window_next_taborder, 1212): the same step, but taken
    /// along the tab strip instead of the usage order. The plain
    /// `cm_window_next`/`prev` rows cannot be the positional ones.
    TabOrderNext,
    /// 上一个窗口(前后顺序) (cm_window_prev_taborder, 1213).
    TabOrderPrev,
    /// 最近的窗口 (cm_window_last, 1221, F12): put the tab used just before this
    /// one on screen — the one-keystroke jump, as opposed to stepping.
    LastWindow,
    /// 显示/隐藏所有面板 (cm_window_toggleallpanels, 1222, F7): one command over
    /// every docked panel, not one per panel.
    ToggleAllPanels,
    /// 切换焦点 (cm_window_switchfocus, 1223, Alt+\\): hand the keyboard to the
    /// other window of the frame — the editor, or the panel that is showing.
    SwitchFocus,
    /// 垂直分割 (cm_window_vertsplit, 1204): cut the editor area left|right.
    /// Already vertical → 撤销分割; already horizontal → switch the cut.
    SplitVertical,
    /// 水平分割 (cm_window_horzsplit, 1205): the same cut, top / bottom.
    SplitHorizontal,
    /// 克隆视图 (cm_window_clone, 1207): make sure there is a split and put
    /// the active document into the other pane as well.
    CloneView,
    /// 同步滚动 (cm_window_syncscroll, 1208): scrolling one pane scrolls both.
    SyncScrollToggle,
    /// 移动到新垂直分组 (cm_window_tovertgroup, 1209): the active document
    /// moves to the other pane — an existing cut in the other direction is
    /// just turned vertical, a matching cut moves the tab across.
    MoveToVertGroup,
    /// 移动到新水平分组 (cm_window_tohorzgroup, 1211): the same, horizontally.
    MoveToHorzGroup,
    /// 合并分组 (cm_window_mergegroup, 1210): drop the split; the active
    /// pane's document and position stay on screen.
    MergeGroups,
    /// 恢复最后关闭的文件 (cm_file_open_recently_closed, 1240, Ctrl+Shift+T):
    /// reopen the document whose tab was closed last.
    OpenRecentlyClosed,
    /// 清除历史记录 (cm_file_clearhis, 1236): confirm, then drop the
    /// `[RecentDoc]` section from history.ini and empty the 最近的文件 list
    FileClearHistory,
    /// 保存到新会话 (cm_file_save_newsession, 1241): a `*.session` save dialog
    /// whose default name is the 无标题 caption, then the
    /// open documents' paths go into the chosen file and the name joins the
    /// 会话 menu (the session menu).
    FileSaveNewSession,
    /// 清除会话菜单 (1242 — this id dispatches by an immediate
    /// comparison in the frame's WM_COMMAND switch, with no `cm_*` row in the
    /// command table): confirm, then empty the 会话 list and the current
    /// session name (the session name reader).
    FileClearSessionMenu,
    /// 保存到当前会话 (cm_file_save_currentsession, 1243): rewrite the open
    /// documents' paths into the current session's file; a no-op while no
    /// session is current (the session gate gates on the name being non-empty).
    FileSaveCurrentSession,
    /// One of the `cm_view_*` options
    /// control stores it in (the view-option switch): read, flip, write back, and
    /// persist as `[Edit]/<Key>` = `Y`/`N`. The whole family — 20 menu ids —
    /// is one command with one argument, which is why there is a single arm for
    /// it in the dispatcher.
    ViewToggle(u32),
    /// 阅览模式 (`cm_doc_browsermode`, Ctrl+K then R): full-screen, panels away,
    /// and the document refuses edits. The same keystroke puts it back.
    ReadBrowserMode,
    /// 写作模式 (`cm_doc_focusmode`, Ctrl+K then F): the same collapse without
    /// the read-only latch, so the text stays editable.
    ReadFocusMode,
    /// 全屏显示 ( F11, `cm_view_fullscreen`): the frame toggles between
    /// windowed and full-screen ( the full-screen toggle).
    ViewFullscreen,
    /// 工具条 ( cm_view_toolbar): the main toolbar's visibility. The bar
    /// is a native control, so the toggle shows/hides the window and
    /// gives the band back to the dock partition.
    ViewToolbar,
    /// 状态栏 ( cm_view_statusbar): the status strip's visibility, same
    /// contract as [Command: : ViewToolbar].
    ViewStatusbar,
    /// 标签栏 ( cm_view_tabbar): the document tab strip's visibility (
    /// `[Tab] TabBar`, Y default). Same contract as [Command: :
    /// ViewToolbar] — the strip is a window and the dock takes its
    /// row back.
    ViewTabbar,
    /// 模式条 ( cm_view_modebar): the mode bar the implementation has no
    /// window for yet — the toggle persists [Setting] ModeToolBar
    /// only ( data seam).
    ViewModeBar,
    /// 隐藏主菜单: the frame's menu bar comes off with SetMenu( frame,
    /// NULL); ALT puts the same HMENU back on.
    HideMenu,
    /// 外部工具条 ( cm_view_exttoolbar): unported, persists [Setting]
    /// ExtToolBar only ( data seam).
    ViewExtToolbar,
    /// 查看 → 深色主题 (1711): 深 / 浅两副面孔的总开关。每一副绑一套配色
    /// (`[Setting] DarkEditTheme` / `LightEditTheme`)，点下去写的是配置、问的是
    /// 重新启动 —— 原生控件的深浅色是进程级的，开机定一次，中途改不动。命令表里
    /// 没有它的名字，只有菜单行号能走到这儿。
    ViewDarkTheme,
    /// 跳转到工程符号 ( `cm_goto_symbol`): the symbols sidebar — LSP
    /// `textDocument/documentSymbol`, falling back to the bundled
    /// ctags.exe. key.ini binds nothing to it, so the 跳转 menu row is
    /// its only default entrance.
    GotoSymbol,
    /// 颜色高亮 (cm_highlight_color, 1271): a 3 px
    /// band under every color literal, in the literal's
    /// own color. Frame-global; persisted as
    /// `Edit/ColorHighlight`.
    ColorHighlightToggle,
    /// 选区高亮 (cm_highlight_sel, 1313): single-line
    /// selections painted as solid bands. Frame-global;
    /// persisted as `Edit/SelHighlight`.
    SelHighlightToggle,
    /// 高亮网址和邮件 (cm_highlight_urlemail, 1327):
    /// URLs and email addresses recolored to the theme's
    /// `Url` / `Email` keys. Frame-global; persisted as
    /// `Edit/UrlHighlight`.
    UrlHighlightToggle,
    // It needs no variant of its own. 监视剪贴板 (cm_doc_monitorclipboard,
    // 1651): the frame joins the clipboard viewer chain and pastes
    // whatever is copied or cut. Runtime-only.
    WatchClipboardToggle,
    /// 重新加载着色 (cm_doc_reloadsyntax, 1653): the
    /// syntax highlighter is reset from the current text
    /// and the keyword tables rebuilt.
    ReloadSyntax,
    /// 切换文件类型 (cm_doc_changetype, 1662): the 切换文件类型 dialog lists the
    /// syntaxes and relabels the active document with the picked one.
    ChangeType,
    RescanParsers,
    /// 查看Scope (cm_doc_viewscope, 1660): the syntax
    /// scope under the caret is named on the status strip.
    ViewScope,
    /// 在子行处换行 (cm_doc_insertlfonwrap, 1668): a
    /// hard line break is inserted at every wrap point,
    /// turning the whole document's soft wraps into real
    /// lines.
    InsertLfOnWrap,
    /// 激活事件脚本 (cm_event_enable, 1564): the
    /// event-script engine latch. Persisted as
    /// `Setting/EnableEvent`; the engine is not implemented yet.
    EventScriptToggle,
    /// 重新加载事件脚本 (cm_event_reload, 1565): marks
    /// every event script for reload. The engine is not
    /// reimplemented, so the command beeps and is a data seam.
    EventScriptReload,
    /// 录制/停止 (cm_macro_record, 1570): toggles macro
    /// recording. Recorded edits accumulate into one
    /// composite undo step and a replayable op list.
    MacroRecord,
    /// 回放 (cm_macro_playback, 1571): replays the
    /// macro buffer once; refused while a recording is
    /// in progress.
    MacroPlay,
    /// 保存宏为脚本 (cm_macro_saveas, 1572): writes the
    /// macro buffer to a `.mac` file (UTF-8 with BOM).
    MacroSaveAs,
    /// 重复播放 (cm_macro_repeat, 1573): replays the
    /// macro buffer N times for a counted prompt.
    MacroRepeat,
    /// 刷新脚本菜单 (cm_macro_reload, 1574): rescans
    /// `<exe_dir>\macro\*.mac` and rebuilds the script
    /// rows at the bottom of the 扩展 menu.
    MacroReload,
    /// 读取 (cm_macro_load, 1576): reads a `.mac` file into the macro
    /// buffer, replacing it. The this implementation invents the
    /// name.
    MacroLoad,
    /// 暂停 (cm_macro_pause, 1578): toggles the macro-capture latch — a
    /// paused recording skips capture, edits stay individual undo
    /// steps. The this implementation invents the name.
    MacroPause,
    /// 快捷键 (cm_config_shortcut, 1460): the shortcut manager — the dialog
    /// behind 工具 > 设置 > 快捷键. It edits `key.ini`'s `[Shortcut]` section and
    /// hands the frame a reloaded map.
    ConfigShortcut,
    /// 语法着色 (cm_config_syntax, 1459): the parser-package manager — 工具 >
    /// 设置 > 语法着色. Lists the scanned `parsers\` packages, lets the user
    /// switch individual ones off, and hands the frame the disabled names to
    /// persist as `[Syntax] Disabled`.
    ConfigSyntax,
    /// 解析器下载 (cm_parser_market, 36011): 工具 > 设置 > 解析器下载 —
    /// 在线解析器列表的管理框，浏览/搜索/下载/更新/卸载；「打开没有解析器
    /// 的文件」时的首开提示框也从这里进。
    ParserMarket,
    /// 主题 (cm_config_theme, 1298): opens the current theme's **external**
    /// `themes/<name>.ini`, copying the bundled text there the first time. The
    /// file is a standard `theme.ini` — the point is that "change the scrollbar
    /// colour" is an edit to one line of a file the user already understands,
    /// not a new format.
    ConfigTheme,
    /// 脚本包 (cm_config_package, 36012): the script-package manager — 扩展 >
    /// 脚本包. Lists every package `package::scan_all` can see (bundled,
    /// user, development, loose) with a checkbox per row; unchecking writes
    /// `[Scripts] Disabled` and the package stops being loaded at the next
    /// start. It never touches the package directory — a disabled package is
    /// still scanned, so re-ticking the box brings it straight back.
    ConfigPackage,
    /// 新建脚本 (cm_script_new, 36013): the new-script wizard — 扩展 >
    /// 新建脚本. Three pages: kind (loose `.lua` or a script package) with a
    /// template, name/description, then a summary. 完成 writes the files,
    /// loads the script right away and opens it in the editor.
    NewScript,
    /// A `[Shortcut]` row whose value is a script path instead of a `cm_*` name:
    /// 绑定脚本 writes one. The index is into [`KeyMap::scripts`]; the frame asks
    /// the map for the path when it runs it.
    MacroScript(usize),
    /// 外部工具 (cm_config_tools, 1580): the tool manager dialog — the
    /// row's own caption is `&External Tools...`, and the lng tooltip
    /// is 添加或删除自定义工具, so the row configures rather than runs. The
    /// same id appears twice in the bar (工具 and 设置), both opening it.
    ConfigTools,
    /// cm_tool_tool1..9 (ids 1601-1609): run the Nth configured tool.
    /// The tool-run dispatch subtracts 0x641 from the id to index the
    /// tool list.
    RunTool(usize),
    /// 批量替换 (cm_replace_batch, 1420): the batch-replace dialog — an
    /// ordered list of 查找/替换 pairs run over the current document or
    /// every open one, with import/export, persistence and statistics.
    ReplaceBatch,
    /// 文件比较(外部) (cm_tool_diff_external, 1616): pick two open documents
    /// and hand them to the configured external diff tool (`[Tool] DiffExe`
    /// / `DiffArgs`).
    ToolDiffExternal,
}

/// The navigation family: every `cm_move_*` caret command key.ini binds and its
/// `cm_select_*` twin, as (name, direction, extend). Tabulated rather than
/// written as 28 routing arms because, as the command router's shift-latch remap
/// shows handler — the ids differ only in whether the selection survives the
/// move. Names and ids come from the command table.
const CARET_COMMANDS: &[(&str, MoveDir, bool)] = &[
    // ids 1, 2 — the class-run forward move/the class-run backward move with the char flag clear.
    ("cm_move_left", MoveDir::Left, false),
    ("cm_move_right", MoveDir::Right, false),
    ("cm_move_up", MoveDir::Up, false),
    ("cm_move_down", MoveDir::Down, false),
    // ids 73, 74: the bare Home/End keys, which reach the bounds of the
    // *wrapped* row, not of the logical line.
    ("cm_move_toheadofsubline", MoveDir::SublineHome, false),
    ("cm_move_toendofsubline", MoveDir::SublineEnd, false),
    // ids 5, 6: the logical line, with smart Home.
    ("cm_move_toheadofline", MoveDir::LineHead, false),
    ("cm_move_toendofline", MoveDir::LineEnd, false),
    ("cm_move_toheadoffile", MoveDir::DocHead, false),
    ("cm_move_toendoffile", MoveDir::DocEnd, false),
    ("cm_move_nextword", MoveDir::WordRight, false),
    ("cm_move_prevword", MoveDir::WordLeft, false),
    ("cm_move_prevpage", MoveDir::PageUp, false),
    ("cm_move_nextpage", MoveDir::PageDown, false),
    // The selection twins: ids 13, 14, 15, 16, 75, 76, 17, 18, 19, 20, 23, 24,
    // 21, 22 — bound in key.ini to S+Left, S+Right, S+Up, S+Down, S+Home, S+End,
    // CS+Home, CS+End, CS+Left, CS+Right, S+PageUp, S+PageDown.
    ("cm_select_toleft", MoveDir::Left, true),
    ("cm_select_toright", MoveDir::Right, true),
    ("cm_select_toup", MoveDir::Up, true),
    ("cm_select_todown", MoveDir::Down, true),
    ("cm_select_toheadofsubline", MoveDir::SublineHome, true),
    ("cm_select_toendofsubline", MoveDir::SublineEnd, true),
    ("cm_select_toheadofline", MoveDir::LineHead, true),
    ("cm_select_toendofline", MoveDir::LineEnd, true),
    ("cm_select_toheadoffile", MoveDir::DocHead, true),
    ("cm_select_toendoffile", MoveDir::DocEnd, true),
    ("cm_select_tonextword", MoveDir::WordRight, true),
    ("cm_select_toprevword", MoveDir::WordLeft, true),
    ("cm_select_toprevpage", MoveDir::PageUp, true),
    ("cm_select_tonextpage", MoveDir::PageDown, true),
];

impl Command {
    /// The caret command for a `cm_*` name, if the name belongs to the
    /// navigation family.
    pub(crate) fn caret(name: &str) -> Option<Command> {
        CARET_COMMANDS
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, dir, extend)| Command::Caret(*dir, *extend))
    }

    /// Parses a `cm_*` command name;
    /// None for names outside the
    /// implementation's command surface (docks,
    /// web preview,...).
    pub(crate) fn parse(name: &str) -> Option<Command> {
        if let Some(cmd) = Command::caret(name) {
            return Some(cmd);
        }
        Some(match name {
            "cm_file_save" => Command::Save,
            "cm_file_saveas" => Command::SaveAs,
            "cm_file_open" => Command::Open,
            "cm_file_new" => Command::New,
            "cm_hex_new" => Command::NewHex,
            "cm_file_other_templates" => Command::NewFromType,
            // Tab surface. The three remaining close scopes exist
            // only as TabMenu items (1244/1245/1202), so their names
            // follow the same convention until the command table says
            // otherwise.
            "cm_file_close" => Command::CloseTab,
            "cm_file_closeothers" => Command::CloseOtherTabs,
            "cm_file_closeleft" => Command::CloseTabsLeft,
            "cm_file_closeright" => Command::CloseTabsRight,
            "cm_file_closeall" => Command::CloseAllTabs,
            "cm_window_next" => Command::NextTab,
            "cm_window_prev" => Command::PrevTab,
            "cm_window_next_taborder" => Command::TabOrderNext,
            "cm_window_prev_taborder" => Command::TabOrderPrev,
            "cm_window_last" => Command::LastWindow,
            "cm_window_toggleallpanels" => Command::ToggleAllPanels,
            "cm_window_switchfocus" => Command::SwitchFocus,
            "cm_window_vertsplit" => Command::SplitVertical,
            "cm_window_horzsplit" => Command::SplitHorizontal,
            "cm_window_clone" => Command::CloneView,
            "cm_window_syncscroll" => Command::SyncScrollToggle,
            "cm_window_tovertgroup" => Command::MoveToVertGroup,
            "cm_window_tohorzgroup" => Command::MoveToHorzGroup,
            "cm_window_mergegroup" => Command::MergeGroups,
            "cm_file_open_recently_closed" => Command::OpenRecentlyClosed,
            "cm_file_clearhis" => Command::FileClearHistory,
            "cm_file_save_newsession" => Command::FileSaveNewSession,
            "cm_file_clearsessionmenu" => Command::FileClearSessionMenu,
            "cm_file_save_currentsession" => Command::FileSaveCurrentSession,
            "cm_doc_eol_pc" => Command::EolPc,
            "cm_doc_eol_unix" => Command::EolUnix,
            "cm_doc_eol_mac" => Command::EolMac,
            "cm_doc_convertencoding" => Command::ConvertEncoding,
            "cm_file_reloadwithencoding" => Command::ReloadWithEncoding,
            "cm_wrap_cancel" => Command::WrapCancel,
            "cm_wrap_smart" => Command::WrapSmart,
            "cm_wrap_winedge" => Command::WrapWindow,
            "cm_wrap_fixedcol" => Command::WrapFixedColumn,
            "cm_wrap_fixedcolwithtab" => Command::WrapFixedColumnTabs,
            "cm_wrap_pixelwidth" => Command::WrapFixedPixel,
            "cm_doc_autopair" => Command::AutoPairToggle,
            "cm_doc_autoindent" => Command::AutoIndentToggle,
            "cm_doc_autocomplete" => Command::WordCompleteToggle,
            "cm_doc_showcomplete" => Command::ShowCompleteList,
            "cm_file_exit" => Command::Exit,
            "cm_edit_undo" => Command::Undo,
            "cm_edit_redo" => Command::Redo,
            "cm_edit_copy" => Command::Copy,
            // 原始复制 ( handled inline by the command router): open the clipboard, empty it,
            // write the canonical selection block. That is exactly what this
            // implementation's copy does, so the row is the same command rather than a
            // second implementation.
            "cm_edit_rawcopy" => Command::Copy,
            "cm_edit_copyappend" => Command::CopyAppend,
            // 剪切并追加. Same behaviour, same command here.
            "cm_edit_cutappend" => Command::CopyAppend,
            "cm_edit_copyline" => Command::CopyLine,
            "cm_edit_copymarkedlines" => Command::CopyMarkedLines,
            "cm_edit_cutmarkedlines" => Command::CutMarkedLines,
            "cm_edit_cut" => Command::Cut,
            "cm_edit_paste" => Command::Paste,
            "cm_edit_pastecolumn" => Command::PasteColumn,
            "cm_view_colmode" => Command::ColumnMode,
            // 编辑 → HEX模式: the per-document view toggle, the row that
            // swaps the text editor for the hex window and back.
            "cm_edit_hexmode" => Command::HexMode,
            // The view-option family: the view-option switch is one switch from the
            // menu id to a bit index, so the names below carry that index and the
            // dispatcher has one arm for all of them. They arrive through
            // `ViewFlags::bit_for_menu_id` instead.
            "cm_view_linenumber" => Command::ViewToggle(ViewFlags::LINE_NUMBER),
            "cm_view_ruler" => Command::ViewToggle(ViewFlags::RULER),
            "cm_view_whitespace" => Command::ViewToggle(ViewFlags::WHITESPACE),
            "cm_view_controlchars" => Command::ViewToggle(ViewFlags::CONTROL_CHAR),
            "cm_view_endofline" => Command::ViewToggle(ViewFlags::END_OF_LINE),
            "cm_view_endoffile" => Command::ViewToggle(ViewFlags::END_OF_FILE),
            "cm_view_activeline" => Command::ViewToggle(ViewFlags::ACTIVE_LINE),
            "cm_view_indentline" => Command::ViewToggle(ViewFlags::INDENT_LINE),
            "cm_view_foldingtail" => Command::ViewToggle(ViewFlags::FOLDING_TAIL),
            "cm_view_showallinsel" => Command::ViewToggle(ViewFlags::INVISIBLE_CHAR_IN_SEL),
            "cm_view_fullscreen" => Command::ViewFullscreen,
            "cm_view_toolbar" => Command::ViewToolbar,
            "cm_view_statusbar" => Command::ViewStatusbar,
            "cm_view_tabbar" => Command::ViewTabbar,
            "cm_view_modebar" => Command::ViewModeBar,
            "cm_view_exttoolbar" => Command::ViewExtToolbar,
            "cm_goto_symbol" => Command::GotoSymbol,
            "cm_view_boundaryline" => Command::ViewToggle(ViewFlags::BOUNDARY_LINE),
            "cm_edit_softtab" => Command::ViewToggle(ViewFlags::SOFT_TAB),
            "cm_view_iconarea" => Command::ViewToggle(ViewFlags::ICON_AREA),
            "cm_view_extendactiveline" => Command::ViewToggle(ViewFlags::EXTEND_ACTIVE),
            "cm_wrap_indentsublines" => Command::ViewToggle(ViewFlags::INDENT_WRAP),
            "cm_wrap_restoreaftercolsel" => Command::ViewToggle(ViewFlags::RESTORE_WRAP),
            "cm_edit_pastefromhistory" => Command::PasteHistory,
            "cm_select_all" => Command::SelectAll,
            "cm_select_word" => Command::SelectWord,
            "cm_find" => Command::Find,
            "cm_find_next" => Command::FindNext,
            "cm_find_prev" => Command::FindPrev,
            "cm_replace" => Command::Replace,
            "cm_find_mini" => Command::FindMini,
            "cm_find_all" => Command::FindAll,
            "cm_find_nextword" => Command::FindNextWord,
            "cm_find_prevword" => Command::FindPrevWord,
            "cm_select_anchor" => Command::SelectAnchor,
            "cm_doc_statistics" => Command::Statistics,
            "cm_emoji_show" => Command::ShowEmoji,
            "cm_config_options" => Command::Options,
            "cm_config_setfont" => Command::ConfigFont,
            "cm_view_zoominfont" => Command::ZoomFont(true),
            "cm_view_zoomoutfont" => Command::ZoomFont(false),
            "cm_view_original_fontsize" => Command::FontOriginal,
            "cm_view_defaultfont" => Command::FontOriginal,
            "cm_view_linespacing" => Command::ViewLineSpacing,
            "cm_move_goto" => Command::GotoLine,
            // The rest of the cm_move_* caret family is the table above; only
            // the two view scrolls, which do not move the caret, are here.
            "cm_move_scrollup" => Command::MoveScrollUp,
            "cm_move_scrolldown" => Command::MoveScrollDown,
            // Line moves and the whole-line selection growth, each pair one
            // command with a direction flag.
            "cm_move_lineup" => Command::MoveLine(true),
            "cm_move_linedown" => Command::MoveLine(false),
            "cm_select_toprevline" => Command::ExtendLineSelection(true),
            "cm_select_tonextline" => Command::ExtendLineSelection(false),
            "cm_move_brace" => Command::GotoMatchingBracket,
            "cm_select_topair" => Command::SelectBracketPair(false),
            "cm_select_topair2" => Command::SelectBracketPair(true),
            "cm_select_line" => Command::SelectLine,
            "cm_move_nextpara" => Command::Paragraph(true),
            "cm_move_prevpara" => Command::Paragraph(false),
            "cm_move_jumpoutofpair" => Command::JumpOutOfPair,
            "cm_move_nextcaretpos" => Command::CaretHistory(true),
            "cm_move_prevcaretpos" => Command::CaretHistory(false),
            "cm_move_nexteditpos" => Command::EditHistory(true),
            "cm_move_preveditpos" => Command::EditHistory(false),
            "cm_move_middlecaret" => Command::CenterCaret,
            "cm_move_middleline" => Command::CenterLine,
            "cm_select_restore" => Command::RestoreSelection,
            "cm_delete_prev" => Command::DeletePrev,
            "cm_delete_next" => Command::DeleteNext,
            "cm_delete_prevword" => Command::DeletePrevWord,
            "cm_delete_nextword" => Command::DeleteNextWord,
            "cm_delete_line" => Command::DeleteLine,
            "cm_insert_newlineafter" => Command::InsertNewlineAfter,
            "cm_insert_newlinebefore" => Command::InsertNewlineBefore,
            "cm_edit_transpose" => Command::Transpose,
            "cm_edit_tolower" => Command::ToLower,
            "cm_edit_toupper" => Command::ToUpper,
            // The editor popup's 转换 / 删除更多 rows. The four marked (inferred)
            // below have neither a `key.ini` binding nor a `cm_*` name in the
            // shipped language table, so they follow the group prefix the named
            // rows around them establish.
            "cm_edit_capitalize" => Command::CapitalizeWords,
            "cm_edit_invertcase" => Command::InvertCase,
            "cm_edit_tab2space" => Command::TabsToSpaces,
            "cm_edit_space2tab" => Command::SpacesToTabs,
            "cm_edit_leadingtabs2space" => Command::LeadingTabsToSpaces,
            "cm_edit_leadingspace2tab" => Command::LeadingSpacesToTabs,
            "cm_delete_leadingspace" => Command::TrimLeadingSpace,
            "cm_delete_tailingspace" => Command::TrimTailingSpace,
            "cm_delete_blanklines" => Command::DeleteBlankLines,
            // Only the bottom-keeping variant has a `cm_*` name; 删除重复行(
            // 从上往下) is `Command::DeleteDupLinesTop`, which the language table
            // leaves unnamed and is reached from the menu id table instead.
            "cm_delete_duplicatedlines" => Command::DeleteDupLinesBottom,
            "cm_delete_toheadofline" => Command::DeleteToHeadOfLine,
            "cm_delete_toendofline" => Command::DeleteToEndOfLine,
            "cm_edit_tofullwidth" => Command::ToFullWidth,
            "cm_edit_tohalfwidth" => Command::ToHalfWidth,
            "cm_insert_date" => Command::InsertDate,
            "cm_insert_time" => Command::InsertTime,
            "cm_misc_configdatetime" => Command::CustomDateTime,
            "cm_misc_usermarker" => Command::MarkDialog,
            "cm_misc_usermarker_enable" => Command::MarkToggle,
            "cm_misc_usermarker_1" => Command::MarkUse(0),
            "cm_misc_usermarker_2" => Command::MarkUse(1),
            "cm_misc_usermarker_3" => Command::MarkUse(2),
            "cm_misc_usermarker_4" => Command::MarkUse(3),
            "cm_misc_usermarker_5" => Command::MarkUse(4),
            "cm_misc_usermarker_6" => Command::MarkUse(5),
            "cm_misc_usermarker_7" => Command::MarkUse(6),
            "cm_misc_usermarker_8" => Command::MarkUse(7),
            "cm_edit_duplicateline" => Command::DuplicateLine,
            "cm_edit_duplicateselection" => Command::DuplicateSelection,
            "cm_edit_formatindent" => Command::FormatIndent,
            "cm_edit_alignleft" => Command::AlignLeft,
            "cm_edit_alignright" => Command::AlignRight,
            "cm_edit_aligncenter" => Command::AlignCenter,
            "cm_edit_joinlines" => Command::JoinLines,
            "cm_edit_increaseindent" => Command::IncreaseIndent,
            "cm_edit_decreaseindent" => Command::DecreaseIndent,
            "cm_edit_linecomment" => Command::LineComment,
            "cm_edit_blockcomment" => Command::BlockComment,
            "cm_marker_toggle" => Command::BookmarkToggle,
            "cm_marker_next" => Command::BookmarkNext,
            "cm_marker_prev" => Command::BookmarkPrev,
            "cm_marker_deleteall" => Command::BookmarkClearAll,
            "cm_tooltip_next" => Command::LineTipNext,
            "cm_tooltip_prev" => Command::LineTipPrev,
            "cm_tooltip_clearall" => Command::LineTipClearAll,
            "cm_folding_foldcurlevel" => Command::FoldCurrent,
            "cm_folding_expandcurlevel" => Command::UnfoldCurrent,
            "cm_folding_level2" => Command::FoldToLevel(2),
            "cm_folding_level3" => Command::FoldToLevel(3),
            "cm_folding_level4" => Command::FoldToLevel(4),
            "cm_folding_level5" => Command::FoldToLevel(5),
            "cm_folding_level6" => Command::FoldToLevel(6),
            "cm_folding_level7" => Command::FoldToLevel(7),
            "cm_folding_level8" => Command::FoldToLevel(8),
            "cm_folding_level9" => Command::FoldToLevel(9),
            "cm_folding_foldall" => Command::FoldFoldAll,
            "cm_folding_expandall" => Command::FoldExpandAll,
            "cm_select_colleft" => Command::ColumnLeft,
            "cm_select_colright" => Command::ColumnRight,
            "cm_select_colup" => Command::ColumnUp,
            "cm_select_coldown" => Command::ColumnDown,
            "cm_select_splitintolines" => Command::SplitSelectionIntoLines,
            "cm_select_addall" => Command::AddAllSelections,
            "cm_select_quick" => Command::QuickSelect,
            "cm_edit_ovrins" => Command::ToggleOverwrite,
            "cm_goto_anything" => Command::GotoAnything,
            "cm_move_goto_tags" => Command::GotoTags,
            "cm_doc_browsermode" => Command::ReadBrowserMode,
            "cm_doc_focusmode" => Command::ReadFocusMode,
            "cm_edit_partially" => Command::PartialEdit,
            // without an implementation equivalent
            // yet — keep the binding so
            // it does not fall through
            // to a hardcoded default
            "cm_edit_date" => Command::Ignore,
            "cm_tool_sort" => Command::SortAdvanced,
            "cm_misc_apphelp" => Command::ShowHelp,
            "cm_tool_sortasc" => Command::SortAsc,
            "cm_tool_sortdec" => Command::SortDesc,
            "cm_base64_utf8_encode" => Command::Base64Encode,
            "cm_base64_utf8_decode" => Command::Base64Decode,
            // Names the command table defines but `key.ini` leaves unbound;
            // the menu rows carry them, so `cmdids::command_for_id` has to
            // know the spelling to make a row live.
            "cm_delete_word" => Command::DeleteNextWord,
            "cm_edit_blockuncomment" => Command::BlockUncomment,
            "cm_edit_lineuncomment" => Command::LineUncomment,
            "cm_edit_pasteindent" => Command::PasteAndIndent,
            "cm_edit_pastetail" => Command::PasteAtLineTail,
            "cm_edit_pastewith0" => Command::PasteWithNul,
            "cm_folding_cancel" => Command::FoldCancel,
            "cm_folding_indent" => Command::FoldByIndent,
            "cm_folding_syntax" => Command::FoldBySyntax,
            "cm_dock_dirview" => Command::DockDirView,
            "cm_dock_filemanager" => Command::DockDirView,
            "cm_view_webpreview2" => Command::WebPreviewExternal,
            "cm_file_print" => Command::Print,
            "cm_file_printpreview" => Command::PrintPreview,
            "cm_dock_openfiles" => Command::DockOpenFiles,
            "cm_dock_outline" => Command::DockOutline,
            "cm_dock_snippet" => Command::DockSnippet,
            "cm_dock_bookmarkmanager" => Command::DockBookmark,
            "cm_dock_output" => Command::DockOutput,
            "cm_find_infiles" => Command::FindInFiles,
            // 大文件模式（本实现的扩展段 36000+）。
            "cm_large_export_sel" => Command::LargeExportSel,
            "cm_large_to_normal" => Command::LargeToNormal,
            "cm_large_replace_export" => Command::LargeReplaceExport,
            "cm_large_tail" => Command::LargeTail,
            // The document/clipboard/window rows the menu bar and the editor
            // popup carry but `key.ini` leaves unbound.
            "cm_file_saveall" => Command::SaveAll,
            // 新建实例 (2012)：文件菜单上的行，key.ini 没有给它键位。
            "cm_new_instance" => Command::NewInstance,
            "cm_file_reload" => Command::ReloadDoc,
            "cm_doc_copyfilename" => Command::CopyFileName,
            "cm_doc_copypath" => Command::CopyFilePath,
            "cm_doc_openfilefolder" => Command::OpenInFolder,
            "cm_doc_rename" => Command::RenameDoc,
            "cm_doc_donotallowedit" => Command::ForbidEdit,
            "cm_edit_eraseclipboard" => Command::EraseClipboard,
            "cm_window_topmost" => Command::Topmost,
            "cm_tool_sumsel" => Command::SumSelection,
            "cm_highlight_color" => Command::ColorHighlightToggle,
            "cm_highlight_sel" => Command::SelHighlightToggle,
            "cm_highlight_urlemail" => Command::UrlHighlightToggle,
            "cm_highlight_brace" => {
                Command::ViewToggle(ViewFlags::BRACKET_HIGHLIGHT)
            }
            "cm_doc_monitorclipboard" => Command::WatchClipboardToggle,
            "cm_doc_reloadsyntax" => Command::ReloadSyntax,
            "cm_doc_changetype" => Command::ChangeType,
            "cm_plugin_rescanparsers" => Command::RescanParsers,
            "cm_doc_viewscope" => Command::ViewScope,
            "cm_doc_insertlfonwrap" => Command::InsertLfOnWrap,
            "cm_event_enable" => Command::EventScriptToggle,
            "cm_event_reload" => Command::EventScriptReload,
            "cm_macro_record" => Command::MacroRecord,
            "cm_macro_playback" => Command::MacroPlay,
            "cm_macro_saveas" => Command::MacroSaveAs,
            "cm_macro_repeat" => Command::MacroRepeat,
            "cm_macro_reload" => Command::MacroReload,
            // the invented names keep them remappable like the rest.
            "cm_macro_load" => Command::MacroLoad,
            "cm_macro_pause" => Command::MacroPause,
            // The external-tool family: one name per slot, so key.ini
            // can bind any of the nine rows here
            "cm_config_tools" => Command::ConfigTools,
            "cm_config_shortcut" => Command::ConfigShortcut,
            "cm_config_syntax" => Command::ConfigSyntax,
            "cm_parser_market" => Command::ParserMarket,
            "cm_config_theme" => Command::ConfigTheme,
            "cm_config_package" => Command::ConfigPackage,
            "cm_script_new" => Command::NewScript,
            "cm_replace_batch" => Command::ReplaceBatch,
            "cm_tool_diff_external" => Command::ToolDiffExternal,
            "cm_tool_tool1" => Command::RunTool(0),
            "cm_tool_tool2" => Command::RunTool(1),
            "cm_tool_tool3" => Command::RunTool(2),
            "cm_tool_tool4" => Command::RunTool(3),
            "cm_tool_tool5" => Command::RunTool(4),
            "cm_tool_tool6" => Command::RunTool(5),
            "cm_tool_tool7" => Command::RunTool(6),
            "cm_tool_tool8" => Command::RunTool(7),
            "cm_tool_tool9" => Command::RunTool(8),
            _ => return None,
        })
    }
}

/// One row of the `[Shortcut]` section, kept as the file spells it.
///
/// The 快捷键 manager edits these rows rather than a re-serialized table, so a
/// name this implementation cannot resolve, a script path and the file's own order all
/// survive a save. [`KeyMap::rows`] is what the manager's 当前快捷键 list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRow {
    /// The combo as written: `C+S`, `CS+F`, or a chord `C+K,R`.
    pub combo: String,
    /// The right-hand side: a `cm_*` name, or a script path.
    pub value: String,
}

/// What one combo runs. A script is held as an index into [`KeyMap::scripts`]
/// rather than a path so [`Command`] stays `Copy` — it rides through
/// `run_command` by value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Binding {
    Cmd(Command),
    Script(usize),
}

impl Binding {
    fn command(self) -> Command {
        match self {
            Binding::Cmd(cmd) => cmd,
            // The frame asks the map for the path; the index is only the key.
            Binding::Script(i) => Command::MacroScript(i),
        }
    }
}

/// The parsed shortcut table. A chord's first stage is latched by the key
/// dispatcher and the second one resolves through `chords`.
#[derive(Clone, Default)]
pub struct KeyMap {
    rows: Vec<KeyRow>,
    index: HashMap<u64, Binding>,
    chords: HashMap<u64, HashMap<u64, Binding>>,
    /// Every distinct script path a row names, in file order. `Binding::Script`
    /// indexes this.
    scripts: Vec<String>,
    /// The file [`KeyMap::save`] writes: the one that was read, or the one a
    /// first save creates.
    path: Option<std::path::PathBuf>,
}

/// The shipped key.ini, the
/// default when no user copy is
/// found next to the executable
/// or in the working directory.
const DEFAULT_KEY_INI: &str = include_str!("../key.ini");

/// Whether a `[Shortcut]` value names a script rather than a command. Anything that
/// cannot be a `cm_*` name but looks like a file is one. The test matters:
/// `C+K,C=cm_dock_command` is a dead `cm_*` name in this repository's `key.ini`,
/// not a script, and must stay unresolved instead of becoming one.
fn is_script_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.ends_with(".mac")
        || lower.ends_with(".macro")
        || value.contains('\\')
        || value.contains('/')
}

/// Splits a combo into its stages' hashes: one for a plain combo, two for a
/// chord.
fn parse_stages(combo: &str) -> Option<Vec<u64>> {
    let mut out = Vec::new();
    for stage in combo.split(',') {
        let (mods, vk) = parse_combo(stage.trim())?;
        out.push(pack(mods, vk));
    }
    (1..=2).contains(&out.len()).then_some(out)
}

impl KeyMap {
    /// Parses a key.ini `[Shortcut]` section. Chords (`C+K,R=...`) and script
    /// bindings are indexed alongside plain rows; a row this implementation cannot resolve
    /// is still kept in [`KeyMap::rows`], it just binds nothing.
    ///
    /// 行的切分、段名大小写、注释都是 [`dg_core::ini`] 的语义；这里只认
    /// `[Shortcut]` 段，把键位行原样收进 [`KeyMap::rows`]。
    pub fn parse(text: &str) -> Self {
        let rows = dg_core::ini::Ini::parse(text)
            .rows_in("Shortcut")
            .into_iter()
            .map(|(combo, value)| KeyRow {
                combo: combo.trim().to_string(),
                value: value.trim().to_string(),
            })
            .collect();
        let mut map = Self {
            rows,
            ..Self::default()
        };
        map.reindex();
        map
    }

    /// Rebuilds both indexes from `rows`. Every mutation goes through here, so
    /// the rows stay the single source of truth.
    fn reindex(&mut self) {
        self.index.clear();
        self.chords.clear();
        self.scripts.clear();
        for row in &self.rows {
            if is_script_value(&row.value) && !self.scripts.contains(&row.value) {
                self.scripts.push(row.value.clone());
            }
        }
        for row in &self.rows {
            let Some(stages) = parse_stages(&row.combo) else {
                continue;
            };
            let Some(binding) = self.binding_of(&row.value) else {
                continue;
            };
            if stages.len() == 1 {
                self.index.insert(stages[0], binding);
            } else {
                self.chords
                    .entry(stages[0])
                    .or_default()
                    .insert(stages[1], binding);
            }
        }
    }

    /// What one right-hand side runs, if this implementation can run it at all.
    fn binding_of(&self, value: &str) -> Option<Binding> {
        if let Some(cmd) = Command::parse(value) {
            return Some(Binding::Cmd(cmd));
        }
        if is_script_value(value) {
            let at = self.scripts.iter().position(|p| p == value)?;
            return Some(Binding::Script(at));
        }
        None
    }

    /// Looks up a (mods, vk) combo.
    pub fn lookup(&self, mods: u8, vk: u32) -> Option<Command> {
        self.index.get(&pack(mods, vk)).map(|b| b.command())
    }

    /// Whether this combo is the first stage of a chord, i.e. whether the key
    /// dispatcher has to wait for the next key instead of dropping it.
    pub fn chord_prefix(&self, mods: u8, vk: u32) -> bool {
        self.chords.contains_key(&pack(mods, vk))
    }

    /// Resolves the second stage of a chord whose first stage was `first`.
    ///
    /// The file spells the two stages independently: `C+K,C+8` names Ctrl for
    /// the second key, `C+K,D` does not — but a user who pressed Ctrl+K is
    /// still holding Ctrl when D comes, so the bare form is tried both with
    /// the modifiers as they are and with the first stage's dropped. Without
    /// that, every `C+K,<letter>` row would only fire after the user let go of
    /// the key they just pressed.
    pub fn lookup_chord(&self, first: u64, mods: u8, vk: u32) -> Option<Command> {
        let map = self.chords.get(&first)?;
        let first_mods = ((first >> 32) & 0xFF) as u8;
        map.get(&pack(mods, vk))
            .or_else(|| map.get(&pack(mods & !first_mods, vk)))
            .map(|b| b.command())
    }

    /// The script a [`Command::MacroScript`] index names.
    pub fn script_path(&self, index: usize) -> Option<&str> {
        self.scripts.get(index).map(String::as_str)
    }

    /// The rows as the file spells them, for the manager's list.
    pub fn rows(&self) -> &[KeyRow] {
        &self.rows
    }

    /// The file a save writes.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Every combo bound to `value`, in file order — the manager's 当前快捷键
    /// list. A chord comes back whole (`C+K,R`), as the file writes it.
    pub fn combos_for(&self, value: &str) -> Vec<String> {
        self.rows
            .iter()
            .filter(|r| r.value == value)
            .map(|r| r.combo.clone())
            .collect()
    }

    /// What `combo` runs, for the manager's duplicate prompt: taking a combo
    /// away from another command is a decision the user has to make.
    pub fn value_of(&self, combo: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.combo.eq_ignore_ascii_case(combo))
            .map(|r| r.value.as_str())
    }

    /// Binds `combo` to `value`, replacing whatever that combo ran before.
    /// Returns whether a row was replaced.
    pub fn set_binding(&mut self, combo: &str, value: &str) -> bool {
        let had = self.clear_combo(combo);
        self.rows.push(KeyRow {
            combo: combo.to_string(),
            value: value.to_string(),
        });
        self.reindex();
        had
    }

    /// Drops every row bound to `combo`. Returns whether anything went.
    pub fn clear_combo(&mut self, combo: &str) -> bool {
        let before = self.rows.len();
        self.rows.retain(|r| !r.combo.eq_ignore_ascii_case(combo));
        let removed = self.rows.len() != before;
        if removed {
            self.reindex();
        }
        removed
    }

    /// Writes the `[Shortcut]` section back. The file is the section alone.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no key.ini to write",
            ));
        };
        let mut ini = dg_core::ini::Ini::new();
        ini.replace_section(
            "Shortcut",
            &self
                .rows
                .iter()
                .map(|r| (r.combo.clone(), r.value.clone()))
                .collect::<Vec<_>>(),
        );
        ini.save(path)
    }

    /// Loads key.ini: next to the
    /// executable, then the working
    /// directory, then the bundled
    /// default.
    pub fn load() -> Self {
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                dirs.push(dir.to_path_buf());
            }
        }
        dirs.push(std::path::PathBuf::from("."));
        for dir in &dirs {
            let path = dir.join("key.ini");
            if let Ok(bytes) = std::fs::read(&path)
                && let Some(text) = dg_core::ini::decode(&bytes)
            {
                let mut map = Self::parse(&text);
                map.path = Some(path);
                return map;
            }
        }
        // Nothing on disk yet: a save creates the user copy next to the
        // executable, which is the first place `load` looks.
        let mut map = Self::parse(DEFAULT_KEY_INI);
        map.path = Some(dirs[0].join("key.ini"));
        map
    }
}

/// The combo text for a (mods, vk) pair modifier letters in the order it
/// writes them (C, S, A), then the key name. The manager cannot bind what it
/// cannot name.
pub fn combo_text(mods: u8, vk: u32) -> Option<String> {
    let key = key_name(vk)?;
    let mut out = String::new();
    if mods & MOD_CTRL != 0 {
        out.push('C');
    }
    if mods & MOD_SHIFT != 0 {
        out.push('S');
    }
    if mods & MOD_ALT != 0 {
        out.push('A');
    }
    if !out.is_empty() {
        out.push('+');
    }
    out.push_str(key);
    Some(out)
}

/// The inverse of [`parse_key`]. The spellings are the ones its own `key.ini`
/// uses (`Del`, `Ins`, `PageUp`, `Equal`, `Sub`).
fn key_name(vk: u32) -> Option<&'static str> {
    Some(match vk {
        0x30..=0x39 => match vk {
            0x30 => "0",
            0x31 => "1",
            0x32 => "2",
            0x33 => "3",
            0x34 => "4",
            0x35 => "5",
            0x36 => "6",
            0x37 => "7",
            0x38 => "8",
            _ => "9",
        },
        0x41..=0x5A => match vk {
            0x41 => "A",
            0x42 => "B",
            0x43 => "C",
            0x44 => "D",
            0x45 => "E",
            0x46 => "F",
            0x47 => "G",
            0x48 => "H",
            0x49 => "I",
            0x4A => "J",
            0x4B => "K",
            0x4C => "L",
            0x4D => "M",
            0x4E => "N",
            0x4F => "O",
            0x50 => "P",
            0x51 => "Q",
            0x52 => "R",
            0x53 => "S",
            0x54 => "T",
            0x55 => "U",
            0x56 => "V",
            0x57 => "W",
            0x58 => "X",
            0x59 => "Y",
            _ => "Z",
        },
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x13 => "Pause",
        0x14 => "CapsLock",
        0x1B => "Esc",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2C => "PrintScreen",
        0x2D => "Ins",
        0x2E => "Del",
        0x6A => "Mul",
        0x6B => "Add",
        0x6C => "Separator",
        0x6D => "Sub",
        0x6E => "Decimal",
        0x6F => "Div",
        0x90 => "NumLock",
        0x91 => "ScrollLock",
        0xBA => ";",
        0xBB => "Equal",
        0xBC => "Comma",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        0x70..=0x87 => match vk - 0x70 {
            0 => "F1",
            1 => "F2",
            2 => "F3",
            3 => "F4",
            4 => "F5",
            5 => "F6",
            6 => "F7",
            7 => "F8",
            8 => "F9",
            9 => "F10",
            10 => "F11",
            11 => "F12",
            12 => "F13",
            13 => "F14",
            14 => "F15",
            15 => "F16",
            16 => "F17",
            17 => "F18",
            18 => "F19",
            19 => "F20",
            20 => "F21",
            21 => "F22",
            22 => "F23",
            _ => "F24",
        },
        _ => return None,
    })
}

/// Parses one combo string such as
/// `CS+F`, `C+K` or a bare `Down`
/// into (mods, vk).
fn parse_combo(combo: &str) -> Option<(u8, u32)> {
    let parts: Vec<&str> = combo.split('+').collect();
    let mut mods = 0u8;
    let key = if parts.len() == 1 {
        // bare key: no modifiers
        parts[0]
    } else {
        for part in &parts[..parts.len() - 1] {
            for ch in part.chars() {
                match ch {
                    'C' | 'c' => mods |= MOD_CTRL,
                    'S' | 's' => mods |= MOD_SHIFT,
                    'A' | 'a' => mods |= MOD_ALT,
                    _ => return None,
                }
            }
        }
        parts[parts.len() - 1]
    };
    let vk = parse_key(key)?;
    Some((mods, vk))
}

fn parse_key(key: &str) -> Option<u32> {
    if key.chars().count() == 1 {
        let ch = key.chars().next()?;
        return Some(match ch {
            ';' => 0xBA,
            ',' => 0xBC,
            '-' => 0xBD,
            '.' => 0xBE,
            '/' => 0xBF,
            '`' => 0xC0,
            '[' => 0xDB,
            // The shipped key.ini spells both `C+\` and `A+\`, and neither can
            // mean the Applications key.
            '\\' => 0xDC,
            ']' => 0xDD,
            '\'' => 0xDE,
            c => c.to_ascii_uppercase() as u32,
        });
    }
    let name = key.to_lowercase();
    Some(match name.as_str() {
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        "f13" => 0x7C,
        "f14" => 0x7D,
        "f15" => 0x7E,
        "f16" => 0x7F,
        "f17" => 0x80,
        "f18" => 0x81,
        "f19" => 0x82,
        "f20" => 0x83,
        "f21" => 0x84,
        "f22" => 0x85,
        "f23" => 0x86,
        "f24" => 0x87,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "home" => 0x24,
        "end" => 0x23,
        "prior" | "pageup" => 0x21,
        "next" | "pagedown" => 0x22,
        "ins" | "insert" => 0x2D,
        "del" | "delete" => 0x2E,
        "backspace" => 0x08,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "space" => 0x20,
        "pause" => 0x13,
        "capslock" => 0x14,
        "numlock" => 0x90,
        "scrolllock" => 0x91,
        "printscreen" | "snapshot" => 0x2C,
        // The numpad keys keep the names the table gives them, and `Equal` is the
        // table's name for VK_OEM_PLUS — the row 加大字号 is bound to (`C+Equal`).
        "equal" => 0xBB,
        "comma" => 0xBC,
        "minus" => 0xBD,
        "mul" => 0x6A,
        "add" => 0x6B,
        "separator" => 0x6C,
        "sub" | "subtract" => 0x6D,
        "decimal" => 0x6E,
        "div" => 0x6F,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[Shortcut]
C+S=cm_file_save
CS+F=cm_find
Down=cm_move_down
C+K,R=cm_dock_dirview
A+F3=cm_select_addall
F9=cm_tool_sortasc
F10=cm_edit_date
Bogus=cm_not_a_command
";

    #[test]
    fn parses_modifiers_and_named_keys() {
        let map = KeyMap::parse(SAMPLE);
        assert_eq!(map.lookup(MOD_CTRL, 'S' as u32), Some(Command::Save));
        assert_eq!(
            map.lookup(MOD_CTRL | MOD_SHIFT, 'F' as u32),
            Some(Command::Find)
        );
        assert_eq!(
            map.lookup(0, 0x28),
            Some(Command::Caret(MoveDir::Down, false))
        );
        // A chord is not a plain binding: the first stage latches, the second
        // one resolves.
        assert_eq!(map.lookup(MOD_CTRL, 'K' as u32), None);
        assert!(map.chord_prefix(MOD_CTRL, 'K' as u32));
        assert_eq!(
            map.lookup_chord(pack(MOD_CTRL, 'K' as u32), 0, 'R' as u32),
            Some(Command::DockDirView)
        );
        // Ctrl is still down when the second key arrives, and the row is
        // written bare: the second stage has to inherit the prefix's modifiers.
        assert_eq!(
            map.lookup_chord(pack(MOD_CTRL, 'K' as u32), MOD_CTRL, 'R' as u32),
            Some(Command::DockDirView)
        );
        // A+F3: add-all-selections
        assert_eq!(map.lookup(MOD_ALT, 0x72), Some(Command::AddAllSelections));
        // F9 is the shipped key.ini's own binding for 升序排序, so the row has
        // to resolve to the command the menu bar uses.
        assert_eq!(map.lookup(0, 0x78), Some(Command::SortAsc));
        // known-but-unported commands
        // bind to Ignore
        assert_eq!(map.lookup(0, 0x79), Some(Command::Ignore));
        // unknown commands are dropped
        // (Bogus=cm_not_a_command)
        assert_eq!(map.rows().len(), 8);
    }

    /// `key.ini` spells the two chord stages independently: `C+K,D` leaves the
    /// modifiers out of the second key, `C+K,C+8` names them. A user holding
    /// Ctrl through both presses has to reach either.
    #[test]
    fn chord_second_stage_takes_the_held_modifiers() {
        let map = KeyMap::parse(DEFAULT_KEY_INI);
        let first = pack(MOD_CTRL, 'K' as u32);
        assert_eq!(
            map.lookup_chord(first, MOD_CTRL, 'D' as u32),
            Some(Command::DockDirView)
        );
        assert_eq!(
            map.lookup_chord(first, MOD_CTRL, '8' as u32),
            Some(Command::FoldToLevel(8))
        );
        // Letting go of Ctrl between the stages still reaches the bare form.
        assert_eq!(
            map.lookup_chord(first, 0, 'D' as u32),
            Some(Command::DockDirView)
        );
        // A key the prefix does not bind stays unbound either way.
        assert_eq!(map.lookup_chord(first, MOD_CTRL, 0x7B), None);
    }

    #[test]
    fn combo_parsing() {
        assert_eq!(parse_combo("C+S"), Some((MOD_CTRL, 'S' as u32)));
        assert_eq!(
            parse_combo("CS+F"),
            Some((MOD_CTRL | MOD_SHIFT, 'F' as u32))
        );
        assert_eq!(parse_combo("SA+Down"), Some((MOD_SHIFT | MOD_ALT, 0x28)));
        // a bare key is a valid
        // no-modifier combo
        assert_eq!(parse_combo("X"), Some((0, 'X' as u32)));
        assert_eq!(parse_combo("Q+W"), None);
    }

    /// The shipped `key.ini` binds the zoom family to names the combo parser has
    /// to know.
    #[test]
    fn zoom_rows_bind() {
        let map = KeyMap::parse(DEFAULT_KEY_INI);
        assert_eq!(
            map.lookup(MOD_CTRL, 0xBB),
            Some(Command::ZoomFont(true)),
            "C+Equal"
        );
        assert_eq!(
            map.lookup(MOD_CTRL, 0x6B),
            Some(Command::ZoomFont(true)),
            "C+Add"
        );
        assert_eq!(
            map.lookup(MOD_CTRL | MOD_SHIFT, 0xBD),
            Some(Command::ZoomFont(true)),
            "CS+-"
        );
        assert_eq!(
            map.lookup(MOD_CTRL, 0xBD),
            Some(Command::ZoomFont(false)),
            "C+-"
        );
        assert_eq!(
            map.lookup(MOD_CTRL, 0x6D),
            Some(Command::ZoomFont(false)),
            "C+Sub"
        );
        assert_eq!(
            crate::cmdids::command_for_id(1469),
            Some(Command::FontOriginal)
        );
    }

    /// The OEM row keys are named by the character they print. Reading `A+\` as
    /// 0x5C would bind the Applications key's slot, which that table names
    /// `RightWin`.
    #[test]
    fn oem_row_names_parse_to_their_vk_codes() {
        assert_eq!(parse_combo("A+\\"), Some((MOD_ALT, 0xDC)));
        assert_eq!(parse_combo("C+\\"), Some((MOD_CTRL, 0xDC)));
        assert_eq!(parse_combo("C+]"), Some((MOD_CTRL, 0xDD)));
        assert_eq!(parse_combo("C+["), Some((MOD_CTRL, 0xDB)));
        assert_eq!(parse_combo("C+`"), Some((MOD_CTRL, 0xC0)));
        assert_eq!(parse_combo("C+'"), Some((MOD_CTRL, 0xDE)));
        assert_eq!(parse_combo("C+-"), Some((MOD_CTRL, 0xBD)));
        assert_eq!(parse_combo("C+/"), Some((MOD_CTRL, 0xBF)));
        assert_eq!(parse_combo("CS+-"), Some((MOD_CTRL | MOD_SHIFT, 0xBD)));
        // A letter still keeps its own virtual key.
        assert_eq!(parse_combo("C+W"), Some((MOD_CTRL, 0x57)));
    }

    /// The four 窗口 rows the shipped `key.ini` binds — and the two menu-only
    /// members of the navigation pairs — all resolve through the command names
    /// in the table
    #[test]
    fn window_rows_bind() {
        let map = KeyMap::parse(DEFAULT_KEY_INI);
        assert_eq!(map.lookup(0, 0x7B), Some(Command::LastWindow), "F12");
        assert_eq!(
            map.lookup(0, 0x76),
            Some(Command::ToggleAllPanels),
            "F7"
        );
        assert_eq!(
            map.lookup(MOD_ALT, 0xDC),
            Some(Command::SwitchFocus),
            "A+\\"
        );
        assert_eq!(
            map.lookup(MOD_CTRL | MOD_SHIFT, 'T' as u32),
            Some(Command::OpenRecentlyClosed),
            "CS+T"
        );
        assert_eq!(
            crate::cmdids::command_for_id(1221),
            Some(Command::LastWindow)
        );
        assert_eq!(
            crate::cmdids::command_for_id(1222),
            Some(Command::ToggleAllPanels)
        );
        assert_eq!(
            crate::cmdids::command_for_id(1223),
            Some(Command::SwitchFocus)
        );
        assert_eq!(
            crate::cmdids::command_for_id(1240),
            Some(Command::OpenRecentlyClosed)
        );
        assert_eq!(
            crate::cmdids::command_for_id(1212),
            Some(Command::TabOrderNext)
        );
        assert_eq!(
            crate::cmdids::command_for_id(1213),
            Some(Command::TabOrderPrev)
        );
    }

    /// `KeyMap::parse` keeps every row. Chords are no longer in that set: `C+K,R` binds
    /// through the two-stage index the dialog writes and the key dispatcher latches. The
    /// list below is the debt, named: each row is a feature this implementation does not
    /// have yet, so the shortcut is dead rather than half-implemented. A row that disappears
    /// from `key.ini`, or a new row the table cannot bind, fails this test - the gaps stay
    /// visible instead of quiet.
    #[test]
    fn shipped_table_loses_no_binding() {
        // In `key.ini` order, which is what the walk produces.
        const UNPORTED: &[&str] = &[
            // `C+K` opens a dock panel, and every panel it names is here. The two
            // rows below are not debt: they are the features decided not to be
            // built, listed so the gap stays visible instead of quiet.
            // The command window and the in-process web view.
            "C+K,C=cm_dock_command",
            // No web view.
            "C+B=cm_view_webpreview",
            // Every other `cm_*` name in `key.ini` resolves through the command
            // table.
        ];
        let map = KeyMap::parse(DEFAULT_KEY_INI);
        let mut unresolved = Vec::new();
        for row in map.rows() {
            let binds = parse_stages(&row.combo).is_some() && map.binding_of(&row.value).is_some();
            if !binds {
                unresolved.push(format!("{}={}", row.combo, row.value));
            }
        }
        assert_eq!(
            unresolved.join("|"),
            UNPORTED.join("|"),
            "key.ini rows this implementation cannot bind, beyond the debt already named"
        );
    }

    /// The manager writes what the dispatcher reads: a combo built from a key
    /// press parses back to the same (mods, vk)
    #[test]
    fn combo_text_round_trips() {
        for (mods, vk, text) in [
            (MOD_CTRL, 'S' as u32, "C+S"),
            (MOD_CTRL | MOD_SHIFT, 'F' as u32, "CS+F"),
            (0, 0x28, "Down"),
            (MOD_ALT | MOD_SHIFT, 0x72, "SA+F3"),
            (MOD_CTRL, 0xBB, "C+Equal"),
            (MOD_CTRL, 0xDC, "C+\\"),
            (MOD_CTRL, 0xBD, "C+-"),
        ] {
            assert_eq!(combo_text(mods, vk).as_deref(), Some(text), "vk {vk:#x}");
            assert_eq!(parse_combo(text), Some((mods, vk)), "{text}");
        }
        assert_eq!(combo_text(0, 0x5B), None);
    }

    /// What 分配 does to the map: replace the combo's row, answer the new
    /// binding, and drop the old one when it is replaced.
    #[test]
    fn manager_edits_rows() {
        let mut map = KeyMap::parse("[Shortcut]\nC+S=cm_file_save\n");
        assert_eq!(map.combos_for("cm_file_save"), vec!["C+S".to_string()]);
        assert!(!map.set_binding("C+F12", "cm_config_shortcut"));
        assert_eq!(map.lookup(MOD_CTRL, 0x7B), Some(Command::ConfigShortcut));
        assert_eq!(
            map.combos_for("cm_config_shortcut"),
            vec!["C+F12".to_string()]
        );
        // Rebinding the same combo replaces the row instead of stacking one.
        assert!(map.set_binding("C+S", "cm_find"));
        assert_eq!(map.lookup(MOD_CTRL, 'S' as u32), Some(Command::Find));
        assert_eq!(map.rows().len(), 2);
        // 删除.
        assert!(map.clear_combo("C+S"));
        assert_eq!(map.lookup(MOD_CTRL, 'S' as u32), None);
        assert!(!map.clear_combo("C+S"));
    }

    /// 语法着色 (cm_config_syntax, 1459) 在 key.ini 里是可绑定的命令名 —
    /// 菜单行和快捷键走的是同一张表。
    #[test]
    fn cm_config_syntax_resolves_to_the_dialog() {
        let map = KeyMap::parse("[Shortcut]\nC+F12=cm_config_syntax\n");
        assert_eq!(map.lookup(MOD_CTRL, 0x7B), Some(Command::ConfigSyntax));
    }

    /// 主题 (cm_config_theme, 1298) 原来只在菜单里占一行、`parse` 不认识它，
    /// 所以 `command_for_id` 把它当 `Ignore` 灰着。这条钉住「那一行现在活了」。
    #[test]
    fn cm_config_theme_is_a_live_command() {
        assert_eq!(
            crate::cmdids::command_for_id(1298),
            Some(Command::ConfigTheme)
        );
        let map = KeyMap::parse("[Shortcut]\nC+F11=cm_config_theme\n");
        assert_eq!(map.lookup(MOD_CTRL, 0x7A), Some(Command::ConfigTheme));
    }

    /// 设置字体 (1321) 的命令名在表里，`parse` 不认识它的时候
    /// `command_for_id` 返回 None，菜单栏就把那一行画成灰的。这条同时钉住
    /// 名字可解析、菜单可点、key.ini 可绑定三件事。
    #[test]
    fn cm_config_setfont_is_a_live_command() {
        assert_eq!(
            Command::parse("cm_config_setfont"),
            Some(Command::ConfigFont)
        );
        assert_eq!(
            crate::cmdids::command_for_id(1321),
            Some(Command::ConfigFont)
        );
        assert!(crate::cmdids::is_answerable(1321), "设置字体 不能被置灰");
        let map = KeyMap::parse("[Shortcut]\nC+F8=cm_config_setfont\n");
        assert_eq!(map.lookup(MOD_CTRL, 0x77), Some(Command::ConfigFont));
    }

    /// Six `key.ini` rows that used to bind nothing reach a command now: 目录树
    /// shares the directory dock's pane, 默认字体 drops the document back to the
    /// configured size, 外部浏览器预览 opens the document's address, Ctrl+F12
    /// opens 跳转 on its symbol rows, and the two reading modes collapse the
    /// chrome.
    #[test]
    fn the_six_newest_chords_bind() {
        let map = KeyMap::parse(DEFAULT_KEY_INI);
        let ctrl_k = pack(MOD_CTRL, 'K' as u32);
        assert_eq!(map.lookup_chord(ctrl_k, 0, 'M' as u32), Some(Command::DockDirView));
        assert_eq!(
            map.lookup_chord(ctrl_k, 0, 'R' as u32),
            Some(Command::ReadBrowserMode)
        );
        assert_eq!(
            map.lookup_chord(ctrl_k, 0, 'F' as u32),
            Some(Command::ReadFocusMode)
        );
        assert_eq!(
            map.lookup(MOD_CTRL | MOD_SHIFT, 'B' as u32),
            Some(Command::WebPreviewExternal)
        );
        assert_eq!(map.lookup(MOD_CTRL, '0' as u32), Some(Command::FontOriginal));
        assert_eq!(map.lookup(MOD_CTRL, 0x7B), Some(Command::GotoTags));
    }

    /// A value that is a path binds a script; a dead `cm_*` name does not.
    #[test]
    fn a_path_value_binds_a_script() {
        let map = KeyMap::parse("[Shortcut]\nC+F12=macro\\run.mac\nF12=cm_dock_command\n");
        assert_eq!(map.lookup(MOD_CTRL, 0x7B), Some(Command::MacroScript(0)));
        assert_eq!(map.script_path(0), Some("macro\\run.mac"));
        assert_eq!(map.lookup(0, 0x7B), None);
        // Two rows naming the same script share one index.
        let map = KeyMap::parse("[Shortcut]\nF11=a\\x.mac\nF12=a\\x.mac\n");
        assert_eq!(map.lookup(0, 0x7A), Some(Command::MacroScript(0)));
        assert_eq!(map.lookup(0, 0x7B), Some(Command::MacroScript(0)));
    }

    /// A save writes the section back in the form `parse` reads, so a manager
    /// session survives a restart.
    #[test]
    fn save_round_trips_the_section() {
        let path = std::env::temp_dir().join(format!(
            "dg_keymap_{}_{}.ini",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut map = KeyMap::parse("[Shortcut]\r\nC+S=cm_file_save\r\nC+K,R=cm_dock_dirview\r\n");
        map.path = Some(path.clone());
        map.set_binding("C+F12", "macro\\run.mac");
        map.save().expect("save");
        let text = std::fs::read_to_string(&path).expect("read back");
        let again = KeyMap::parse(&text);
        assert_eq!(again.rows(), map.rows());
        assert_eq!(again.lookup(MOD_CTRL, 0x7B), Some(Command::MacroScript(0)));
        assert_eq!(
            again.lookup_chord(pack(MOD_CTRL, 'K' as u32), 0, 'R' as u32),
            Some(Command::DockDirView)
        );
        let _ = std::fs::remove_file(&path);
    }
}

    #[test]
    fn split_and_group_names_parse() {
        assert_eq!(Command::parse("cm_window_vertsplit"), Some(Command::SplitVertical));
        assert_eq!(Command::parse("cm_window_horzsplit"), Some(Command::SplitHorizontal));
        assert_eq!(Command::parse("cm_window_clone"), Some(Command::CloneView));
        assert_eq!(Command::parse("cm_window_syncscroll"), Some(Command::SyncScrollToggle));
        assert_eq!(Command::parse("cm_window_tovertgroup"), Some(Command::MoveToVertGroup));
        assert_eq!(Command::parse("cm_window_tohorzgroup"), Some(Command::MoveToHorzGroup));
        assert_eq!(Command::parse("cm_window_mergegroup"), Some(Command::MergeGroups));
        assert_eq!(Command::parse("cm_window_nosplit"), None);
    }
