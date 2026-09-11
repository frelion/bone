use std::cell::Cell;

use unicode_segmentation::UnicodeSegmentation;

use super::layout::vertical_cursor;

/// Per-draft interaction state. The viewport is derived rendering state and is
/// deliberately excluded from persisted drafts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditorState {
    anchor: Option<usize>,
    viewport: Cell<usize>,
    typing: bool,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditSnapshot {
    text: String,
    cursor: usize,
}

impl EditorState {
    pub(crate) fn viewport_origin(&self) -> usize {
        self.viewport.get()
    }

    pub(crate) fn viewport(&self) -> &Cell<usize> {
        &self.viewport
    }

    pub(crate) fn selection(&self, cursor: usize) -> Option<std::ops::Range<usize>> {
        self.anchor
            .filter(|anchor| *anchor != cursor)
            .map(|anchor| anchor.min(cursor)..anchor.max(cursor))
    }

    pub(crate) fn checkpoint(&mut self, text: &str, cursor: usize) {
        self.typing = false;
        self.redo.clear();
        Self::push(&mut self.undo, text, cursor);
        self.anchor = None;
    }

    pub(crate) fn history_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|item| item.text.len())
            .sum()
    }

    pub(crate) fn evict_oldest(&mut self) -> bool {
        if !self.undo.is_empty() {
            self.undo.remove(0);
            true
        } else if !self.redo.is_empty() {
            self.redo.remove(0);
            true
        } else {
            false
        }
    }

    fn push(stack: &mut Vec<EditSnapshot>, text: &str, cursor: usize) {
        const BYTES: usize = 4 * 1024 * 1024;
        if text.len() > BYTES {
            stack.clear();
            return;
        }
        stack.push(EditSnapshot {
            text: text.into(),
            cursor,
        });
        while stack.len() > 64 || stack.iter().map(|item| item.text.len()).sum::<usize>() > BYTES {
            stack.remove(0);
        }
    }

    fn undo(&mut self, text: &mut String, cursor: &mut usize, revision: &mut u64, redo: bool) {
        self.typing = false;
        let (source, target) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        if let Some(previous) = source.pop() {
            Self::push(target, text, *cursor);
            *text = previous.text;
            *cursor = previous.cursor;
            *revision = revision.wrapping_add(1);
            self.anchor = None;
        }
    }
}

/// A short-lived mutable view over one orphan, session, or answer draft.
pub(crate) struct EditBuffer<'a> {
    text: &'a mut String,
    cursor: &'a mut usize,
    revision: &'a mut u64,
    state: &'a mut EditorState,
}

impl<'a> EditBuffer<'a> {
    pub(crate) fn new(
        text: &'a mut String,
        cursor: &'a mut usize,
        revision: &'a mut u64,
        state: &'a mut EditorState,
    ) -> Self {
        Self {
            text,
            cursor,
            revision,
            state,
        }
    }

    pub(crate) fn stop_typing(&mut self) {
        self.state.typing = false;
    }

    pub(crate) fn clear(&mut self) -> bool {
        if self.text.is_empty() {
            return false;
        }
        self.state.checkpoint(self.text, *self.cursor);
        self.text.clear();
        *self.cursor = 0;
        *self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn undo(&mut self, redo: bool) {
        self.state.undo(self.text, self.cursor, self.revision, redo);
    }

    pub(crate) fn insert(&mut self, value: &str, typing: bool) -> bool {
        if value.is_empty() {
            return false;
        }
        let selection = self.state.selection(*self.cursor);
        if !typing || !self.state.typing || selection.is_some() {
            self.state.checkpoint(self.text, *self.cursor);
        }
        self.state.typing = typing;
        if let Some(range) = selection {
            *self.cursor = range.start;
            self.text.replace_range(range, "");
        }
        self.text.insert_str(*self.cursor, value);
        *self.cursor = ceil_grapheme_boundary(self.text, *self.cursor + value.len());
        *self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn delete_before(&mut self) -> bool {
        if let Some(range) = self.state.selection(*self.cursor) {
            self.state.checkpoint(self.text, *self.cursor);
            *self.cursor = range.start;
            self.text.replace_range(range, "");
            *self.cursor = ceil_grapheme_boundary(self.text, *self.cursor);
        } else if let Some((index, _)) =
            self.text[..*self.cursor].grapheme_indices(true).next_back()
        {
            self.state.checkpoint(self.text, *self.cursor);
            self.text.drain(index..*self.cursor);
            *self.cursor = ceil_grapheme_boundary(self.text, index);
        } else {
            return false;
        }
        *self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn delete_after(&mut self) -> bool {
        if let Some(range) = self.state.selection(*self.cursor) {
            self.state.checkpoint(self.text, *self.cursor);
            *self.cursor = range.start;
            self.text.replace_range(range, "");
            *self.cursor = ceil_grapheme_boundary(self.text, *self.cursor);
        } else if let Some(len) = self.text[*self.cursor..]
            .graphemes(true)
            .next()
            .map(str::len)
        {
            self.state.checkpoint(self.text, *self.cursor);
            self.text.drain(*self.cursor..*self.cursor + len);
            *self.cursor = ceil_grapheme_boundary(self.text, *self.cursor);
        } else {
            return false;
        }
        *self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Apply the reducer's rich cursor action while preserving its selection
    /// semantics: an unmodified move clears the anchor before moving.
    pub(crate) fn move_cursor(
        &mut self,
        direction: i8,
        width: u16,
        select: bool,
        word: bool,
        preferred_column: &mut Option<usize>,
    ) {
        if select {
            self.state.anchor.get_or_insert(*self.cursor);
        } else {
            self.state.anchor = None;
        }
        *self.cursor = match direction {
            -1 | 1 if word => word_cursor(self.text, *self.cursor, direction > 0),
            -1 | 1 => moved_cursor(self.text, *self.cursor, direction as isize),
            -2 | 2 => vertical_cursor(
                self.text,
                *self.cursor,
                width,
                direction > 0,
                preferred_column,
            ),
            -3 | 3 => line_edge(self.text, *self.cursor, direction > 0),
            _ => *self.cursor,
        };
    }

    pub(crate) fn extend_pointer_selection(&mut self, byte: usize) {
        self.state.anchor.get_or_insert(*self.cursor);
        *self.cursor = floor_grapheme_boundary(self.text, byte);
    }

    pub(crate) fn begin_pointer_selection(&mut self, byte: usize) {
        *self.cursor = floor_grapheme_boundary(self.text, byte);
        self.state.anchor = Some(*self.cursor);
    }

    /// Legacy horizontal actions collapse an existing selection toward the
    /// requested side before moving by a grapheme.
    pub(crate) fn move_horizontal(&mut self, delta: isize) {
        *self.cursor = if let Some(range) = self.state.selection(*self.cursor) {
            if delta < 0 { range.start } else { range.end }
        } else {
            moved_cursor(self.text, *self.cursor, delta)
        };
        self.state.anchor = None;
    }

    pub(crate) fn move_vertical(
        &mut self,
        width: u16,
        down: bool,
        preferred_column: &mut Option<usize>,
    ) {
        self.state.anchor = None;
        *self.cursor = vertical_cursor(self.text, *self.cursor, width, down, preferred_column);
    }

    pub(crate) fn move_line_edge(&mut self, end: bool) {
        self.state.anchor = None;
        *self.cursor = line_edge(self.text, *self.cursor, end);
    }
}

pub(crate) fn floor_grapheme_boundary(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(std::iter::once(text.len()))
        .take_while(|at| *at <= byte)
        .last()
        .unwrap_or(0)
}

fn ceil_grapheme_boundary(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(std::iter::once(text.len()))
        .find(|at| *at >= byte)
        .unwrap_or(text.len())
}

fn moved_cursor(text: &str, cursor: usize, delta: isize) -> usize {
    if delta < 0 {
        text[..cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(cursor, |(index, _)| index)
    } else {
        text[cursor..]
            .graphemes(true)
            .next()
            .map_or(cursor, |value| cursor + value.len())
    }
}

fn word_cursor(value: &str, cursor: usize, right: bool) -> usize {
    if right {
        value
            .unicode_word_indices()
            .find(|(start, word)| start + word.len() > cursor)
            .map_or(value.len(), |(start, word)| start + word.len())
    } else {
        value
            .unicode_word_indices()
            .rev()
            .find(|(start, _)| *start < cursor)
            .map_or(0, |(start, _)| start)
    }
}

fn line_edge(text: &str, cursor: usize, end: bool) -> usize {
    if end {
        cursor + text[cursor..].find('\n').unwrap_or(text.len() - cursor)
    } else {
        text[..cursor].rfind('\n').map_or(0, |index| index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_is_one_undo_step_and_never_splits_a_cluster() {
        let mut text = String::new();
        let mut cursor = 0;
        let mut revision = 0;
        let mut state = EditorState::default();
        {
            let mut buffer = EditBuffer::new(&mut text, &mut cursor, &mut revision, &mut state);
            buffer.insert("e", true);
            buffer.insert("\u{301}", true);
            buffer.insert("🙂", true);
            buffer.delete_before();
            buffer.undo(false);
        }
        assert_eq!(text, "e\u{301}🙂");
        assert_eq!(cursor, text.len());
        {
            let mut buffer = EditBuffer::new(&mut text, &mut cursor, &mut revision, &mut state);
            buffer.undo(false);
        }
        assert_eq!(text, "");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn selection_replacement_uses_grapheme_boundaries() {
        let mut text = "A👨‍👩‍👧‍👦B".to_owned();
        let mut cursor = text.len();
        let mut revision = 0;
        let mut state = EditorState::default();
        {
            let mut buffer = EditBuffer::new(&mut text, &mut cursor, &mut revision, &mut state);
            buffer.begin_pointer_selection(2);
            buffer.extend_pointer_selection("A👨‍👩‍👧‍👦".len());
            buffer.insert("中", false);
        }
        assert_eq!(text, "A中B");
        assert_eq!(cursor, "A中".len());
        assert_eq!(revision, 1);
    }

    #[test]
    fn word_navigation_handles_unicode_words_and_punctuation() {
        let value = "one, café two";
        assert_eq!(word_cursor(value, 0, true), 3);
        assert_eq!(word_cursor(value, 3, true), 10);
        assert_eq!(word_cursor(value, 10, false), 5);
    }
}
