//! Text selection addresses source bytes, never terminal-buffer cells.
use std::{ops::Range, sync::Arc};

use bone_app::SessionId;
use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::state::reader::ReaderSource;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CopySource {
    Overlay,
    Transcript(SessionId),
    Composer {
        session: Option<SessionId>,
        question: Option<bone_app::QuestionId>,
    },
    Title(SessionId),
    Details {
        session: SessionId,
        source: ReaderSource,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TextPoint {
    pub source: CopySource,
    pub item: u64,
    pub byte: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectableText {
    pub source: CopySource,
    pub item: u64,
    pub text: Arc<str>,
    pub rows: Vec<TextRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TextRow {
    pub area: Rect,
    /// Columns relative to the row and grapheme-aligned source byte boundaries.
    pub boundaries: Vec<(u16, usize)>,
}

impl TextRow {
    pub fn new(area: Rect, text: &str, range: Range<usize>) -> Self {
        let mut boundaries = vec![(0, range.start)];
        let mut column = 0;
        for (offset, grapheme) in text[range.clone()].grapheme_indices(true) {
            if grapheme.contains('\n') {
                break;
            }
            let displayed = crate::text::display_grapheme(grapheme);
            let width = displayed.width() as u16;
            if width == 0 {
                if let Some((_, byte)) = boundaries.last_mut() {
                    *byte = range.start + offset + grapheme.len();
                }
                continue;
            }
            if column >= area.width {
                break;
            }
            column = column.saturating_add(width).min(area.width);
            boundaries.push((column, range.start + offset + grapheme.len()));
        }
        Self { area, boundaries }
    }

    pub fn byte_at(&self, x: u16) -> usize {
        let column = x.saturating_sub(self.area.x).min(self.area.width);
        self.boundaries
            .iter()
            .rev()
            .find(|(x, _)| *x <= column)
            .unwrap()
            .1
    }

    pub fn selected_cells(&self, range: Range<usize>) -> impl Iterator<Item = Rect> + '_ {
        self.boundaries.windows(2).filter_map(move |pair| {
            let [(start, byte), (end, _)] = pair else {
                unreachable!()
            };
            range
                .contains(byte)
                .then(|| Rect::new(self.area.x + start, self.area.y, end - start, 1))
        })
    }
}

impl SelectableText {
    pub fn point_at(&self, x: u16, y: u16) -> Option<TextPoint> {
        let row = self
            .rows
            .iter()
            .find(|row| row.area.contains((x, y).into()))?;
        Some(TextPoint {
            source: self.source,
            item: self.item,
            byte: row.byte_at(x),
        })
    }
}

/// Keep one packed offset per visual row. Both rendering and hit testing use
/// these source boundaries; a soft wrap never inserts text into the document.
pub(crate) fn source_starts(value: &str, width: usize) -> Vec<u32> {
    let width = width.max(1);
    let mut starts = vec![0];
    let mut column = 0;
    for (byte, grapheme) in value.grapheme_indices(true) {
        if matches!(grapheme, "\n" | "\r\n") {
            starts.push(
                u32::try_from(byte + grapheme.len()).expect("text exceeds source offset limit"),
            );
            column = 0;
            continue;
        }
        let cells = crate::text::display_grapheme(grapheme).width();
        if column > 0 && column + cells > width {
            starts.push(u32::try_from(byte).expect("text exceeds source offset limit"));
            column = 0;
        }
        column += cells;
    }
    starts
}

pub(crate) fn row_range(value: &str, starts: &[u32], row: usize) -> Range<usize> {
    let start = starts[row] as usize;
    let mut end = starts
        .get(row + 1)
        .map_or(value.len(), |byte| *byte as usize);
    if value[start..end].ends_with('\n') {
        end -= 1;
        if value[start..end].ends_with('\r') {
            end -= 1;
        }
    }
    start..end
}

pub(crate) fn display_text(value: &str) -> String {
    value
        .graphemes(true)
        .map(crate::text::display_grapheme)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_wraps_preserve_original_whitespace_and_hard_newlines() {
        let source = "  abcdef\n\t界e\u{301}";
        let starts = source_starts(source, 5);
        let ranges: Vec<_> = (0..starts.len())
            .map(|row| row_range(source, &starts, row))
            .collect();
        assert_eq!(
            ranges
                .iter()
                .map(|r| &source[r.clone()])
                .collect::<Vec<_>>(),
            ["  abc", "def", "\t", "界e\u{301}"]
        );
        assert_eq!(&source[ranges[0].start..ranges.last().unwrap().end], source);
    }

    #[test]
    fn hit_boundaries_preserve_wide_combining_and_tab_graphemes() {
        let source = "界e\u{301}\tx";
        let row = TextRow::new(Rect::new(4, 3, 12, 1), source, 0..source.len());
        assert_eq!(row.byte_at(4), 0);
        assert_eq!(row.byte_at(5), 0);
        assert_eq!(row.byte_at(6), "界".len());
        assert_eq!(row.byte_at(9), "界e\u{301}".len());
        assert_eq!(row.byte_at(11), "界e\u{301}\t".len());
        assert_eq!(
            row.selected_cells(0.."界".len()).collect::<Vec<_>>(),
            [Rect::new(4, 3, 2, 1)]
        );
    }
}
