use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::text::display_grapheme;

// Byte offsets, displayed rows and cell columns share one layout so movement,
// rendering, selection and pointer hit testing cannot disagree.
struct EditorLayout {
    lines: Vec<String>,
    positions: Vec<(usize, usize, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditorViewport {
    pub(crate) text: String,
    pub(crate) cursor_x: u16,
    pub(crate) cursor_y: u16,
    /// Visual row index in the fully wrapped document.
    pub(crate) row_origin: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SingleLineViewport {
    pub(crate) text: String,
    pub(crate) cursor_x: u16,
    /// Grapheme-aligned UTF-8 byte offset in the source document.
    pub(crate) byte_origin: usize,
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
        // Composer sizing only needs to know when its six-line viewport is full.
        if rows > 6 {
            return rows;
        }
    }
    rows.saturating_add(u16::from(column >= width))
}

pub(super) fn vertical_cursor(
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
    let preferred_column = *preferred.get_or_insert(column);
    let target = if down {
        (row + 1).min(layout.lines.len() - 1)
    } else {
        row.saturating_sub(1)
    };
    layout
        .positions
        .iter()
        .filter(|(_, row, _)| *row == target)
        .min_by_key(|(_, _, column)| column.abs_diff(preferred_column))
        .map_or(cursor, |(byte, _, _)| *byte)
}

pub(crate) fn stable_editor_viewport(
    value: &str,
    cursor: usize,
    width: u16,
    height: u16,
    previous_row_origin: usize,
) -> EditorViewport {
    let layout = editor_layout(value, width);
    let (_, row, column) = layout
        .positions
        .iter()
        .rev()
        .find(|(byte, _, _)| *byte <= cursor)
        .copied()
        .unwrap();
    let height = usize::from(height.max(1));
    let mut start = previous_row_origin.min(layout.lines.len().saturating_sub(height));
    if row < start {
        start = row;
    }
    if row >= start + height {
        start = row + 1 - height;
    }
    EditorViewport {
        text: layout.lines[start..(start + height).min(layout.lines.len())].join("\n"),
        cursor_x: column.min(usize::from(width.saturating_sub(1))) as u16,
        cursor_y: (row - start) as u16,
        row_origin: start,
    }
}

/// Keep a one-line editor's insertion point visible without borrowing the
/// composer's wrapping behavior. `origin` stores a grapheme-aligned byte
/// offset, which also lets pointer input resolve against the same window.
pub(crate) fn single_line_editor_viewport(
    value: &str,
    cursor: usize,
    width: u16,
) -> SingleLineViewport {
    let width = usize::from(width.max(1));
    let cursor = super::floor_grapheme_boundary(value, cursor.min(value.len()));
    let mut start = cursor;
    let mut cursor_column: usize = 0;
    for (offset, grapheme) in value[..cursor].grapheme_indices(true).rev() {
        let cells = UnicodeWidthStr::width(display_grapheme(grapheme).as_ref()).max(1);
        if cursor_column.saturating_add(cells) >= width {
            break;
        }
        cursor_column += cells;
        start = offset;
    }
    let mut shown = String::new();
    let mut used: usize = 0;
    for grapheme in value[start..].graphemes(true) {
        let displayed = display_grapheme(grapheme);
        let cells = UnicodeWidthStr::width(displayed.as_ref()).max(1);
        if used.saturating_add(cells) > width {
            break;
        }
        shown.push_str(&displayed);
        used += cells;
    }
    SingleLineViewport {
        text: shown,
        cursor_x: cursor_column.min(width.saturating_sub(1)) as u16,
        byte_origin: start,
    }
}

pub(crate) fn cursor_at_single_line(value: &str, origin: usize, column: u16) -> usize {
    let origin = super::floor_grapheme_boundary(value, origin.min(value.len()));
    let target = usize::from(column);
    let mut used: usize = 0;
    let mut nearest = origin;
    for (offset, grapheme) in value[origin..].grapheme_indices(true) {
        let cells = UnicodeWidthStr::width(display_grapheme(grapheme).as_ref()).max(1);
        if target < used.saturating_add(cells).saturating_sub(cells / 2) {
            break;
        }
        used += cells;
        nearest = origin + offset + grapheme.len();
        if used > target {
            break;
        }
    }
    nearest
}

/// Selected cells in the exact byte-origin window used by the one-line title
/// editor. Keeping this beside pointer hit testing makes clipping, wide glyphs,
/// emoji and combining clusters use one geometry.
pub(crate) fn single_line_selection_cells(
    value: &str,
    origin: usize,
    width: u16,
    selection: std::ops::Range<usize>,
) -> Vec<(u16, u16)> {
    let origin = super::floor_grapheme_boundary(value, origin.min(value.len()));
    let width = usize::from(width);
    let mut used = 0usize;
    let mut cells = Vec::new();
    for (offset, grapheme) in value[origin..].grapheme_indices(true) {
        let displayed = display_grapheme(grapheme);
        let grapheme_cells = UnicodeWidthStr::width(displayed.as_ref()).max(1);
        if used.saturating_add(grapheme_cells) > width {
            break;
        }
        if selection.contains(&(origin + offset)) {
            cells.push((used as u16, grapheme_cells as u16));
        }
        used += grapheme_cells;
    }
    cells
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
fn editor_viewport(value: &str, cursor: usize, width: u16, height: u16) -> (String, u16, u16) {
    let viewport = stable_editor_viewport(value, cursor, width, height, 0);
    (viewport.text, viewport.cursor_x, viewport.cursor_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_cells_preserve_clusters_and_visible_scroll_origin() {
        assert_eq!(cursor_at_origin("ab中文", 10, 0, 3, 0), 2);
        assert_eq!(cursor_at_origin("ab中文", 10, 0, 4, 0), 5);
        assert_eq!(cursor_at_origin("e\u{301}x", 8, 0, 1, 0), 3);
        assert_eq!(cursor_at_origin("ab中文🙂z", 5, 1, 2, 0), "ab中文".len());
    }

    #[test]
    fn cursor_tracks_soft_wrap_cjk_and_combining_clusters() {
        let (text, x, y) = editor_viewport("ab中文🙂z", "ab中文🙂".len(), 5, 4);
        assert_eq!(text, "ab中\n文🙂z\n");
        assert_eq!((x, y), (4, 1));
        let (_, x, y) = editor_viewport("e\u{301}x", "e\u{301}".len(), 8, 2);
        assert_eq!((x, y), (1, 0));
    }

    #[test]
    fn explicit_lines_and_scrolled_viewport_share_one_geometry() {
        assert_eq!(
            editor_viewport("one\ntwo\nthree", 7, 20, 2),
            ("one\ntwo".into(), 3, 1)
        );
        let value = "one\ntwo\nthree\nfour\nfive";
        let first = stable_editor_viewport(value, value.len(), 20, 3, 0);
        assert_eq!(first.text, "three\nfour\nfive");
        let cursor = cursor_at_origin(value, 20, first.row_origin, 2, 0);
        let second = stable_editor_viewport(value, cursor, 20, 3, first.row_origin);
        assert_eq!(second.text, first.text);
        assert_eq!((second.cursor_x, second.cursor_y), (2, 0));
    }

    #[test]
    fn full_line_end_has_its_own_insertion_cell() {
        assert_eq!(editor_viewport("abcd", 4, 4, 2), ("abcd\n".into(), 0, 1));
    }

    #[test]
    fn one_line_viewport_reserves_a_real_caret_cell_at_exact_width() {
        let viewport = single_line_editor_viewport("abcd", 4, 4);

        assert_eq!(viewport.text, "bcd");
        assert_eq!(viewport.cursor_x, 3);
        assert_eq!(viewport.byte_origin, 1);
        assert_eq!(cursor_at_single_line("abcd", viewport.byte_origin, 0), 1);
        assert_eq!(cursor_at_single_line("abcd", viewport.byte_origin, 3), 4);
    }

    #[test]
    fn one_line_viewport_scrolls_only_at_grapheme_boundaries() {
        let value = "Ae\u{301}👩‍💻Z";
        let cursor = "Ae\u{301}👩‍💻".len();
        let viewport = single_line_editor_viewport(value, cursor, 4);

        assert_eq!(viewport.text, "e\u{301}👩‍💻Z");
        assert_eq!(viewport.cursor_x, 3);
        assert_eq!(viewport.byte_origin, 1);
        assert!(value.is_char_boundary(viewport.byte_origin));
        assert_eq!(
            cursor_at_single_line(value, viewport.byte_origin, viewport.cursor_x),
            cursor
        );

        let cjk = "ab中文🙂z";
        let viewport = single_line_editor_viewport(cjk, cjk.len(), 5);
        assert_eq!(viewport.text, "🙂z");
        assert_eq!(viewport.cursor_x, 3);
    }

    #[test]
    fn one_line_selection_uses_the_same_scrolled_unicode_cells_as_the_caret() {
        let value = "ab中e\u{301}🙂zTAIL";
        let viewport = single_line_editor_viewport(value, value.len(), 8);
        let selected_from = value.find('z').unwrap();

        assert_eq!(viewport.text, "🙂zTAIL");
        assert_eq!(viewport.cursor_x, 7);
        assert_eq!(
            single_line_selection_cells(value, viewport.byte_origin, 8, selected_from..value.len()),
            vec![(2, 1), (3, 1), (4, 1), (5, 1), (6, 1)]
        );
        let emoji = value.find('🙂').unwrap();
        assert_eq!(
            single_line_selection_cells(value, viewport.byte_origin, 8, emoji..value.len()),
            vec![(0, 2), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)]
        );
        assert!(value.is_char_boundary(viewport.byte_origin));
        assert_eq!(
            cursor_at_single_line(value, viewport.byte_origin, 2),
            selected_from
        );
    }

    #[test]
    fn vertical_movement_uses_visual_rows_and_a_stable_column() {
        let mut preferred = None;
        let next = vertical_cursor("ab中文🙂z", 2, 5, true, &mut preferred);
        assert_eq!(next, "ab中文".len());
        assert_eq!(
            vertical_cursor("ab中文🙂z", next, 5, false, &mut preferred),
            2
        );

        let value = "123456\nx\n123456";
        let mut preferred = None;
        let cursor = vertical_cursor(value, 5, 20, true, &mut preferred);
        assert_eq!(cursor, 8);
        assert_eq!(vertical_cursor(value, cursor, 20, true, &mut preferred), 14);
    }

    #[test]
    fn windows_line_endings_share_render_and_cursor_geometry() {
        assert_eq!(editor_rows("a\r\nb", 20), 2);
        assert_eq!(editor_viewport("a\r\nb", 4, 20, 3), ("a\nb".into(), 1, 1));
    }

    #[test]
    fn selection_geometry_uses_display_cells() {
        assert_eq!(
            selection_cells("中e\u{301}🙂", 8, 2, 0, 3..11),
            vec![(2, 0, 1), (3, 0, 2)]
        );
    }
}
