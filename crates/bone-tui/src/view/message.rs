use bone_app::SessionEvent;
use ratatui::style::Stylize;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    ACCENT, ATTENTION, CYAN, DANGER, GREEN, INK, MUTED, PURPLE, USER, primitives::wrap_text,
};

pub(super) fn rows(event: &SessionEvent, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    match event {
        SessionEvent::InputSubmitted { text, .. } => user_rows(text, width),
        SessionEvent::Reply { text, .. } => reply_rows(text, width),
        SessionEvent::QuestionAsked { text, .. } => body_rows(text, width, ATTENTION),
        SessionEvent::ToolFinished { tool, outcome, .. } => {
            let mut lines = vec![compact(
                "›",
                &format!(
                    "{}  {}",
                    tool,
                    if outcome.result.is_ok() {
                        "done"
                    } else {
                        "failed"
                    }
                ),
                if outcome.result.is_ok() {
                    GREEN
                } else {
                    DANGER
                },
            )];
            if let Err(error) = &outcome.result {
                lines.extend(
                    error_preview(&error.message, width)
                        .into_iter()
                        .map(|(_, text)| Line::styled(text, Style::default().fg(DANGER))),
                );
            }
            lines
        }
        SessionEvent::RoutingFailed { message, .. }
        | SessionEvent::InputRejected { message, .. } => vec![compact("·", message, DANGER)],
        SessionEvent::JobFinished { summary, .. } => body_rows(summary, width, INK),
        SessionEvent::AcceptanceRecorded { reason, .. } => vec![compact("·", reason, ACCENT)],
        SessionEvent::Interrupted { .. } => vec![compact("·", "Execution interrupted.", ATTENTION)],
        SessionEvent::InputAccepted { .. } => vec![compact("·", "Request received", MUTED)],
        SessionEvent::InputCancelled { .. } => vec![compact("·", "Request cancelled", MUTED)],
        SessionEvent::InputFinished { outcome, .. } => {
            vec![compact("·", &format!("Request {outcome:?}"), MUTED)]
        }
        SessionEvent::WriteResolved { evidence, .. } => vec![compact("·", evidence, ACCENT)],
        // Runtime lifecycle is transport plumbing, not conversation content.
        SessionEvent::RuntimeStarted { .. }
        | SessionEvent::RuntimeReconfigured { .. }
        | SessionEvent::RuntimeClosed { .. } => Vec::new(),
    }
}

/// Original UTF-8 offsets, one per visual row, including user-message padding.
/// These do not depend on terminal coordinates and survive different wrap widths.
pub(super) fn row_offsets(event: &SessionEvent, width: u16) -> Vec<usize> {
    let width = usize::from(width.max(1));
    match event {
        SessionEvent::InputSubmitted { text, .. } => {
            let mut offsets = vec![0];
            offsets.extend(wrap_offsets(text, width.saturating_sub(6).max(1)));
            offsets.push(text.len());
            offsets
        }
        SessionEvent::Reply { text, .. } => {
            let mut offsets = Vec::new();
            let mut base = 0;
            let mut code = false;
            for line in text.split_inclusive('\n') {
                let source = line.trim_end_matches('\n').trim_end_matches('\r');
                let clean = super::sanitize_external(source);
                if clean.trim_start().starts_with("```") {
                    code = !code;
                    if code && !clean.trim_start().trim_start_matches('`').trim().is_empty() {
                        offsets.push(base);
                    }
                } else {
                    offsets.extend(
                        wrap_offsets(source, width.saturating_sub(4).max(1))
                            .into_iter()
                            .map(|offset| base + offset),
                    );
                }
                base += line.len();
            }
            offsets
        }
        SessionEvent::ToolFinished { outcome, .. } => {
            let mut offsets = vec![0];
            if let Err(error) = &outcome.result {
                // Reserve zero for the tool heading; the preview uses original
                // error byte offsets after that virtual heading byte.
                offsets.extend(
                    error_preview(&error.message, width)
                        .into_iter()
                        .map(|(byte, _)| byte + 1),
                );
            }
            offsets
        }
        SessionEvent::QuestionAsked { text, .. } => wrap_offsets(text, width),
        SessionEvent::JobFinished { summary, .. } => wrap_offsets(summary, width),
        SessionEvent::RuntimeStarted { .. }
        | SessionEvent::RuntimeReconfigured { .. }
        | SessionEvent::RuntimeClosed { .. } => Vec::new(),
        _ => vec![0],
    }
}

// Bound work independently of the full tool error size, including pathological
// zero-width/control-only input. The reader retains the complete original error.
fn error_preview(message: &str, width: usize) -> Vec<(usize, String)> {
    const PREFIX_BYTES: usize = 4096;
    let mut end = message.len().min(PREFIX_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    if end < message.len() {
        // Do not render a partial cluster at the bounded prefix edge.
        end = message[..end]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(byte, _)| byte);
    }
    let prefix = &message[..end];
    let indent = if width >= 4 { "  " } else { "" };
    let content_width = width.saturating_sub(indent.len()).max(1);
    let lines = wrap_text(prefix, content_width);
    let offsets = wrap_offsets(prefix, content_width);
    let mut preview: Vec<_> = lines
        .into_iter()
        .zip(offsets)
        .filter(|(line, _)| !line.trim().is_empty())
        .take(4)
        .collect();
    let truncated = end < message.len() || preview.len() > 3;
    preview.truncate(3);
    if preview.is_empty() {
        preview.push((
            if message.is_empty() {
                "(empty error)".into()
            } else {
                String::new()
            },
            0,
        ));
    }
    if truncated
        || preview
            .last()
            .is_some_and(|(line, _)| UnicodeWidthStr::width(line.as_str()) > content_width)
    {
        let line = &mut preview.last_mut().unwrap().0;
        while UnicodeWidthStr::width(line.as_str()) >= content_width {
            let Some((byte, _)) = line.grapheme_indices(true).next_back() else {
                break;
            };
            line.truncate(byte);
        }
        line.push('…');
    }
    preview
        .into_iter()
        .map(|(mut text, byte)| {
            if UnicodeWidthStr::width(text.as_str()) > content_width {
                while UnicodeWidthStr::width(text.as_str()) >= content_width {
                    let Some((start, _)) = text.grapheme_indices(true).next_back() else {
                        break;
                    };
                    text.truncate(start);
                }
                text.push('…');
            }
            (byte, format!("{indent}{text}"))
        })
        .collect()
}

fn wrap_offsets(value: &str, width: usize) -> Vec<usize> {
    let mut offsets = vec![0];
    let mut column = 0;
    for (byte, grapheme) in value.grapheme_indices(true) {
        if grapheme == "\n" || grapheme == "\r\n" {
            offsets.push(byte + grapheme.len());
            column = 0;
            continue;
        }
        let clean = crate::text::display_grapheme(grapheme);
        // Tabs are expanded before wrapping, so they may span multiple rows.
        for displayed in clean.graphemes(true) {
            let cells = UnicodeWidthStr::width(displayed);
            if column > 0 && column + cells > width {
                offsets.push(byte);
                column = 0;
            }
            column += cells;
        }
    }
    offsets
}

fn user_rows(value: &str, width: usize) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(6).max(1);
    let blank = || Line::styled(" ".repeat(width), Style::default().bg(USER));
    let mut lines = vec![blank()];
    lines.extend(wrap_text(value, content_width).into_iter().map(|content| {
        let used = UnicodeWidthStr::width(content.as_str()).min(content_width);
        Line::from(vec![
            Span::styled(" │  ", Style::default().fg(MUTED)),
            Span::styled(
                format!("{content}{}", " ".repeat(width.saturating_sub(4 + used))),
                Style::default().fg(INK),
            ),
        ])
        .style(Style::default().bg(USER))
    }));
    lines.push(blank());
    lines
}

// Preserve source text and code indentation; fenced code stays on the open canvas.
fn reply_rows(value: &str, width: usize) -> Vec<Line<'static>> {
    let mut code = false;
    let mut rows = Vec::new();
    for source in super::sanitize_external(value).lines() {
        if source.trim_start().starts_with("```") {
            code = !code;
            if code {
                let language = source.trim_start().trim_start_matches('`').trim();
                if !language.is_empty() {
                    rows.push(Line::styled(
                        format!("  {language}"),
                        Style::default().fg(PURPLE),
                    ));
                }
            }
            continue;
        }
        let heading = !code && source.starts_with("# ");
        for text in wrap_text(source, width.saturating_sub(4)) {
            let style = Style::default().fg(if code { CYAN } else { INK });
            rows.push(Line::styled(
                format!("  {text}"),
                if heading { style.bold() } else { style },
            ));
        }
    }
    rows
}

pub(super) fn body_rows(value: &str, width: usize, tone: Color) -> Vec<Line<'static>> {
    wrap_text(value, width)
        .into_iter()
        .map(|line| Line::styled(line, Style::default().fg(tone)))
        .collect()
}

pub(super) fn compact(icon: &str, body: &str, tone: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{icon} "), Style::default().fg(tone)),
        Span::styled(super::single_line_external(body), Style::default().fg(tone)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_app::{InputId, RequestId, RuntimeId};

    #[test]
    fn long_user_message_is_not_capped() {
        let event = SessionEvent::InputSubmitted {
            input: InputId(1),
            request_id: RequestId::new(),
            text: "一二三四五六七八九十".repeat(20),
            reply_to: None,
        };
        assert!(rows(&event, 12).len() > 12);
    }

    #[test]
    fn runtime_lifecycle_does_not_become_a_message() {
        let event = SessionEvent::RuntimeClosed {
            runtime: RuntimeId::new(),
        };
        assert!(rows(&event, 80).is_empty());
    }
}

#[cfg(test)]
mod tool_error_tests {
    use super::*;
    fn failure(message: impl Into<String>) -> SessionEvent {
        let runtime = bone_app::RuntimeId::new();
        SessionEvent::ToolFinished {
            tool: "read".into(),
            call: bone_app::CallRef { runtime, id: 1 },
            job: bone_app::JobRef { runtime, id: 1 },
            outcome: bone_app::ToolOutcome::failed(message),
        }
    }
    #[test]
    fn errors_show_three_safe_unicode_rows_with_matching_anchors() {
        let message = "\u{1b}权限 e\u{301}错误\r\n第二行\t详细信息\n第三行\nTAIL";
        let event = failure(message);
        for width in [1, 4, 12, 40, 160] {
            let lines = rows(&event, width);
            let offsets = row_offsets(&event, width);
            assert!(lines.len() >= 2 && lines.len() <= 4);
            assert_eq!(lines.len(), offsets.len());
            for line in lines.iter().skip(1) {
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(!text.chars().any(char::is_control));
                assert!(UnicodeWidthStr::width(text.as_str()) <= usize::from(width));
                assert!(!text.contains("TAIL"));
            }
            assert_eq!(offsets[0], 0);
            assert!(
                offsets[1..]
                    .iter()
                    .all(|byte| message.is_char_boundary(byte - 1))
            );
            assert!(offsets.windows(2).all(|pair| pair[0] <= pair[1]));
        }
    }
    #[test]
    fn huge_errors_and_pathological_clusters_have_bounded_previews() {
        for message in [
            "failure ".repeat(131072),
            "\u{1b}".repeat(1024 * 1024),
            format!("e{}TAIL", "\u{301}".repeat(512 * 1024)),
        ] {
            let event = failure(message);
            let lines = rows(&event, 40);
            assert!(lines.len() <= 4);
            let offsets = row_offsets(&event, 40);
            assert_eq!(lines.len(), offsets.len());
            assert!(offsets.into_iter().all(|offset| offset <= 4097));
            assert!(
                lines
                    .last()
                    .unwrap()
                    .spans
                    .iter()
                    .any(|span| span.content.contains('…'))
            );
        }
    }
    #[test]
    fn short_empty_and_exact_boundary_errors_do_not_add_extra_rows() {
        for message in ["", "one", "中e\u{301}", "one\ntwo\nthree"] {
            let event = failure(message);
            assert_eq!(rows(&event, 40).len(), row_offsets(&event, 40).len());
            let text: String = rows(&event, 40)
                .into_iter()
                .skip(1)
                .flat_map(|row| row.spans.into_iter().map(|span| span.content.into_owned()))
                .collect();
            assert!(!text.contains('…'));
        }
        assert_eq!(rows(&failure("one\ntwo\nthree"), 40).len(), 4);
    }
}
