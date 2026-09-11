use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn sanitize_external(value: &str) -> String {
    value
        .chars()
        .filter(|c| matches!(c, '\n' | '\t') || display_character(*c))
        .collect()
}
fn display_character(c: char) -> bool {
    !c.is_control()
        && !matches!(c as u32, 0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
}
pub(crate) fn display_grapheme(value: &str) -> Cow<'_, str> {
    if value == "\t" {
        Cow::Borrowed("    ")
    } else if value.chars().all(display_character) {
        Cow::Borrowed(value)
    } else {
        Cow::Owned(sanitize_external(value))
    }
}

pub(crate) fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let clean = sanitize_external(value).replace('\t', "    ");
    let mut lines = vec![String::new()];
    let mut column = 0;
    for grapheme in clean.graphemes(true) {
        if grapheme == "\n" {
            lines.push(String::new());
            column = 0;
            continue;
        }
        let cells = UnicodeWidthStr::width(grapheme);
        if column > 0 && column + cells > width {
            lines.push(String::new());
            column = 0;
        }
        lines.last_mut().unwrap().push_str(grapheme);
        column += cells;
    }
    lines
}

// Byte offsets, displayed rows and cell columns share one layout so movement
// and rendering agree at soft wraps, wide characters and combining clusters.
struct EditorLayout {
    lines: Vec<String>,
    positions: Vec<(usize, usize, usize)>,
}
fn editor_layout(value: &str, width: u16) -> EditorLayout {
    let width = usize::from(width.max(1));
    let mut lines = vec![String::new()];
    let mut positions = vec![(0, 0, 0)];
    let (mut row, mut column) = (0, 0);
    for (offset, grapheme) in value.grapheme_indices(true) {
        if matches!(grapheme, "\n" | "\r\n") {
            row += 1;
            column = 0;
            lines.push(String::new());
        } else {
            let displayed = display_grapheme(grapheme);
            let cells = UnicodeWidthStr::width(displayed.as_ref());
            if column > 0 && column + cells > width {
                row += 1;
                column = 0;
                lines.push(String::new());
                *positions.last_mut().unwrap() = (offset, row, column);
            }
            lines[row].push_str(&displayed);
            column += cells;
        }
        positions.push((offset + grapheme.len(), row, column));
    }
    if column >= width {
        row += 1;
        lines.push(String::new());
        *positions.last_mut().unwrap() = (value.len(), row, 0);
    }
    EditorLayout { lines, positions }
}
pub(crate) fn editor_rows(value: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let (mut rows, mut column) = (1u16, 0usize);
    for grapheme in value.graphemes(true) {
        if matches!(grapheme, "\n" | "\r\n") {
            rows = rows.saturating_add(1);
            column = 0;
        } else {
            let cells = UnicodeWidthStr::width(display_grapheme(grapheme).as_ref());
            if column > 0 && column + cells > width {
                rows = rows.saturating_add(1);
                column = 0;
            }
            column += cells;
        }
        // Sizing only needs to know whether the six-line viewport is full.
        if rows > 6 {
            return rows;
        }
    }
    rows.saturating_add(u16::from(column >= width))
}
pub(crate) fn vertical_cursor(
    value: &str,
    cursor: usize,
    width: u16,
    down: bool,
    preferred: &mut Option<usize>,
) -> usize {
    let layout = editor_layout(value, width);
    let (_, row, column) = layout
        .positions
        .iter()
        .rev()
        .find(|(byte, _, _)| *byte <= cursor)
        .copied()
        .unwrap();
    let column = *preferred.get_or_insert(column);
    let target = if down {
        (row + 1).min(layout.lines.len() - 1)
    } else {
        row.saturating_sub(1)
    };
    layout
        .positions
        .iter()
        .filter(|(_, r, _)| *r == target)
        .min_by_key(|(_, _, c)| c.abs_diff(column))
        .map_or(cursor, |(byte, _, _)| *byte)
}
#[cfg(test)]
pub(crate) fn editor_viewport(
    value: &str,
    cursor: usize,
    width: u16,
    height: u16,
) -> (String, u16, u16) {
    let layout = editor_layout(value, width);
    let (_, row, column) = layout
        .positions
        .iter()
        .rev()
        .find(|(byte, _, _)| *byte <= cursor)
        .copied()
        .unwrap();
    let height = usize::from(height.max(1));
    let start = (row + 1).saturating_sub(height);
    let end = (start + height).min(layout.lines.len());
    (
        layout.lines[start..end].join("\n"),
        column.min(usize::from(width.saturating_sub(1))) as u16,
        (row - start) as u16,
    )
}

/// Resolve a visible cell to an insertion boundary using the same soft wraps
/// and scroll origin as the rendered editor.
#[cfg(test)]
pub(crate) fn cursor_at_cell(
    value: &str,
    cursor: usize,
    width: u16,
    height: u16,
    x: u16,
    y: u16,
) -> usize {
    let layout = editor_layout(value, width);
    let row = layout
        .positions
        .iter()
        .rev()
        .find(|(byte, _, _)| *byte <= cursor)
        .unwrap()
        .1;
    let start = (row + 1).saturating_sub(usize::from(height.max(1)));
    let target = (start + usize::from(y)).min(layout.lines.len() - 1);
    layout
        .positions
        .iter()
        .filter(|(_, row, _)| *row == target)
        .min_by_key(|(_, _, column)| column.abs_diff(usize::from(x)))
        .map_or(value.len(), |(byte, _, _)| *byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mouse_cells_preserve_clusters_and_visible_scroll_origin() {
        assert_eq!(cursor_at_cell("ab中文", 8, 10, 2, 3, 0), 2);
        assert_eq!(cursor_at_cell("ab中文", 8, 10, 2, 4, 0), 5);
        assert_eq!(cursor_at_cell("e\u{301}x", 4, 8, 2, 1, 0), 3);
        assert_eq!(cursor_at_cell("one\ntwo\nthree", 13, 20, 2, 1, 0), 5);
        assert_eq!(cursor_at_cell("one\ntwo\nthree", 13, 20, 2, 19, 1), 13);
        assert_eq!(cursor_at_cell("ab中文🙂z", 13, 5, 2, 2, 0), "ab中文".len());
    }
    #[test]
    fn editor_cursor_tracks_soft_wrap_and_cjk_cells() {
        let (text, x, y) = editor_viewport("ab中文🙂z", "ab中文🙂".len(), 5, 4);
        assert_eq!(text, "ab中\n文🙂z\n");
        assert_eq!((x, y), (4, 1));
    }
    #[test]
    fn editor_cursor_tracks_explicit_lines_and_scrolled_viewport() {
        assert_eq!(
            editor_viewport("one\ntwo\nthree", 7, 20, 2),
            ("one\ntwo".into(), 3, 1)
        );
    }
    #[test]
    fn editor_cursor_never_splits_combining_grapheme() {
        let (_, x, y) = editor_viewport("e\u{301}x", "e\u{301}".len(), 8, 2);
        assert_eq!((x, y), (1, 0));
    }
    #[test]
    fn full_line_end_has_its_own_insertion_cell() {
        assert_eq!(editor_viewport("abcd", 4, 4, 2), ("abcd\n".into(), 0, 1));
    }
    #[test]
    fn vertical_movement_uses_visual_rows_and_grapheme_boundaries() {
        let next = vertical_cursor("ab中文🙂z", 2, 5, true, &mut None);
        assert_eq!(next, "ab中文".len());
        assert_eq!(vertical_cursor("ab中文🙂z", next, 5, false, &mut None), 2);
    }
    #[test]
    fn windows_line_endings_share_render_and_cursor_geometry() {
        assert_eq!(editor_rows("a\r\nb", 20), 2);
        assert_eq!(editor_viewport("a\r\nb", 4, 20, 3), ("a\nb".into(), 1, 1));
    }
    #[test]
    fn short_lines_do_not_erase_the_preferred_column() {
        let value = "123456\nx\n123456";
        let mut preferred = None;
        let cursor = vertical_cursor(value, 5, 20, true, &mut preferred);
        assert_eq!(cursor, 8);
        assert_eq!(vertical_cursor(value, cursor, 20, true, &mut preferred), 14);
    }
}

/// Per-draft editing history. Viewport is a rendering cache, never persisted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditorState {
    pub anchor: Option<usize>,
    pub viewport: std::cell::Cell<usize>,
    pub typing: bool,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct EditSnapshot {
    text: String,
    cursor: usize,
}
impl EditorState {
    pub fn history_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|item| item.text.len())
            .sum()
    }
    pub fn evict_oldest(&mut self) -> bool {
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
    pub fn selection(&self, cursor: usize) -> Option<std::ops::Range<usize>> {
        self.anchor
            .filter(|anchor| *anchor != cursor)
            .map(|anchor| anchor.min(cursor)..anchor.max(cursor))
    }
    pub fn checkpoint(&mut self, text: &str, cursor: usize) {
        self.typing = false;
        self.redo.clear();
        Self::push(&mut self.undo, text, cursor);
        self.anchor = None;
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
    pub fn undo(&mut self, text: &mut String, cursor: &mut usize, revision: &mut u64, redo: bool) {
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

pub(crate) fn word_cursor(value: &str, cursor: usize, right: bool) -> usize {
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

pub(crate) fn stable_editor_viewport(
    value: &str,
    cursor: usize,
    width: u16,
    height: u16,
    origin: &std::cell::Cell<usize>,
) -> (String, u16, u16) {
    let layout = editor_layout(value, width);
    let (_, row, column) = layout
        .positions
        .iter()
        .rev()
        .find(|(byte, _, _)| *byte <= cursor)
        .copied()
        .unwrap();
    let height = usize::from(height.max(1));
    let mut start = origin.get().min(layout.lines.len().saturating_sub(height));
    if row < start {
        start = row;
    }
    if row >= start + height {
        start = row + 1 - height;
    }
    origin.set(start);
    (
        layout.lines[start..(start + height).min(layout.lines.len())].join("\n"),
        column.min(usize::from(width.saturating_sub(1))) as u16,
        (row - start) as u16,
    )
}

pub(crate) fn cursor_at_origin(value: &str, width: u16, origin: usize, x: u16, y: u16) -> usize {
    let layout = editor_layout(value, width);
    let target = (origin + usize::from(y)).min(layout.lines.len() - 1);
    layout
        .positions
        .iter()
        .filter(|(_, row, _)| *row == target)
        .min_by_key(|(_, _, column)| column.abs_diff(usize::from(x)))
        .map_or(value.len(), |(byte, _, _)| *byte)
}

pub(crate) fn selection_cells(
    value: &str,
    width: u16,
    height: u16,
    origin: usize,
    selection: std::ops::Range<usize>,
) -> Vec<(u16, u16, u16)> {
    let layout = editor_layout(value, width);
    value
        .grapheme_indices(true)
        .filter(|(byte, _)| selection.contains(byte))
        .filter_map(|(byte, grapheme)| {
            let index = layout
                .positions
                .binary_search_by_key(&byte, |(offset, _, _)| *offset)
                .ok()?;
            let (_, row, column) = &layout.positions[index];
            if *row < origin || *row >= origin + usize::from(height) {
                return None;
            }
            let cells = UnicodeWidthStr::width(display_grapheme(grapheme).as_ref()).max(1);
            Some((
                *column as u16,
                (*row - origin) as u16,
                (cells as u16).min(width.saturating_sub(*column as u16)),
            ))
        })
        .collect()
}

#[cfg(test)]
mod editing_tests {
    use super::*;
    #[test]
    fn clicking_scrolled_top_row_preserves_viewport_and_selection_geometry() {
        let value = "one\ntwo\nthree\nfour\nfive";
        let origin = std::cell::Cell::new(0);
        let first = stable_editor_viewport(value, value.len(), 20, 3, &origin);
        assert_eq!(first.0, "three\nfour\nfive");
        let cursor = cursor_at_origin(value, 20, origin.get(), 2, 0);
        let second = stable_editor_viewport(value, cursor, 20, 3, &origin);
        assert_eq!(second.0, first.0);
        assert_eq!((second.1, second.2), (2, 0));
        assert_eq!(
            selection_cells("中e\u{301}🙂", 8, 2, 0, 3..11),
            vec![(2, 0, 1), (3, 0, 2)]
        );
    }
    #[test]
    fn word_navigation_handles_unicode_words_and_punctuation() {
        let value = "one, café two";
        assert_eq!(word_cursor(value, 0, true), 3);
        assert_eq!(word_cursor(value, 3, true), 10);
        assert_eq!(word_cursor(value, 10, false), 5);
    }
}
