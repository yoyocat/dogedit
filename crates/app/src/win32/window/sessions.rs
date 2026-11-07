//! 最近文件与会话（`history.ini` / `session/`）—— `EditorState` 的这一组方法。
//!
//! 从 `window.rs` 原样搬来：逻辑一行未改，只把 `fn` 放宽成 `pub(super) fn`。
//! 原因是 Rust 的可见性以**定义所在的模块**为准 —— 子模块里的私有方法对父模块
//! 不可见，而 `window.rs` 里剩下的命令分发、菜单和测试都要调它们。
//! `window.rs` 仍是 `EditorState` 的定义处；这里只有第二个 `impl` 块。

use super::*;

impl EditorState {

    // -----------------------------------------------------------------------
    // 最近的文件 (ids 35001+) and 会话 (ids 2801+) Both menus are state, not
    // definition — the bar's ROWS table carries their static rows at run
    // time. The recent entries carry the caret the file was left at inside
    // the item string — `path|line|col` in the real history.ini — which is
    // what puts the caret back on a click.
    // -----------------------------------------------------------------------

    /// The shipped setting.ini carries 16.
    pub(super) fn recent_max(&self) -> usize {
        self.setting_section_key("Setting", "RecentDoc")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(16)
    }

    /// Reads the persisted state: `[RecentDoc]` into the recent list and the
    /// `[Session]` name list into `sessions`.
    pub(super) fn load_history_state(&mut self) {
        let path = crate::win32::grepdlg::ini_path();
        let ini = crate::win32::grepdlg::ini_load(&path);
        let max = self.recent_max();
        let count: usize = crate::win32::grepdlg::ini_get(&ini, "RecentDoc", "Count", "0")
            .parse()
            .unwrap_or(0);
        for i in 0..count.min(max) {
            let raw = crate::win32::grepdlg::ini_get(&ini, "RecentDoc", &format!("Item{i}"), "");
            let (p, cursor) = split_mru_entry(&raw);
            // No real path is that short.
            if p.chars().count() < 3 {
                continue;
            }
            self.recent_files.push((p, cursor));
        }
        let count: usize = crate::win32::grepdlg::ini_get(&ini, "Session", "Count", "0")
            .parse()
            .unwrap_or(0);
        for i in 0..count {
            let name = crate::win32::grepdlg::ini_get(&ini, "Session", &format!("Item{i}"), "");
            if !name.is_empty() {
                self.sessions.push(name);
            }
        }
    }

    /// Rewrites `[RecentDoc]` in history.ini, leaving every other section
    /// (the find histories, the window position) exactly as they were.
    pub(super) fn persist_recent(&self) {
        let path = crate::win32::grepdlg::ini_path();
        let mut ini = crate::win32::grepdlg::ini_load(&path);
        crate::win32::grepdlg::ini_remove_section(&mut ini, "RecentDoc");
        crate::win32::grepdlg::ini_set(
            &mut ini,
            "RecentDoc",
            "Count",
            &self.recent_files.len().to_string(),
        );
        for (i, (p, cursor)) in self.recent_files.iter().enumerate() {
            let value = match cursor {
                Some((line, col)) => format!("{p}|{line}|{col}"),
                None => p.clone(),
            };
            crate::win32::grepdlg::ini_set(&mut ini, "RecentDoc", &format!("Item{i}"), &value);
        }
        crate::win32::grepdlg::ini_save(&ini, &path);
    }

    /// Rewrites the 会话 name list.
    pub(super) fn persist_sessions(&self) {
        let path = crate::win32::grepdlg::ini_path();
        let mut ini = crate::win32::grepdlg::ini_load(&path);
        crate::win32::grepdlg::ini_remove_section(&mut ini, "Session");
        crate::win32::grepdlg::ini_set(
            &mut ini,
            "Session",
            "Count",
            &self.sessions.len().to_string(),
        );
        for (i, name) in self.sessions.iter().enumerate() {
            crate::win32::grepdlg::ini_set(&mut ini, "Session", &format!("Item{i}"), name);
        }
        crate::win32::grepdlg::ini_save(&ini, &path);
    }

    /// A file was opened: its row moves to the top of the 最近的文件 list,
    /// deduplicated, clamped to `[Setting] RecentDoc`, and the ini is
    /// rewritten. A cursor already stored for the path survives the move —
    /// it belongs to the file, not to the click.
    pub(super) fn mru_touch(&mut self, path: &str) {
        let cursor = self
            .recent_files
            .iter()
            .find(|(p, _)| p == path)
            .and_then(|(_, c)| *c);
        self.recent_files.retain(|(p, _)| p != path);
        self.recent_files.insert(0, (path.to_string(), cursor));
        self.recent_files.truncate(self.recent_max());
        self.persist_recent();
        self.rebuild_recent_menu();
    }

    /// A document was closed: the caret it was left at goes into its recent
    /// entry.
    pub(super) fn mru_set_cursor(&mut self, path: &str, line: usize, col: usize) {
        let Some(slot) = self
            .recent_files
            .iter_mut()
            .find(|(p, _)| p == path)
        else {
            return;
        };
        slot.1 = Some((line, col));
        self.persist_recent();
    }

    /// Rebuilds the 最近的文件 popup's dynamic rows: everything from the
    /// static base (清除历史记录 and its separator) down is removed, then a
    /// separator and one row per entry — `&1 …` through `&9 …` for the first
    /// nine, bare numbers past that — commanded 35001+i.
    pub(super) fn rebuild_recent_menu(&mut self) {
        let Some(bar) = self.menu_bar.as_mut() else {
            return;
        };
        let file = unsafe { bar.submenu(menubar::file_menu_index()) };
        if file.0.is_null() {
            return;
        }
        let (_, recent_pos) = menubar::file_menu_dynamic_popups();
        let recent = unsafe { menu::child_popup(file, recent_pos) };
        if recent.0.is_null() {
            return;
        }
        // 清除历史记录 (1236) is the popup's only static row; the separator
        // and the items form the dynamic block after it
        // order (live walk: 1236, SEP, 35001, 35002, …). The separator rides
        // with the rows — pushing it from the static table as well would put
        // two lines between 1236 and the first item.
        unsafe {
            bar.clear_scripts(recent, 1);
        }
        if self.recent_files.is_empty() {
            return;
        }
        unsafe {
            bar.push_separator(recent);
        }
        for (i, (path, _)) in self.recent_files.iter().enumerate() {
            unsafe {
                bar.push_command(recent, 35001 + i as u32, &mru_caption(i, path));
            }
        }
    }

    /// Rebuilds the 会话 popup's dynamic rows the same way: the three static
    /// rows (保存到新会话/保存到当前会话/清除会话菜单) stay, a separator and one row per
    /// saved session follow, commanded 2801+i.
    pub(super) fn rebuild_session_menu(&mut self) {
        let Some(bar) = self.menu_bar.as_mut() else {
            return;
        };
        let file = unsafe { bar.submenu(menubar::file_menu_index()) };
        if file.0.is_null() {
            return;
        }
        let (session_pos, _) = menubar::file_menu_dynamic_popups();
        let session = unsafe { menu::child_popup(file, session_pos) };
        if session.0.is_null() {
            return;
        }
        unsafe {
            bar.clear_scripts(session, 3);
        }
        if self.sessions.is_empty() {
            return;
        }
        unsafe {
            bar.push_separator(session);
        }
        for (i, name) in self.sessions.iter().enumerate() {
            unsafe {
                bar.push_command(session, 2801 + i as u32, name);
            }
        }
    }

    /// A click on a recent-file row: open the file, and put the caret back
    /// where the entry recorded it — the same `goto_target` hand-off the
    /// search results and 恢复最后关闭的文件 use. A file that has gone away
    /// leaves the row in place and opens nothing.
    pub(super) fn open_recent_row(&mut self, index: usize) {
        let Some((path, cursor)) = self.recent_files.get(index) else {
            return;
        };
        let (path, cursor) = (path.clone(), *cursor);
        if !std::path::Path::new(&path).exists() {
            return;
        }
        if let Some((line, col)) = cursor {
            self.goto_target = Some((path.clone(), line, col));
        }
        self.open_file(&path);
        self.apply_goto_target();
    }

    /// 清除历史记录 (cm_file_clearhis, 1236). Exactly that section, whatever
    /// the command's description promises about find and replace
    /// histories.
    pub(super) fn clear_history(&mut self, hwnd: HWND) {
        if self.ask(
            hwnd,
            "DogEdit",
            &lt(40081, "确定执行这个操作吗?"),
            MB_OKCANCEL | MB_ICONQUESTION,
        ) != IDOK.0
        {
            return;
        }
        let path = crate::win32::grepdlg::ini_path();
        let mut ini = crate::win32::grepdlg::ini_load(&path);
        crate::win32::grepdlg::ini_remove_section(&mut ini, "RecentDoc");
        crate::win32::grepdlg::ini_save(&ini, &path);
        self.recent_files.clear();
        self.rebuild_recent_menu();
    }

    /// The file a session name loads from: next to the executable, where the rest of
    /// this implementation's data files (setting.ini, history.ini) live. The row
    /// list carries names only, so the lookup has to be deterministic.
    pub(super) fn session_file_path(&self, name: &str) -> std::path::PathBuf {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        dir.join(format!("{name}.session"))
    }

    /// Writes the open documents into a session file: one `[Item%d]` section
    /// per document with a path
    /// `[Session] Count` first, then `Path`/`Type`/`EOL`/`BOM`/`Encoding`/
    /// `Cursor`/`Order` per item. The cursor key is this implementation's own encoding
    /// (`line,col`)
    pub(super) fn write_session_data(&self, path: &std::path::Path) {
        let mut ini = crate::win32::grepdlg::ini_new();
        let mut count = 0usize;
        for (i, doc) in self.docs.iter().enumerate() {
            let Some(p) = doc.path.as_deref() else {
                continue;
            };
            let sec = format!("Item{count}");
            crate::win32::grepdlg::ini_set(&mut ini, &sec, "Path", p);
            // Type 0 is a text document
            // distinguishes are not sessioned here.
            crate::win32::grepdlg::ini_set(&mut ini, &sec, "Type", "0");
            let eol = match doc.eol {
                Eol::Crlf => 0,
                Eol::Lf => 1,
                Eol::Cr => 2,
            };
            crate::win32::grepdlg::ini_set(&mut ini, &sec, "EOL", &eol.to_string());
            crate::win32::grepdlg::ini_set(
                &mut ini,
                &sec,
                "BOM",
                if doc.doc.has_bom() { "Y" } else { "N" },
            );
            crate::win32::grepdlg::ini_set(&mut ini, &sec, "Encoding", doc.doc.encoding().name());
            crate::win32::grepdlg::ini_set(
                &mut ini,
                &sec,
                "Cursor",
                &format!("{},{}", doc.caret.0, doc.caret.1),
            );
            crate::win32::grepdlg::ini_set(&mut ini, &sec, "Order", &i.to_string());
            count += 1;
        }
        crate::win32::grepdlg::ini_set(&mut ini, "Session", "Count", &count.to_string());
        crate::win32::grepdlg::ini_save(&ini, path);
    }

    /// Reads a session file back into open order: highest item first so the last file
    /// opened. Sections whose path is empty are skipped.
    pub(super) fn read_session_data(path: &std::path::Path) -> Vec<(String, Option<(usize, usize)>)> {
        let ini = crate::win32::grepdlg::ini_load(path);
        let count: usize = crate::win32::grepdlg::ini_get(&ini, "Session", "Count", "0")
            .parse()
            .unwrap_or(0);
        let mut items = Vec::new();
        for i in 0..count {
            let sec = format!("Item{i}");
            let p = crate::win32::grepdlg::ini_get(&ini, &sec, "Path", "");
            if p.is_empty() {
                continue;
            }
            let cursor = parse_session_cursor(&crate::win32::grepdlg::ini_get(
                &ini,
                &sec,
                "Cursor",
                "",
            ));
            items.push((p, cursor));
        }
        items
    }

    /// 保存到新会话 (cm_file_save_newsession, 1241). `无标题` — plus the extension
    /// (the session menu). The chosen name joins the 会话 list and becomes
    /// the current session; the data lands at the chosen path and, when
    /// that is elsewhere, also next to the exe where the row's click will
    /// look.
    pub(super) fn save_new_session(&mut self, hwnd: HWND) {
        let Some(path) = self.file_dialog(
            hwnd,
            true,
            &lt(40067, "保存到新会话"),
            "*.session\0*.session\0*.*\0*.*\0",
            Some(&lt(40069, "无标题.session")),
        ) else {
            return;
        };
        let Some(name) = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
        else {
            return;
        };
        let chosen = std::path::PathBuf::from(&path);
        self.write_session_data(&chosen);
        let canonical = self.session_file_path(&name);
        if canonical != chosen {
            self.write_session_data(&canonical);
        }
        self.current_session = name.clone();
        self.sessions.retain(|s| *s != name);
        self.sessions.insert(0, name);
        self.persist_sessions();
        self.rebuild_session_menu();
    }

    /// 保存到当前会话 (cm_file_save_currentsession, 1243). The session gate
    /// gates on the current session's name being non-empty — with none, the
    /// row is disabled and the command does nothing.
    pub(super) fn save_current_session(&mut self) {
        if self.current_session.is_empty() {
            return;
        }
        let path = self.session_file_path(&self.current_session);
        self.write_session_data(&path);
    }

    /// 清除会话菜单 (1242). The session name reader asks the same OK/Cancel question
    /// as 1236, then empties the name vector, clears the current session
    /// name and rewrites `[Session] Count` as 0.
    pub(super) fn clear_session_menu(&mut self, hwnd: HWND) {
        if self.ask(
            hwnd,
            "DogEdit",
            &lt(40081, "确定执行这个操作吗?"),
            MB_OKCANCEL | MB_ICONQUESTION,
        ) != IDOK.0
        {
            return;
        }
        self.sessions.clear();
        self.current_session.clear();
        self.persist_sessions();
        self.rebuild_session_menu();
    }

    pub(super) fn remove_session(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        self.sessions.remove(index);
        self.persist_sessions();
        self.rebuild_session_menu();
    }

    /// A click on a 会话 row (the session rows share one id range, counted from
    /// its base). The branches, in order:
    ///
    /// - the session's file exists — **Ctrl+click** removes the entry without a
    /// prompt; a plain click closes every open document first (the 关闭全部 command,
    /// skipped when **Shift** is held, and bailed out of when a save prompt was
    /// cancelled), then makes the name current and loads the file; a
    /// **Ctrl+click** on a missing file skips its notice box;
    /// - the file is gone — the notice (`%s\n\n该文件不存在!`,
    /// warning icon) shows unless Ctrl is held, and the stale entry is
    /// removed either way.
    pub(super) fn session_row_clicked(&mut self, hwnd: HWND, index: usize) {
        let Some(name) = self.sessions.get(index).cloned() else {
            return;
        };
        let file = self.session_file_path(&name);
        let ctrl = unsafe { GetKeyState(0x11) } < 0;
        let shift = unsafe { GetKeyState(0x10) } < 0;
        if file.exists() {
            if ctrl {
                self.remove_session(index);
                return;
            }
            if !shift {
                self.close_docs(CloseScope::All, hwnd);
                // 关闭全部窗口 (1202) leaves documents on screen when a save
                // prompt was cancelled
                // when documents are still on screen.
                let still_open = self.docs.len() > 1
                    || (self.docs.len() == 1 && !self.docs.act().is_untitled_blank());
                if still_open {
                    return;
                }
            }
            self.current_session = name;
            self.session_queue = Self::read_session_data(&file)
                .into_iter()
                .map(|(path, cursor)| QueueItem::File { path, cursor })
                .collect();
            self.session_load_next(hwnd);
        } else {
            if !ctrl {
                let text = lfmt(40082, &[&name])
                    .unwrap_or_else(|| format!("{name}\n\n该文件不存在!"));
                self.ask(hwnd, "DogEdit", &text, MB_ICONWARNING);
            }
            self.remove_session(index);
        }
    }

    /// Advances the session load by one file. The progressive loader is
    /// single-slot — starting a second load orphans the first one's events —
    /// so the queue advances from the load's `Done` branch; a large file
    /// loads synchronously and never owns the channel, so the walk continues
    /// straight through it.
    pub(super) fn session_load_next(&mut self, hwnd: HWND) {
        while let Some(item) = self.session_queue.pop() {
            match item {
                QueueItem::File { path, cursor } => {
                    if let Some((line, col)) = cursor {
                        self.goto_target = Some((path.clone(), line, col));
                    }
                    self.open_file(&path);
                    self.apply_goto_target();
                    let large = std::fs::metadata(&path)
                        .map(|m| m.len() >= LARGE_FILE_THRESHOLD)
                        .unwrap_or(false);
                    if !large {
                        return;
                    }
                }
                QueueItem::Hot { key } => {
                    let root = self.session.settings.root();
                    let Some((bytes, meta)) = crate::win32::session::read_hot(&root, &key) else {
                        // 载荷在回收里没了：这一项跳过，队列继续。它不再算数，
                        // 所以恢复计数跟着减，别把没开的标签报给用户。
                        self.session_restore_total = self.session_restore_total.saturating_sub(1);
                        continue;
                    };
                    let staging = crate::win32::session::staging_path(&root, &key);
                    if std::fs::write(&staging, &bytes).is_err() {
                        self.session_restore_total = self.session_restore_total.saturating_sub(1);
                        continue;
                    }
                    // 编码按记下来的那份指定：源文件是 GBK 又没有 BOM 的场合，
                    // 光看字节猜不出来。
                    let enc = encoding_rs::Encoding::for_label(meta.encoding.as_bytes());
                    self.open_file_enc(&staging.to_string_lossy(), enc);
                    let id = self.docs.act().id;
                    if matches!(&self.session_active, Some(ActiveTab::Hot(k)) if *k == key) {
                        self.session_active_id = Some(id);
                    }
                    let title = if meta.path.is_empty() {
                        Some(if meta.title.is_empty() {
                            lt(15021, "无标题")
                        } else {
                            meta.title.clone()
                        })
                    } else {
                        None
                    };
                    self.restore_pending = Some(RestorePending {
                        id,
                        path: meta.path.clone(),
                        title,
                        cursor: Some(meta.cursor),
                        temp: Some(staging),
                    });
                    // 内容已经在标签里了，载荷不用留着 —— 下次启动不该再还原一遍。
                    crate::win32::session::drop_hot(&root, &key);
                    return;
                }
            }
        }
        self.session_restore_done(hwnd);
    }

    /// 会话恢复队列走完了：切回退出时活动的那个标签，报一次数。
    pub(super) fn session_restore_done(&mut self, hwnd: HWND) {
        let active = match self.session_active.take() {
            Some(ActiveTab::File(path)) => self.docs.index_of_path(&path),
            // 热保存的那一份：恢复时记下了文档 id，未命名文档只能这么认。
            Some(ActiveTab::Hot(_)) => self
                .session_active_id
                .and_then(|id| self.docs.index_of_id(id)),
            None => None,
        };
        self.session_active_id = None;
        if let Some(index) = active {
            self.switch_doc(index, hwnd);
        }
        // 安全模式那句话优先：它解释的是「窗格和配色为什么都不见了」，比
        // 「恢复了几个标签」要紧。两句进同一个消息区，后说的盖掉先说的。
        let restored = std::mem::take(&mut self.session_restore_total);
        if crate::safety::on() {
            self.show_status_message(hwnd, "安全模式：脚本和外部主题都没加载".to_string());
        } else if restored > 0 {
            self.show_status_message(
                hwnd,
                crate::win32::lang::format(40111, &[&restored.to_string()])
                    .unwrap_or_else(|| format!("已恢复上次的 {restored} 个标签页")),
            );
        }
        // 命令行带来的文件最后开：它们才是用户此刻要看的那些。资源管理器
        // 一次选中多个文件时会给出好几个，顺序按命令行上的次序。
        for path in std::mem::take(&mut self.session_open_after) {
            self.open_file(&path);
        }
    }

    /// 把一批文件排到「会话恢复完之后」；没在恢复就立刻开。
    ///
    /// 启动时的 `-g`/命令行文件和别的进程用 `WM_COPYDATA` 交过来的文件走
    /// 同一条路。恢复是渐进的（一次只装一份，热保存那份还要等载入完成），
    /// 这时候直接 `open_file` 会和队列抢同一批状态，所以统一排队。
    pub(super) fn queue_or_open_files(&mut self, files: &[String]) {
        if files.is_empty() {
            return;
        }
        let restoring = !self.session_queue.is_empty() || self.restore_pending.is_some();
        self.session_open_after.extend(files.iter().cloned());
        if !restoring {
            for path in std::mem::take(&mut self.session_open_after) {
                self.open_file(&path);
            }
        }
    }

    /// 会话：普通打开的文件在加载完成后把上次的光标与书签贴回去。恢复标签
    /// （热保存 / 备份恢复）自己带着光标，不走这里。
    pub(super) fn apply_session_memory(&mut self, index: usize) {
        let Some(path) = self.docs.get(index).and_then(|d| d.path.clone()) else {
            return;
        };
        if self.session.settings.cursor && self.goto_target.is_none() {
            if let Some((line, col)) = self.session.memory.cursor(&path) {
                self.goto_target = Some((path.clone(), line, col));
            }
        }
        if self.session.settings.markers {
            if let Some(lines) = self.session.memory.marks(&path) {
                let count = self
                    .docs
                    .get(index)
                    .map(|d| d.doc.line_count())
                    .unwrap_or(0);
                if let Some(d) = self.docs.get_mut(index) {
                    // 文件可能被外部改短了：越界的书签行留着也画不出来。
                    d.bookmarks
                        .set_lines(lines.into_iter().filter(|l| *l < count));
                }
            }
        }
    }

    /// 关掉一份文档 / 退出之前，把它的光标和书签记下来。
    pub(super) fn session_remember(&mut self, path: &str, caret: (usize, usize), marks: Vec<usize>) {
        if !self.session.settings.cursor && !self.session.settings.markers {
            return;
        }
        if self.session.settings.cursor {
            self.session.memory.set_cursor(path, caret);
        }
        if self.session.settings.markers {
            self.session.memory.set_marks(path, marks);
        }
        let root = self.session.settings.root();
        self.session.memory.save(&root);
    }

    /// 退出前的一次快照：光标 / 书签、标签页清单、脏文档的热保存。
    ///
    /// 顺序很重要 —— 热保存必须在「保存吗」的询问**之前**写，否则用户选
    /// 「否」丢掉的那份内容根本没被抄下来。用户在询问里选「是」时，
    /// `save_file` 会调 [`Self::session_drop_hot`] 把这份删掉。
    pub(super) fn session_capture(&mut self) {
        let root = self.session.settings.root();
        let _ = std::fs::create_dir_all(&root);

        // ① 光标 / 书签：每份有路径的文档一条。
        let mut remembered = Vec::new();
        for d in self.docs.iter() {
            if let Some(p) = d.path.clone() {
                remembered.push((p, d.caret, d.bookmarks.lines()));
            }
        }
        for (path, caret, marks) in remembered {
            self.session_remember(&path, caret, marks);
        }

        // ② 热保存 + 清单。两件事在同一个循环里做：热保存的那份在清单里是
        //    `H|key`，其余是 `F|path`。
        let settings = self.session.settings.clone();
        let active = self.docs.active();
        let mut items = Vec::new();
        let mut active_index = 0;
        let mut written = Vec::new();
        for (index, d) in self.docs.iter().enumerate() {
            // 只读标签（查找结果）和 hex 文档没有「未保存的内容」可言。
            if d.read_only || d.hex.is_some() {
                continue;
            }
            if d.is_modified() && !d.doc.is_large() && settings.hot {
                let key = match d.path.as_deref() {
                    Some(p) => crate::win32::autobackup::key_for_path(p),
                    None => crate::win32::autobackup::key_for_untitled(d.id),
                };
                let meta = crate::win32::session::HotMeta {
                    path: d.path.clone().unwrap_or_default(),
                    title: d
                        .tab_title
                        .clone()
                        .unwrap_or_else(|| crate::win32::lang::editor_name(d)),
                    cursor: d.caret,
                    encoding: d.doc.encoding().name().to_string(),
                    bom: d.doc.has_bom(),
                    time: 0,
                };
                if crate::win32::session::write_hot(&root, &key, &d.doc, &meta) {
                    written.push((d.id, key.clone()));
                    if index == active {
                        active_index = items.len();
                    }
                    items.push(crate::win32::session::OpenItem::Hot { key });
                    continue;
                }
            }
            // 未命名又没有热保存：没有可以重开的东西，跳过。
            let Some(path) = d.path.clone() else {
                continue;
            };
            if index == active {
                active_index = items.len();
            }
            items.push(crate::win32::session::OpenItem::File {
                path,
                cursor: Some(d.caret),
            });
        }
        for (id, key) in written {
            self.session.note_hot(id, key);
        }
        crate::win32::session::write_open(
            &root,
            &crate::win32::session::OpenList {
                items,
                active: active_index,
            },
        );
        // 上次留下的过期载荷顺手清一遍。
        crate::win32::session::prune_hot(&root);
    }

    /// 恢复还没落地就被打断（标签关了、加载被顶掉）：临时文件删掉，别留在
    /// `hot\` 里等 30 天回收。
    pub(super) fn abort_restore_pending(&mut self) {
        if let Some(p) = self.restore_pending.take() {
            if let Some(tmp) = p.temp {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }

    /// 取消退出：这次写出去的热保存全部作废。
    pub(super) fn session_abort_capture(&mut self) {
        let root = self.session.settings.root();
        for (_, key) in self.session.take_hot() {
            crate::win32::session::drop_hot(&root, &key);
        }
    }

    /// 保存成功：这份文档的热保存副本可以删了（内容已经在磁盘上）。
    pub(super) fn session_drop_hot(&mut self, id: u64) {
        if let Some(key) = self.session.forget_hot(id) {
            let root = self.session.settings.root();
            crate::win32::session::drop_hot(&root, &key);
        }
    }

    /// 启动时的会话恢复：把上次的标签页清单排进队列，一份一份开。
    /// `command_line` 是这次启动带来的全部文件，按命令行顺序，恢复完再开。
    pub(super) fn session_restore_start(&mut self, hwnd: HWND, command_line: &[String]) {
        let settings = self.session.settings.clone();
        if !settings.files && !settings.hot {
            for path in command_line {
                self.open_file(path);
            }
            return;
        }
        let root = settings.root();
        let list = crate::win32::session::read_open(&root);
        let mut queue = Vec::new();
        for item in &list.items {
            match item {
                crate::win32::session::OpenItem::File { path, cursor } if settings.files => {
                    // 文件没了就不开：清单里的残条不该变成错误弹窗。
                    if std::path::Path::new(path).exists() {
                        queue.push(QueueItem::File {
                            path: path.clone(),
                            cursor: *cursor,
                        });
                    }
                }
                crate::win32::session::OpenItem::Hot { key } if settings.hot => {
                    queue.push(QueueItem::Hot { key: key.clone() });
                }
                _ => {}
            }
        }
        self.session_active = list.items.get(list.active).map(|item| match item {
            crate::win32::session::OpenItem::File { path, .. } => ActiveTab::File(path.clone()),
            crate::win32::session::OpenItem::Hot { key } => ActiveTab::Hot(key.clone()),
        });
        self.session_open_after = command_line.to_vec();
        if queue.is_empty() {
            // 清单是空的（第一次启动 / 选项刚打开）：只开命令行文件。
            self.session_restore_done(hwnd);
            return;
        }
        self.session_restore_total = queue.len();
        // 队列是弹栈的：倒着进，正着开。
        queue.reverse();
        self.session_queue = queue;
        self.session_load_next(hwnd);
    }
}
