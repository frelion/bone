use unicode_segmentation::UnicodeSegmentation;

use super::layout::vertical_cursor;
use crate::state::{CursorMove, EditCommand};

/// One complete editable document. Text, cursor identity and interaction
/// history move together when a draft changes owner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EditorBuffer {
    pub(crate) text: String,
    pub(crate) cursor: usize,
    pub(crate) revision: u64,
    preferred_column: Option<usize>,
    interaction: EditorInteraction,
}

#[cfg(test)]
impl From<String> for EditorBuffer {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

#[cfg(test)]
impl From<&str> for EditorBuffer {
    fn from(text: &str) -> Self {
        Self::new(text.into())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EditorInteraction {
    anchor: Option<usize>,
    typing: bool,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditSnapshot {
    text: String,
    cursor: usize,
}

impl EditorBuffer {
    pub(crate) fn new(text: String) -> Self {
        Self {
            cursor: text.len(),
            text,
            ..Self::default()
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(crate) fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.interaction
            .anchor
            .filter(|anchor| *anchor != self.cursor)
            .map(|anchor| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub(crate) fn break_interaction(&mut self) {
        self.interaction.typing = false;
        self.preferred_column = None;
    }

    pub(crate) fn checkpoint(&mut self) {
        self.interaction.typing = false;
        self.interaction.redo.clear();
        Self::push(&mut self.interaction.undo, &self.text, self.cursor);
        self.interaction.anchor = None;
    }

    pub(crate) fn replace_user(&mut self, text: String, cursor: usize) {
        let cursor = floor_grapheme_boundary(&text, cursor);
        if self.text != text {
            self.replace_document(text, cursor);
        } else {
            self.cursor = cursor;
            self.preferred_column = None;
        }
    }

    /// Reconcile text supplied by durable state without making the replaced
    /// local buffer an undo target. Undo must never remove an authoritative
    /// prefix that arrived during hydration.
    pub(crate) fn reconcile(&mut self, text: String, cursor: usize) {
        let revision = if self.text == text {
            self.revision
        } else {
            self.revision.wrapping_add(1)
        };
        self.reset_external(text, cursor, revision);
    }

    /// Replace non-user state while preserving the caller's revision meaning.
    /// This deliberately clears selection, typing state, and both histories.
    pub(crate) fn reset_external(&mut self, text: String, cursor: usize, revision: u64) {
        let cursor = floor_grapheme_boundary(&text, cursor);
        *self = Self {
            text,
            cursor,
            revision,
            ..Self::default()
        };
    }

    pub(crate) fn clear(&mut self) -> bool {
        if self.text.is_empty() {
            return false;
        }
        self.checkpoint();
        self.text.clear();
        self.cursor = 0;
        self.revision = self.revision.wrapping_add(1);
        self.preferred_column = None;
        true
    }

    pub(crate) fn apply(&mut self, command: EditCommand) -> bool {
        if !matches!(
            command,
            EditCommand::Move {
                cursor: CursorMove::Up { .. } | CursorMove::Down { .. },
                ..
            }
        ) {
            self.preferred_column = None;
        }
        match command {
            EditCommand::Insert { text, typing } => self.insert(&text, typing),
            EditCommand::Replace { text } => {
                let cursor = text.len();
                self.replace_document(text, cursor);
                true
            }
            EditCommand::DeleteBefore => self.delete_before(),
            EditCommand::DeleteAfter => self.delete_after(),
            EditCommand::Move { cursor, select } => {
                self.move_cursor(cursor, select);
                false
            }
            EditCommand::Point { byte, extend } => {
                if extend {
                    self.extend_pointer_selection(byte);
                } else {
                    self.begin_pointer_selection(byte);
                }
                false
            }
            EditCommand::Clear => self.clear(),
            EditCommand::Undo => self.undo(false),
            EditCommand::Redo => self.undo(true),
        }
    }

    fn replace_document(&mut self, text: String, cursor: usize) {
        self.checkpoint();
        self.text = text;
        self.cursor = cursor;
        self.revision = self.revision.wrapping_add(1);
        self.preferred_column = None;
    }

    pub(crate) fn history_bytes(&self) -> usize {
        self.interaction
            .undo
            .iter()
            .chain(&self.interaction.redo)
            .map(|item| item.text.len())
            .sum()
    }

    pub(crate) fn evict_oldest(&mut self) -> bool {
        if !self.interaction.undo.is_empty() {
            self.interaction.undo.remove(0);
            true
        } else if !self.interaction.redo.is_empty() {
            self.interaction.redo.remove(0);
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

    fn undo(&mut self, redo: bool) -> bool {
        self.interaction.typing = false;
        let (source, target) = if redo {
            (&mut self.interaction.redo, &mut self.interaction.undo)
        } else {
            (&mut self.interaction.undo, &mut self.interaction.redo)
        };
        let Some(previous) = source.pop() else {
            return false;
        };
        Self::push(target, &self.text, self.cursor);
        self.text = previous.text;
        self.cursor = previous.cursor;
        self.revision = self.revision.wrapping_add(1);
        self.interaction.anchor = None;
        true
    }

    fn insert(&mut self, value: &str, typing: bool) -> bool {
        if value.is_empty() {
            return false;
        }
        let selection = self.selection();
        if !typing || !self.interaction.typing || selection.is_some() {
            self.checkpoint();
        }
        self.interaction.typing = typing;
        if let Some(range) = selection {
            self.cursor = range.start;
            self.text.replace_range(range, "");
        }
        self.text.insert_str(self.cursor, value);
        self.cursor = ceil_grapheme_boundary(&self.text, self.cursor + value.len());
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn delete_before(&mut self) -> bool {
        if let Some(range) = self.selection() {
            self.checkpoint();
            self.cursor = range.start;
            self.text.replace_range(range, "");
            self.cursor = ceil_grapheme_boundary(&self.text, self.cursor);
        } else if let Some((index, _)) = self.text[..self.cursor].grapheme_indices(true).next_back()
        {
            self.checkpoint();
            self.text.drain(index..self.cursor);
            self.cursor = ceil_grapheme_boundary(&self.text, index);
        } else {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn delete_after(&mut self) -> bool {
        if let Some(range) = self.selection() {
            self.checkpoint();
            self.cursor = range.start;
            self.text.replace_range(range, "");
            self.cursor = ceil_grapheme_boundary(&self.text, self.cursor);
        } else if let Some(len) = self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map(str::len)
        {
            self.checkpoint();
            self.text.drain(self.cursor..self.cursor + len);
            self.cursor = ceil_grapheme_boundary(&self.text, self.cursor);
        } else {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn move_cursor(&mut self, movement: CursorMove, select: bool) {
        let selection = self.selection();
        if select {
            self.interaction.anchor.get_or_insert(self.cursor);
        } else {
            self.interaction.anchor = None;
            if let Some(range) = selection {
                match movement {
                    CursorMove::Left => {
                        self.cursor = range.start;
                        return;
                    }
                    CursorMove::Right => {
                        self.cursor = range.end;
                        return;
                    }
                    _ => {}
                }
            }
        }
        self.cursor = match movement {
            CursorMove::Left => moved_cursor(&self.text, self.cursor, -1),
            CursorMove::Right => moved_cursor(&self.text, self.cursor, 1),
            CursorMove::Up { width } => vertical_cursor(
                &self.text,
                self.cursor,
                width,
                false,
                &mut self.preferred_column,
            ),
            CursorMove::Down { width } => vertical_cursor(
                &self.text,
                self.cursor,
                width,
                true,
                &mut self.preferred_column,
            ),
            CursorMove::WordLeft => word_cursor(&self.text, self.cursor, false),
            CursorMove::WordRight => word_cursor(&self.text, self.cursor, true),
            CursorMove::LineStart => line_edge(&self.text, self.cursor, false),
            CursorMove::LineEnd => line_edge(&self.text, self.cursor, true),
        };
    }

    fn extend_pointer_selection(&mut self, byte: usize) {
        self.interaction.anchor.get_or_insert(self.cursor);
        self.cursor = floor_grapheme_boundary(&self.text, byte);
    }

    fn begin_pointer_selection(&mut self, byte: usize) {
        self.cursor = floor_grapheme_boundary(&self.text, byte);
        self.interaction.anchor = Some(self.cursor);
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
        text[cursor..]
            .grapheme_indices(true)
            .find(|(_, grapheme)| matches!(*grapheme, "\r" | "\n" | "\r\n"))
            .map_or(text.len(), |(offset, _)| cursor + offset)
    } else {
        text[..cursor]
            .grapheme_indices(true)
            .rev()
            .find(|(_, grapheme)| matches!(*grapheme, "\r" | "\n" | "\r\n"))
            .map_or(0, |(offset, grapheme)| offset + grapheme.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_is_one_undo_step_and_never_splits_a_cluster() {
        let mut buffer = EditorBuffer::default();
        buffer.apply(EditCommand::Insert {
            text: "e".into(),
            typing: true,
        });
        buffer.apply(EditCommand::Insert {
            text: "\u{301}".into(),
            typing: true,
        });
        buffer.apply(EditCommand::Insert {
            text: "🙂".into(),
            typing: true,
        });
        buffer.apply(EditCommand::DeleteBefore);
        buffer.apply(EditCommand::Undo);
        assert_eq!(buffer.text, "e\u{301}🙂");
        assert_eq!(buffer.cursor, buffer.text.len());
        buffer.apply(EditCommand::Undo);
        assert_eq!(buffer.text, "");
        assert_eq!(buffer.cursor, 0);
    }

    #[test]
    fn selection_replacement_uses_grapheme_boundaries() {
        let mut buffer = EditorBuffer::new("A👨‍👩‍👧‍👦B".into());
        buffer.apply(EditCommand::Point {
            byte: 2,
            extend: false,
        });
        buffer.apply(EditCommand::Point {
            byte: "A👨‍👩‍👧‍👦".len(),
            extend: true,
        });
        buffer.apply(EditCommand::Insert {
            text: "中".into(),
            typing: false,
        });
        assert_eq!(buffer.text, "A中B");
        assert_eq!(buffer.cursor, "A中".len());
        assert_eq!(buffer.revision, 1);
    }

    #[test]
    fn word_navigation_handles_unicode_words_and_punctuation() {
        let value = "one, café two";
        assert_eq!(word_cursor(value, 0, true), 3);
        assert_eq!(word_cursor(value, 3, true), 10);
        assert_eq!(word_cursor(value, 10, false), 5);
    }

    #[test]
    fn crlf_line_edges_never_split_the_newline_cluster() {
        let mut buffer = EditorBuffer::new("first\r\nsecond".into());
        buffer.apply(EditCommand::Move {
            cursor: CursorMove::LineStart,
            select: false,
        });
        assert_eq!(buffer.cursor(), "first\r\n".len());
        buffer.apply(EditCommand::Move {
            cursor: CursorMove::LineEnd,
            select: false,
        });
        assert_eq!(buffer.cursor(), buffer.text().len());
        buffer.apply(EditCommand::Move {
            cursor: CursorMove::LineStart,
            select: false,
        });
        buffer.apply(EditCommand::DeleteBefore);
        assert_eq!(buffer.text(), "firstsecond");
        buffer.apply(EditCommand::Insert {
            text: "!".into(),
            typing: false,
        });
        assert_eq!(buffer.text(), "first!second");
    }

    #[test]
    fn plain_left_and_right_collapse_a_selection_to_their_nearest_edge() {
        let mut buffer = EditorBuffer::new("abc".into());
        for movement in [CursorMove::Left, CursorMove::Right] {
            buffer.apply(EditCommand::Point {
                byte: 0,
                extend: false,
            });
            buffer.apply(EditCommand::Point {
                byte: 2,
                extend: true,
            });
            buffer.apply(EditCommand::Move {
                cursor: movement,
                select: false,
            });
            assert_eq!(
                buffer.cursor(),
                if movement == CursorMove::Left { 0 } else { 2 }
            );
            assert!(buffer.selection().is_none());
        }
    }

    #[test]
    fn vertical_column_survives_a_run_and_resets_at_an_action_boundary() {
        let text = "12345\nx\n12345";
        let mut continuous = EditorBuffer::new(text.into());
        continuous.reset_external(text.into(), 5, 0);
        continuous.apply(EditCommand::Move {
            cursor: CursorMove::Down { width: 20 },
            select: false,
        });
        continuous.apply(EditCommand::Move {
            cursor: CursorMove::Down { width: 20 },
            select: false,
        });
        assert_eq!(continuous.cursor(), text.len());

        let mut interrupted = EditorBuffer::new(text.into());
        interrupted.reset_external(text.into(), 5, 0);
        interrupted.apply(EditCommand::Move {
            cursor: CursorMove::Down { width: 20 },
            select: false,
        });
        interrupted.break_interaction();
        interrupted.apply(EditCommand::Move {
            cursor: CursorMove::Down { width: 20 },
            select: false,
        });
        assert_eq!(interrupted.cursor(), "12345\nx\n1".len());
    }
}
