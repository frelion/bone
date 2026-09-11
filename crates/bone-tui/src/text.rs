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
