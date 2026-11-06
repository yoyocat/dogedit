//! Undo/redo command system: every revertible edit is an object that
//! knows how to apply itself forwards and backwards. Concrete
//! commands record exactly the text they delete so the inverse edit
//! restores it; `MacroCommand` is a composite that replays its
//! children (macro recording). Commands live on a dual stack in the
//! view: edits push onto `undo`, undo pops onto `redo`, and any new
//! edit clears `redo`. Positions are the document's native `(line,
//! col)` pairs. The inverse edit's end position is derived from the
//! recorded text alone ([`text_end_pos`]), so undo never re-measures
//! the mutated document.

use crate::document::Document;
use crate::linedoc::Pos;

/// A line-count change caused by an edit:
/// the (line, col) the edit anchored at and
/// how many lines it adds (negative removes).
/// Undo/redo report these so callers can
/// keep line-addressed state (bookmarks) in
/// sync with the text.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ChangeSpan {
    pub line: usize,
    pub col: usize,
    pub line_delta: isize,
}

impl ChangeSpan {
    pub fn new(line: usize, col: usize, line_delta: isize) -> Self {
        Self {
            line,
            col,
            line_delta,
        }
    }
}

/// Lines `text` contributes when inserted — CR, LF and CRLF each
/// close a line, the same split the store performs.
pub fn text_lines(text: &str) -> isize {
    let b = text.as_bytes();
    let mut n = 0isize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                n += 1;
                i += if i + 1 < b.len() && b[i + 1] == b'\n' {
                    2
                } else {
                    1
                };
            }
            b'\n' => {
                n += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    n
}

/// The (line, col) a caret lands on after
/// inserting `text` at `pos` — the same
/// split the store's insert performs
/// (segments per EOL): CR,
/// LF and CRLF each close a line, a
/// trailing terminator leaves an empty
/// final line.
pub fn text_end_pos(pos: Pos, text: &str) -> Pos {
    let mut line = pos.line;
    let mut col = pos.col;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                line += 1;
                col = 0;
                if chars.clone().next() == Some('\n') {
                    chars.next();
                }
            }
            '\n' => {
                line += 1;
                col = 0;
            }
            _ => col += 1,
        }
    }
    Pos { line, col }
}

/// A single undoable edit: apply it, or revert it.
pub trait Command {
    /// Apply (redo=true) or revert (redo=false)
    /// the edit on `doc`.
    fn exec(&mut self, doc: &mut Document, redo: bool);
    /// Unique monotonically increasing id,
    /// parity with the global the field
    /// command counter.
    fn id(&self) -> u64;
    /// Line-count change the edit causes at
    /// its anchor line, in the direction
    /// `redo`. Used to shift line-addressed
    /// state (bookmarks) on undo/redo.
    fn spans(&self, redo: bool) -> Vec<ChangeSpan>;
    /// The edits this command performs in the direction `redo`, as the same data an
    /// [`EditOp`] carries when the edit goes forward: the caller folds them into the
    /// syntax mirror and the layout region instead of re-doing the whole document.
    /// `None` when the command cannot describe them that way — a replayed macro,
    /// whose children are offsets into each other's output rather than one
    /// gesture's disjoint edits.
    fn edit_ops(&self, redo: bool) -> Option<Vec<EditOp>>;
}

/// Insert `text` at `pos`. Reverting removes
/// the inserted span.
pub struct InsertCommand {
    id: u64,
    pos: Pos,
    text: String,
}

impl InsertCommand {
    pub fn new(id: u64, pos: Pos, text: impl Into<String>) -> Self {
        Self {
            id,
            pos,
            text: text.into(),
        }
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Command for InsertCommand {
    fn exec(&mut self, doc: &mut Document, redo: bool) {
        if redo {
            doc.edit_pos(self.pos, self.pos, &self.text);
        } else {
            let end = text_end_pos(self.pos, &self.text);
            doc.edit_pos(self.pos, end, "");
        }
    }

    fn spans(&self, redo: bool) -> Vec<ChangeSpan> {
        let delta = text_lines(&self.text);
        vec![ChangeSpan::new(
            self.pos.line,
            self.pos.col,
            if redo { delta } else { -delta },
        )]
    }

    fn edit_ops(&self, redo: bool) -> Option<Vec<EditOp>> {
        Some(vec![if redo {
            EditOp::Insert {
                pos: self.pos,
                text: self.text.clone(),
            }
        } else {
            EditOp::Delete {
                start: self.pos,
                end: text_end_pos(self.pos, &self.text),
                text: self.text.clone(),
            }
        }])
    }

    fn id(&self) -> u64 {
        self.id
    }
}

/// Delete the `(line, col)` range `start..end`,
/// keeping the removed text so undo restores it.
pub struct DeleteCommand {
    id: u64,
    start: Pos,
    end: Pos,
    text: String,
}

impl DeleteCommand {
    pub fn new(id: u64, start: Pos, end: Pos, text: impl Into<String>) -> Self {
        Self {
            id,
            start,
            end,
            text: text.into(),
        }
    }

    pub fn start(&self) -> Pos {
        self.start
    }

    pub fn end(&self) -> Pos {
        self.end
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Command for DeleteCommand {
    fn exec(&mut self, doc: &mut Document, redo: bool) {
        if redo {
            doc.edit_pos(self.start, self.end, "");
        } else {
            doc.edit_pos(self.start, self.start, &self.text);
        }
    }

    fn spans(&self, redo: bool) -> Vec<ChangeSpan> {
        let delta = text_lines(&self.text);
        vec![ChangeSpan::new(
            self.start.line,
            self.start.col,
            if redo { -delta } else { delta },
        )]
    }

    fn edit_ops(&self, redo: bool) -> Option<Vec<EditOp>> {
        Some(vec![if redo {
            EditOp::Delete {
                start: self.start,
                end: self.end,
                text: self.text.clone(),
            }
        } else {
            EditOp::Insert {
                pos: self.start,
                text: self.text.clone(),
            }
        }])
    }

    fn id(&self) -> u64 {
        self.id
    }
}

/// Replace `start..end` with `new_text`,
/// keeping `old_text` for undo.
pub struct ReplaceCommand {
    id: u64,
    start: Pos,
    end: Pos,
    old_text: String,
    new_text: String,
}

impl ReplaceCommand {
    pub fn new(
        id: u64,
        start: Pos,
        end: Pos,
        old_text: impl Into<String>,
        new_text: impl Into<String>,
    ) -> Self {
        Self {
            id,
            start,
            end,
            old_text: old_text.into(),
            new_text: new_text.into(),
        }
    }
}

impl Command for ReplaceCommand {
    fn exec(&mut self, doc: &mut Document, redo: bool) {
        if redo {
            doc.edit_pos(self.start, self.end, &self.new_text);
        } else {
            let end = text_end_pos(self.start, &self.new_text);
            doc.edit_pos(self.start, end, &self.old_text);
        }
    }

    fn spans(&self, redo: bool) -> Vec<ChangeSpan> {
        let delta = text_lines(&self.new_text)
            - text_lines(&self.old_text);
        vec![ChangeSpan::new(
            self.start.line,
            self.start.col,
            if redo { delta } else { -delta },
        )]
    }

    fn edit_ops(&self, redo: bool) -> Option<Vec<EditOp>> {
        Some(vec![if redo {
            EditOp::Replace {
                start: self.start,
                end: self.end,
                old_text: self.old_text.clone(),
                new_text: self.new_text.clone(),
            }
        } else {
            EditOp::Replace {
                start: self.start,
                end: text_end_pos(self.start, &self.new_text),
                old_text: self.new_text.clone(),
                new_text: self.old_text.clone(),
            }
        }])
    }

    fn id(&self) -> u64 {
        self.id
    }
}

/// Composite command — replays its children in
/// order (a macro is a list of commands, each applied with the same
/// `exec(redo)` call). Undo replays in
/// reverse.
pub struct MacroCommand {
    id: u64,
    commands: Vec<Box<dyn Command>>,
    /// True for the composite of one undo group ([`UndoStack::end_group`]): its
    /// children are the disjoint edits of a single gesture, every one of them still
    /// expressed against the document the gesture started from. A recorded macro
    /// is not that — each of its edits starts where the previous one left off.
    grouped: bool,
}

impl MacroCommand {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            commands: Vec::new(),
            grouped: false,
        }
    }

    /// The composite of one undo group.
    pub fn grouped(id: u64) -> Self {
        Self {
            id,
            commands: Vec::new(),
            grouped: true,
        }
    }

    pub fn push(&mut self, cmd: Box<dyn Command>) {
        self.commands.push(cmd);
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Command for MacroCommand {
    fn exec(&mut self, doc: &mut Document, redo: bool) {
        if redo {
            for c in self.commands.iter_mut() {
                c.exec(doc, true);
            }
        } else {
            for c in self.commands.iter_mut().rev() {
                c.exec(doc, false);
            }
        }
    }

    fn spans(&self, redo: bool) -> Vec<ChangeSpan> {
        let mut out = Vec::new();
        if redo {
            for c in self.commands.iter() {
                out.extend(c.spans(true));
            }
        } else {
            for c in self.commands.iter().rev() {
                out.extend(c.spans(false));
            }
        }
        out
    }

    fn edit_ops(&self, redo: bool) -> Option<Vec<EditOp>> {
        if !self.grouped {
            return None;
        }
        let mut out = Vec::new();
        for c in self.commands.iter() {
            out.extend(c.edit_ops(redo)?);
        }
        Some(out)
    }

    fn id(&self) -> u64 {
        self.id
    }
}

/// Editable operation description — the data
/// needed to build a `Command`. Kept as plain
/// data so the stack can clone it into both
/// the undo history and a recording macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOp {
    Insert { pos: Pos, text: String },
    Delete { start: Pos, end: Pos, text: String },
    Replace {
        start: Pos,
        end: Pos,
        old_text: String,
        new_text: String,
    },
}

impl EditOp {
    fn into_command(self, id: u64) -> Box<dyn Command> {
        match self {
            EditOp::Insert { pos, text } => {
                Box::new(InsertCommand::new(id, pos, text))
            }
            EditOp::Delete { start, end, text } => {
                Box::new(DeleteCommand::new(id, start, end, text))
            }
            EditOp::Replace {
                start,
                end,
                old_text,
                new_text,
            } => Box::new(ReplaceCommand::new(
                id, start, end, old_text, new_text,
            )),
        }
    }
}

pub struct UndoStack {
    undo: Vec<Box<dyn Command>>,
    redo: Vec<Box<dyn Command>>,
    next_id: u64,
    limit: usize,
    /// Commands dropped from the bottom by
    /// `limit`. Kept separate from `undo`
    /// because the save point below is a
    /// position in history: trimming must not
    /// shift it, or hitting the limit would
    /// silently clear the modified flag.
    trimmed: usize,
    /// History position at the last save; see
    /// [`UndoStack::is_modified`]. `None` means
    /// the buffer was never marked saved, in
    /// which case any recorded edit counts as
    /// unsaved.
    save_index: Option<usize>,
    /// 强制标脏。恢复出来的标签内容不是从磁盘读来的，历史位置还停在
    /// 0，光看位置会判成"干净" —— 那样关窗不拦、Ctrl+S 也不写回原文件。
    /// 只有 [`UndoStack::mark_modified`] 置位，保存后由 `mark_saved` 清掉。
    forced: bool,
    /// Macro recording buffer: `(macro_id,
    /// ops)`. While open, applied edits are
    /// appended here instead of the undo
    /// stack — the whole macro becomes one
    /// undo step when recording ends.
    recording: Option<(u64, Vec<EditOp>)>,
    /// 暂停 (cm_macro_pause, 1578): the macro-capture latch. While set,
    /// a recording is still open but applied edits fall through to
    /// the undo stack as individual steps capture, so a paused edit
    /// stays separately undoable.
    macro_paused: bool,
    /// Undo-step grouping: the edits collected between
    /// [`UndoStack::begin_group`] and [`UndoStack::end_group`] become one history
    /// entry. The multi-caret edit uses it, so the one Backspace that removes five
    /// words comes back with one 撤销.
    group: Option<Vec<EditOp>>,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoStack {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            limit: 1000,
            trimmed: 0,
            save_index: None,
            forced: false,
            recording: None,
            macro_paused: false,
            group: None,
        }
    }

    /// Position in history: counts every
    /// command ever applied and not yet
    /// undone, including the ones `limit`
    /// dropped.
    ///
    /// Public because the backup timer reads it as a change token: an edit
    /// moves it, an undo moves it back, and two equal readings mean the
    /// buffer has nothing new to write down.
    pub fn position(&self) -> usize {
        self.undo.len() + self.trimmed
    }

    /// Records the current position as "what
    /// is on disk". Call after a successful
    /// save *and* after loading a file.
    pub fn mark_saved(&mut self) {
        let pos = self.position();
        self.save_index = Some(pos);
        self.forced = false;
    }

    /// 强制标脏：恢复出来的内容不在磁盘上，标签得带 `*`、关窗得拦、
    /// Ctrl+S 得写回原文件。下一次保存把它清掉。
    pub fn mark_modified(&mut self) {
        self.forced = true;
    }

    /// True when the buffer differs from its
    /// last save. Undoing back to the saved
    /// position clears it, which is the point
    /// of tracking a position rather than a
    /// "someone edited" boolean.
    pub fn is_modified(&self) -> bool {
        if self.forced {
            return true;
        }
        match self.save_index {
            Some(i) => self.position() != i,
            None => self.position() > 0 || self.recording.is_some(),
        }
    }

    /// History position captured by
    /// [`UndoStack::mark_saved`], for tests and
    /// for callers that need to compare two
    /// documents' saved states.
    pub fn save_position(&self) -> Option<usize> {
        self.save_index
    }


    /// Apply an edit: execute it, push it on
    /// the undo stack, drop the redo stack,
    /// and (while recording) append it to the
    /// pending macro.
    pub fn apply(&mut self, doc: &mut Document, op: EditOp) -> bool {
        let id = self.alloc_id();
        let mut cmd = op.clone().into_command(id);
        cmd.exec(doc, true);
        if !self.macro_paused && let Some(r) = self.recording.as_mut() {
            r.1.push(op);
            return true;
        }
        if let Some(g) = self.group.as_mut() {
            g.push(op);
            return true;
        }
        self.redo.clear();
        self.undo.push(cmd);
        if self.undo.len() > self.limit {
            self.undo.remove(0);
            self.trimmed += 1;
        }
        true
    }

    /// Reverts the top command, returning the
    /// change spans it undid (empty when the
    /// stack is empty).
    pub fn undo(&mut self, doc: &mut Document) -> Vec<ChangeSpan> {
        let mut cmd = match self.undo.pop() {
            Some(c) => c,
            None => return Vec::new(),
        };
        cmd.exec(doc, false);
        let spans = cmd.spans(false);
        self.redo.push(cmd);
        spans
    }

    /// Re-applies the top redo command,
    /// returning its change spans (empty
    /// when the stack is empty).
    pub fn redo(&mut self, doc: &mut Document) -> Vec<ChangeSpan> {
        let mut cmd = match self.redo.pop() {
            Some(c) => c,
            None => return Vec::new(),
        };
        cmd.exec(doc, true);
        let spans = cmd.spans(true);
        self.undo.push(cmd);
        spans
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The edits the next [`UndoStack::undo`] would land, in the order it lands
    /// them — `None` when the top command cannot describe them (a replayed macro).
    pub fn undo_ops(&self) -> Option<Vec<EditOp>> {
        self.undo.last().and_then(|c| c.edit_ops(false))
    }

    /// [`UndoStack::undo_ops`] for the next [`UndoStack::redo`].
    pub fn redo_ops(&self) -> Option<Vec<EditOp>> {
        self.redo.last().and_then(|c| c.edit_ops(true))
    }

    /// Max undo steps kept before the oldest is dropped. This
    /// implementation has no settings page for it yet.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
    }

    /// Drops history, as when a different
    /// document is loaded. The save point goes
    /// with it: a fresh stack is "never saved",
    /// which reads as unmodified at position 0
    /// and as modified from the first edit. A
    /// caller that just loaded bytes from disk
    /// must [`UndoStack::mark_saved`] itself.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.trimmed = 0;
        self.save_index = None;
        self.recording = None;
        self.macro_paused = false;
        self.group = None;
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Sets the macro-capture pause latch (1578). While
    /// paused, recorded edits skip the macro buffer and
    /// land on the undo stack as individual steps.
    pub fn set_macro_paused(&mut self, paused: bool) {
        self.macro_paused = paused;
    }

    /// The macro-capture pause latch (1578).
    pub fn is_macro_paused(&self) -> bool {
        self.macro_paused
    }

    /// The ops captured so far while
    /// recording (`None` when not
    /// recording).
    pub fn recording_ops(&self) -> Option<&[EditOp]> {
        self.recording
            .as_ref()
            .map(|(_, ops)| ops.as_slice())
    }

    /// Builds a replayable macro from
    /// recorded edit ops, minting
    /// fresh child ids.
    pub fn macro_from_ops(
        &mut self,
        ops: &[EditOp],
    ) -> Option<MacroCommand> {
        if ops.is_empty() {
            return None;
        }
        let mut m = MacroCommand::new(self.alloc_id());
        for op in ops.iter().cloned() {
            m.push(op.into_command(self.alloc_id()));
        }
        Some(m)
    }

    /// [`UndoStack::macro_from_ops`] for one undo group: the composite says so,
    /// which is what lets an undo of it re-land through the incremental paths.
    fn group_from_ops(&mut self, ops: &[EditOp]) -> Option<MacroCommand> {
        if ops.is_empty() {
            return None;
        }
        let mut m = MacroCommand::grouped(self.alloc_id());
        for op in ops.iter().cloned() {
            m.push(op.into_command(self.alloc_id()));
        }
        Some(m)
    }

    pub fn begin_macro(&mut self) {
        if self.recording.is_none() {
            self.recording = Some((self.alloc_id(), Vec::new()));
        }
    }

    /// Ends macro recording. The recorded edits
    /// become one composite undo step; a
    /// structurally identical MacroCommand is
    /// returned so the caller can store and
    /// replay it.
    pub fn end_macro(&mut self) -> Option<MacroCommand> {
        let (id, ops) = self.recording.take()?;
        if ops.is_empty() {
            return None;
        }
        let mut history = MacroCommand::new(id);
        for op in ops.iter().cloned() {
            history.push(op.into_command(self.alloc_id()));
        }
        let mut playback = MacroCommand::new(id);
        for op in ops {
            playback.push(op.into_command(self.alloc_id()));
        }
        self.redo.clear();
        self.undo.push(Box::new(history));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        Some(playback)
    }

    /// Opens the undo group. Every edit that lands while it is open is
    /// collected instead of becoming its own history entry.
    pub fn begin_group(&mut self) {
        if self.group.is_none() {
            self.group = Some(Vec::new());
        }
    }

    /// Closes the group: what it collected becomes one composite history entry,
    /// reverted and replayed as a unit. An empty group leaves the history and the
    /// redo stack untouched, and a macro recording that took the edits itself has
    /// already got them, so nothing is pushed twice.
    pub fn end_group(&mut self) {
        let Some(ops) = self.group.take() else {
            return;
        };
        if ops.is_empty() || (!self.macro_paused && self.recording.is_some()) {
            return;
        }
        let Some(m) = self.group_from_ops(&ops) else {
            return;
        };
        self.redo.clear();
        self.undo.push(Box::new(m));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
            self.trimmed += 1;
        }
    }

    /// Replay a recorded macro against `doc`.
    /// Each child is also pushed onto the undo
    /// stack as one composite step.
    pub fn play_macro(&mut self, doc: &mut Document, mut macro_cmd: MacroCommand) -> bool {
        macro_cmd.exec(doc, true);
        self.redo.clear();
        self.undo.push(Box::new(macro_cmd));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        true
    }

    /// Capture the exact text of a `(line, col)`
    /// range — each line contributes its own
    /// stored terminator (for DeleteCommand /
    /// ReplaceCommand construction).
    pub fn capture(doc: &Document, start: Pos, end: Pos) -> String {
        doc.range_text(start, end)
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;


    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn text_end_pos_splits_like_the_store() {
        assert_eq!(text_end_pos(pos(0, 0), "abc"), pos(0, 3));
        assert_eq!(text_end_pos(pos(0, 1), "a\nb"), pos(1, 1));
        assert_eq!(text_end_pos(pos(0, 0), "a\r\nb"), pos(1, 1));
        // a trailing terminator opens an empty final line
        assert_eq!(text_end_pos(pos(0, 0), "a\r\n"), pos(1, 0));
        assert_eq!(text_end_pos(pos(0, 0), "a\n"), pos(1, 0));
        assert_eq!(text_end_pos(pos(0, 0), "a\r"), pos(1, 0));
        // CR counts as a column before its LF
        assert_eq!(text_end_pos(pos(2, 4), "\r\n"), pos(3, 0));
    }

    #[test]
    fn insert_undo_redo_roundtrip() {
        let mut d = doc("hello");
        let mut s = UndoStack::new();
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 5),
            text: " world".into(),
        });
        assert_eq!(d.to_text(), "hello world");
        assert!(!s.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "hello");
        assert!(!s.redo(&mut d).is_empty());
        assert_eq!(d.to_text(), "hello world");
        // a new edit clears redo
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 0),
            text: "X".into(),
        });
        assert!(!s.can_redo());
    }

    #[test]
    fn insert_undo_across_lines() {
        let mut d = doc("ab\ncd");
        let mut s = UndoStack::new();
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 1),
            text: "X\nY".into(),
        });
        assert_eq!(d.to_text(), "aX\nYb\ncd");
        assert!(!s.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "ab\ncd");
        assert!(!s.redo(&mut d).is_empty());
        assert_eq!(d.to_text(), "aX\nYb\ncd");
    }

    #[test]
    fn delete_restores_text() {
        let mut d = doc("hello world");
        let mut s = UndoStack::new();
        let text = UndoStack::capture(&d, pos(0, 5), pos(0, 11));
        s.apply(&mut d, EditOp::Delete {
            start: pos(0, 5),
            end: pos(0, 11),
            text,
        });
        assert_eq!(d.to_text(), "hello");
        assert!(!s.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "hello world");
    }

    #[test]
    fn replace_swaps_text() {
        let mut d = doc("fn main() {}");
        let mut s = UndoStack::new();
        let old = UndoStack::capture(&d, pos(0, 0), pos(0, 2));
        s.apply(&mut d, EditOp::Replace {
            start: pos(0, 0),
            end: pos(0, 2),
            old_text: old,
            new_text: "pub".into(),
        });
        assert_eq!(d.to_text(), "pub main() {}");
        assert!(!s.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "fn main() {}");
    }

    #[test]
    fn macro_replays_and_undoes_as_one_step() {
        let mut d = doc("a");
        let mut s = UndoStack::new();
        s.begin_macro();
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 1),
            text: "b".into(),
        });
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 2),
            text: "c".into(),
        });
        let m = s.end_macro().expect("macro recorded");
        assert_eq!(m.len(), 2);
        assert_eq!(d.to_text(), "abc");
        // macro itself is one undo step
        assert!(!s.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "a");
        assert!(!s.redo(&mut d).is_empty());
        assert_eq!(d.to_text(), "abc");
    }

    #[test]
    fn play_macro_reapplies() {
        let mut d = doc("x");
        let mut s = UndoStack::new();
        let mut m = MacroCommand::new(99);
        m.push(Box::new(InsertCommand::new(1, pos(0, 1), "y")));
        m.push(Box::new(InsertCommand::new(2, pos(0, 2), "z")));
        assert!(s.play_macro(&mut d, m));
        assert_eq!(d.to_text(), "xyz");
        assert!(!s.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "x");
    }

    #[test]
    fn insert_spans_track_line_delta() {
        let c = InsertCommand::new(1, pos(1, 3), "a\n\nb");
        assert_eq!(c.spans(true), vec![ChangeSpan::new(1, 3, 2)]);
        assert_eq!(c.spans(false), vec![ChangeSpan::new(1, 3, -2)]);
        let single = InsertCommand::new(2, pos(0, 0), "flat");
        assert_eq!(single.spans(true)[0].line_delta, 0);
    }

    #[test]
    fn delete_spans_track_line_delta() {
        let c = DeleteCommand::new(1, pos(0, 0), pos(0, 1), "x\ny\n");
        assert_eq!(c.spans(true), vec![ChangeSpan::new(0, 0, -2)]);
        assert_eq!(c.spans(false), vec![ChangeSpan::new(0, 0, 2)]);
    }

    #[test]
    fn replace_spans_track_net_delta() {
        let c = ReplaceCommand::new(1, pos(0, 0), pos(0, 1), "a", "x\ny");
        assert_eq!(c.spans(true), vec![ChangeSpan::new(0, 0, 1)]);
        assert_eq!(c.spans(false), vec![ChangeSpan::new(0, 0, -1)]);
    }

    #[test]
    fn macro_spans_flatten_in_exec_order() {
        let mut m = MacroCommand::new(1);
        m.push(Box::new(InsertCommand::new(10, pos(0, 0), "a\n")));
        m.push(Box::new(DeleteCommand::new(11, pos(1, 0), pos(1, 5), "b\nc\n")));
        // redo replays children in order
        assert_eq!(
            m.spans(true),
            vec![
                ChangeSpan::new(0, 0, 1),
                ChangeSpan::new(1, 0, -2),
            ]
        );
        // undo replays reversed, deltas negated
        assert_eq!(
            m.spans(false),
            vec![
                ChangeSpan::new(1, 0, 2),
                ChangeSpan::new(0, 0, -1),
            ]
        );
    }

    #[test]
    fn undo_redo_report_spans() {
        let mut d = doc("a\nb\nc");
        let mut s = UndoStack::new();
        s.apply(&mut d, EditOp::Insert {
            pos: pos(1, 0),
            text: "X\nY\n".into(),
        });
        assert_eq!(s.undo(&mut d), vec![ChangeSpan::new(1, 0, -2)]);
        assert_eq!(s.redo(&mut d), vec![ChangeSpan::new(1, 0, 2)]);
        // one more undo succeeds, then the stack is drained
        assert_eq!(s.undo(&mut d), vec![ChangeSpan::new(1, 0, -2)]);
        assert_eq!(s.undo(&mut d), Vec::new());
    }

    #[test]
    fn modified_tracks_history_position() {
        let mut d = doc("hello");
        let mut s = UndoStack::new();
        // never saved, nothing applied
        assert!(!s.is_modified());
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 5),
            text: "!".into(),
        });
        assert!(s.is_modified());
        s.mark_saved();
        assert!(!s.is_modified());
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 0),
            text: "X".into(),
        });
        assert!(s.is_modified());
        // undoing back to the save point is clean again: the flag tracks
        // position, not "did anyone ever type"
        s.undo(&mut d);
        assert!(!s.is_modified());
        s.redo(&mut d);
        assert!(s.is_modified());
    }

    #[test]
    fn fresh_stack_after_clear_is_unsaved_but_clean() {
        let mut d = doc("x");
        let mut s = UndoStack::new();
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 0),
            text: "y".into(),
        });
        s.mark_saved();
        s.clear();
        assert_eq!(s.save_position(), None);
        assert!(!s.is_modified());
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 0),
            text: "z".into(),
        });
        assert!(s.is_modified());
    }

    #[test]
    fn hitting_the_history_limit_keeps_the_modified_flag() {
        let mut d = doc("");
        let mut s = UndoStack::new();
        s.set_limit(4);
        for i in 0..4 {
            s.apply(&mut d, EditOp::Insert {
                pos: pos(0, 0),
                text: format!("{i}"),
            });
        }
        s.mark_saved();
        assert!(!s.is_modified());
        // 5th apply trims the oldest command; the save point must not move
        s.apply(&mut d, EditOp::Insert {
            pos: pos(0, 0),
            text: "9".into(),
        });
        assert!(s.is_modified());
    }
}
