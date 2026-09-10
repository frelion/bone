use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::sanitize_external;

pub(super) fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let clean = sanitize_external(value).replace('\t', "    ");
    let mut lines = vec![String::new()];
    let mut cells = 0usize;
    for grapheme in clean.graphemes(true) {
        if grapheme == "\n" {
            lines.push(String::new());
            cells = 0;
            continue;
        }
        let grapheme_cells = UnicodeWidthStr::width(grapheme);
        if cells > 0 && cells.saturating_add(grapheme_cells) > width {
            lines.push(String::new());
            cells = 0;
        }
        lines
            .last_mut()
            .expect("one line exists")
            .push_str(grapheme);
        cells = cells.saturating_add(grapheme_cells);
    }
    lines
}

/// Wrap inert text without splitting grapheme clusters. The returned cursor is
/// expressed in terminal cells relative to the returned viewport.
pub(super) fn editor_viewport(
    value: &str,
    cursor_byte: usize,
    width: u16,
    height: u16,
) -> (String, u16, u16) {
    let width = usize::from(width.max(1));
    let mut cursor_byte = cursor_byte.min(value.len());
    while !value.is_char_boundary(cursor_byte) {
        cursor_byte = cursor_byte.saturating_sub(1);
    }
    let clean = sanitize_external(value).replace('\t', "    ");
    let cursor_clean = sanitize_external(&value[..cursor_byte]).replace('\t', "    ");
    let cursor_chars = cursor_clean.graphemes(true).count();
    let mut lines = vec![String::new()];
    let mut positions = vec![(0usize, 0usize)];
    let mut row = 0usize;
    let mut column = 0usize;

    for grapheme in clean.graphemes(true) {
        if grapheme == "\n" {
            row += 1;
            column = 0;
            lines.push(String::new());
            positions.push((row, column));
            continue;
        }
        let cells = UnicodeWidthStr::width(grapheme);
        if column > 0 && column.saturating_add(cells) > width {
            row += 1;
            column = 0;
            lines.push(String::new());
        }
        lines[row].push_str(grapheme);
        column = column.saturating_add(cells).min(width);
        positions.push((row, column));
    }

    let (cursor_row, cursor_column) = positions
        .get(cursor_chars)
        .copied()
        .unwrap_or((row, column));
    let height = usize::from(height.max(1));
    let start = cursor_row.saturating_add(1).saturating_sub(height);
    let end = (start + height).min(lines.len());
    (
        lines[start..end].join("\n"),
        cursor_column.min(width.saturating_sub(1)) as u16,
        cursor_row.saturating_sub(start) as u16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_cursor_tracks_soft_wrap_and_cjk_cells() {
        let (text, x, y) = editor_viewport("ab中文🙂z", "ab中文🙂".len(), 5, 4);
        assert_eq!(text, "ab中\n文🙂z");
        assert_eq!((x, y), (4, 1));
    }

    #[test]
    fn editor_cursor_tracks_explicit_lines_and_scrolled_viewport() {
        let (text, x, y) = editor_viewport("one\ntwo\nthree", 7, 20, 2);
        assert_eq!(text, "one\ntwo");
        assert_eq!((x, y), (3, 1));
    }

    #[test]
    fn editor_cursor_never_splits_combining_grapheme() {
        let value = "e\u{301}x";
        let (_, x, y) = editor_viewport(value, "e\u{301}".len(), 8, 2);
        assert_eq!((x, y), (1, 0));
    }
}
