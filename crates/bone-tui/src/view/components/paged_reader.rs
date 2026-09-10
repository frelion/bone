use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

use crate::state::TextLayoutCache;

/// Builds only the wrapped lines that can be displayed. The source remains in
/// the bounded frontend cache; skipped and trailing text are never copied into
/// Ratatui's per-frame `Text` tree.
pub(in crate::view) fn visible_lines(
    source: &str,
    scroll: usize,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let wanted = usize::from(height).saturating_add(1);
    let end_line = scroll.saturating_add(wanted);
    let mut logical_line = 0usize;
    let mut column = 0usize;
    let mut visible = Vec::with_capacity(wanted);
    let mut current = String::new();

    let finish_line =
        |logical_line: &mut usize, current: &mut String, visible: &mut Vec<Line<'static>>| {
            if *logical_line >= scroll && *logical_line < end_line {
                visible.push(Line::raw(std::mem::take(current)));
            } else {
                current.clear();
            }
            *logical_line = (*logical_line).saturating_add(1);
        };

    for character in source.chars() {
        if logical_line >= end_line {
            break;
        }
        if character == '\n' {
            finish_line(&mut logical_line, &mut current, &mut visible);
            column = 0;
            continue;
        }
        if character == '\t' {
            let spaces = 4usize.saturating_sub(column % 4);
            for _ in 0..spaces {
                if column == width {
                    finish_line(&mut logical_line, &mut current, &mut visible);
                    column = 0;
                }
                if logical_line >= scroll && logical_line < end_line {
                    current.push(' ');
                }
                column += 1;
            }
            continue;
        }
        if character.is_control()
            || matches!(
                character as u32,
                0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
            )
        {
            continue;
        }
        let character_width = character.width().unwrap_or(0);
        if character_width > 0 && column > 0 && column.saturating_add(character_width) > width {
            finish_line(&mut logical_line, &mut current, &mut visible);
            column = 0;
        }
        if logical_line >= scroll && logical_line < end_line {
            current.push(character);
        }
        column = column.saturating_add(character_width);
    }
    if logical_line < end_line && (!current.is_empty() || source.is_empty() || column == 0) {
        finish_line(&mut logical_line, &mut current, &mut visible);
    }
    visible.truncate(usize::from(height));
    visible
}

/// Indexed variant for pageable bodies. Repeated deep scrolling is O(viewport)
/// after one width-specific pass instead of rescanning the skipped prefix.
pub(in crate::view) fn indexed_visible_lines(
    source: &str,
    cache: &std::cell::RefCell<TextLayoutCache>,
    scroll: usize,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    {
        let mut cache = cache.borrow_mut();
        if cache.source_len != source.len() || cache.width != width {
            cache.source_len = source.len();
            cache.width = width;
            cache.lines.clear();
            index_lines(source, usize::from(width), &mut cache.lines);
        }
    }
    let cache = cache.borrow();
    cache
        .lines
        .iter()
        .skip(scroll)
        .take(usize::from(height))
        .map(|&(start, end)| Line::raw(super::super::sanitize_external(&source[start..end])))
        .collect()
}

fn index_lines(source: &str, width: usize, output: &mut Vec<(usize, usize)>) {
    let mut start = 0usize;
    let mut column = 0usize;
    for (index, character) in source.char_indices() {
        if character == '\n' {
            output.push((start, index));
            start = index + character.len_utf8();
            column = 0;
            continue;
        }
        if character.is_control()
            || matches!(character as u32, 0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
        {
            continue;
        }
        let character_width = character.width().unwrap_or(0);
        if character_width > 0 && column > 0 && column.saturating_add(character_width) > width {
            output.push((start, index));
            start = index;
            column = 0;
        }
        column = column.saturating_add(character_width);
    }
    output.push((start, source.len()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_bounded_by_viewport_even_for_a_large_source() {
        let source = "界".repeat(512 * 1024);
        let lines = visible_lines(&source, 20_000, 40, 12);
        assert_eq!(lines.len(), 12);
        assert!(lines.iter().map(|line| line.width()).sum::<usize>() <= 40 * 12);
    }

    #[test]
    fn terminal_controls_are_removed_from_visible_text() {
        let lines = visible_lines("ok\u{1b}]8;;bad\u{7}link\u{1b}\\\u{202e}X", 0, 80, 2);
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, "ok]8;;badlink\\X");
    }

    #[test]
    fn indexed_reader_reuses_its_line_index_for_deep_scrolls() {
        let source = (0..40_000)
            .map(|index| format!("line {index:05} 界界界\n"))
            .collect::<String>();
        let cache = std::cell::RefCell::new(TextLayoutCache::default());

        let first = indexed_visible_lines(&source, &cache, 30_000, 18, 8);
        let indexed_lines = cache.borrow().lines.len();
        let second = indexed_visible_lines(&source, &cache, 30_001, 18, 8);

        assert_eq!(first.len(), 8);
        assert_eq!(second.len(), 8);
        assert_eq!(cache.borrow().lines.len(), indexed_lines);
        assert!(indexed_lines >= 40_000);
    }

    #[test]
    fn one_mib_chinese_deep_scroll_keeps_frame_output_viewport_bounded() {
        let unit = "中文\n";
        let repetitions = (1024usize * 1024).div_ceil(unit.len());
        let source = unit.repeat(repetitions);
        let cache = std::cell::RefCell::new(TextLayoutCache::default());

        let first = indexed_visible_lines(&source, &cache, 100_000, 32, 12);
        let indexed_lines = cache.borrow().lines.len();
        let second = indexed_visible_lines(&source, &cache, 100_001, 32, 12);

        assert_eq!(first.len(), 12);
        assert_eq!(second.len(), 12);
        assert!(first.iter().map(Line::width).sum::<usize>() <= 32 * 12);
        assert!(second.iter().map(Line::width).sum::<usize>() <= 32 * 12);
        assert_eq!(cache.borrow().lines.len(), indexed_lines);
        assert!(indexed_lines > 65_535);
        assert!(source.len() >= 1024 * 1024);
    }
}
