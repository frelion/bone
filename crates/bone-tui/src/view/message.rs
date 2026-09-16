use crate::{
    layout::AnchorPart,
    text::wrap_text,
    ui::theme::{self, CODE_LABEL, DANGER, INK, MUTED, SUCCESS, USER, WARNING},
};
use bone_app::SessionEvent;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct MessageRow {
    pub line: Line<'static>,
    pub byte: usize,
    pub part: AnchorPart,
    copyable: bool,
}

pub(super) fn render(event: &SessionEvent, width: u16) -> Vec<MessageRow> {
    let width = usize::from(width.max(1));
    match event {
        SessionEvent::InputSubmitted { text, .. } => user_message(text, width),
        SessionEvent::Reply { text, .. } => reply_message(text, width),
        SessionEvent::QuestionAsked { text, .. } => message_body(text, width, WARNING),
        SessionEvent::ToolFinished { tool, outcome, .. } => {
            let mut rows = vec![MessageRow::text(
                compact(
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
                        SUCCESS
                    } else {
                        DANGER
                    },
                ),
                0,
            )];
            if let Err(error) = &outcome.result {
                rows.extend(error_preview(&error.message, width).into_iter().map(
                    |(byte, text)| {
                        MessageRow::text(Line::styled(text, Style::default().fg(DANGER)), byte + 1)
                    },
                ));
            }
            rows
        }
        SessionEvent::RoutingFailed { message, .. }
        | SessionEvent::InputRejected { message, .. } => {
            vec![MessageRow::text(compact("·", message, DANGER), 0)]
        }
        SessionEvent::JobFinished { summary, .. } => message_body(summary, width, INK),
        SessionEvent::AcceptanceRecorded { reason, .. } => {
            vec![MessageRow::text(compact("·", reason, SUCCESS), 0)]
        }
        SessionEvent::Interrupted { .. } => vec![MessageRow::text(
            compact("·", "Execution interrupted.", WARNING),
            0,
        )],
        SessionEvent::InputAccepted { .. } => {
            vec![MessageRow::text(compact("·", "Request received", MUTED), 0)]
        }
        SessionEvent::InputCancelled { .. } => vec![MessageRow::text(
            compact("·", "Request cancelled", MUTED),
            0,
        )],
        SessionEvent::InputFinished { outcome, .. } => vec![MessageRow::text(
            compact("·", &format!("Request {outcome:?}"), MUTED),
            0,
        )],
        SessionEvent::WriteResolved { evidence, .. } => {
            vec![MessageRow::text(compact("·", evidence, SUCCESS), 0)]
        }
        // Runtime and execution lifecycle are transport plumbing, not conversation content.
        SessionEvent::RuntimeStarted { .. }
        | SessionEvent::RuntimeReconfigured { .. }
        | SessionEvent::RuntimeClosed { .. }
        | SessionEvent::JobCreated { .. }
        | SessionEvent::CallStarted { .. }
        | SessionEvent::CallFinished { .. } => Vec::new(),
    }
}

impl MessageRow {
    fn text(line: Line<'static>, byte: usize) -> Self {
        Self {
            line,
            byte,
            part: AnchorPart::Text,
            copyable: true,
        }
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
    let mut preview: Vec<_> = wrapped_source(prefix, content_width)
        .into_iter()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(4)
        .collect();
    let truncated = end < message.len() || preview.len() > 3;
    preview.truncate(3);
    if preview.is_empty() {
        preview.push((
            0,
            if message.is_empty() {
                "(empty error)".into()
            } else {
                String::new()
            },
        ));
    }
    if truncated
        || preview
            .last()
            .is_some_and(|(_, line)| UnicodeWidthStr::width(line.as_str()) > content_width)
    {
        let line = &mut preview.last_mut().unwrap().1;
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
        .map(|(byte, mut text)| {
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

/// Wrap display text and retain the original UTF-8 byte at each visual row.
fn wrapped_source(value: &str, width: usize) -> Vec<(usize, String)> {
    let starts = crate::ui::selection::source_starts(value, width);
    (0..starts.len())
        .map(|row| {
            let range = crate::ui::selection::row_range(value, &starts, row);
            (
                range.start,
                crate::ui::selection::display_text(&value[range]),
            )
        })
        .collect()
}

/// Copy content is the original body for rich messages, and plain semantic
/// labels for compact events. No terminal padding or message decoration enters it.
pub(super) fn copy_text(event: &SessionEvent, rows: &[MessageRow]) -> std::sync::Arc<str> {
    match event {
        SessionEvent::InputSubmitted { text, .. }
        | SessionEvent::Reply { text, .. }
        | SessionEvent::QuestionAsked { text, .. } => text.as_str().into(),
        SessionEvent::JobFinished { summary, .. } => summary.as_str().into(),
        SessionEvent::ToolFinished { tool, outcome, .. } => {
            let mut text = format!(
                "{}  {}",
                tool,
                if outcome.result.is_ok() {
                    "done"
                } else {
                    "failed"
                }
            );
            if let Err(error) = &outcome.result {
                text.push('\n');
                text.push_str(&error.message);
            }
            text.into()
        }
        _ => rows
            .iter()
            .map(|row| {
                let spans = if row.line.spans.len() > 1 {
                    &row.line.spans[1..]
                } else {
                    &row.line.spans[..]
                };
                spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .into(),
    }
}

/// Returns the source offset and the layout-only left gutter for this row.
pub(super) fn copy_row(
    event: &SessionEvent,
    row: &MessageRow,
    index: usize,
    width: u16,
) -> Option<(usize, u16)> {
    if !row.copyable || row.part != AnchorPart::Text {
        return None;
    }
    Some(match event {
        SessionEvent::InputSubmitted { .. } => (row.byte, 2),
        SessionEvent::Reply { .. } => (row.byte, 2),
        SessionEvent::QuestionAsked { .. } | SessionEvent::JobFinished { .. } => (row.byte, 0),
        SessionEvent::ToolFinished { tool, outcome, .. } if index > 0 => {
            let header_len = tool.len() + if outcome.result.is_ok() { 6 } else { 8 };
            (header_len + row.byte, if width >= 4 { 2 } else { 0 })
        }
        _ => (0, if row.line.spans.len() > 1 { 2 } else { 0 }),
    })
}

fn user_message(value: &str, width: usize) -> Vec<MessageRow> {
    let content_width = width.saturating_sub(4).max(1);
    let blank = || Line::from(Span::styled(" ".repeat(width), Style::default().bg(USER)));
    let mut rows = vec![MessageRow {
        line: blank(),
        byte: 0,
        part: AnchorPart::UserTop,
        copyable: false,
    }];
    rows.extend(
        wrapped_source(value, content_width)
            .into_iter()
            .map(|(byte, content)| {
                let used = UnicodeWidthStr::width(content.as_str()).min(content_width);
                MessageRow::text(
                    Line::from(vec![
                        Span::styled("  ", Style::default().bg(USER)),
                        Span::styled(
                            format!("{content}{}", " ".repeat(width.saturating_sub(2 + used))),
                            theme::body_on(INK, USER),
                        ),
                    ])
                    .style(Style::default().bg(USER)),
                    byte,
                )
            }),
    );
    rows.push(MessageRow {
        line: blank(),
        byte: value.len(),
        part: AnchorPart::UserBottom,
        copyable: false,
    });
    rows
}

// Preserve source text and code indentation; fenced code stays on the open canvas.
fn reply_message(value: &str, width: usize) -> Vec<MessageRow> {
    let mut code = false;
    let mut rows = Vec::new();
    let mut base = 0;
    for source_line in value.split_inclusive('\n') {
        let source = source_line.trim_end_matches('\n').trim_end_matches('\r');
        let clean = super::sanitize_external(source);
        // `str::lines` omits a final fragment that sanitizes to empty.
        if !source_line.ends_with('\n') && clean.is_empty() {
            break;
        }
        if clean.trim_start().starts_with("```") {
            code = !code;
            if code {
                let language = clean.trim_start().trim_start_matches('`').trim();
                if !language.is_empty() {
                    let mut label = MessageRow::text(
                        Line::styled(format!("  {language}"), Style::default().fg(CODE_LABEL)),
                        base,
                    );
                    label.copyable = false;
                    rows.push(label);
                }
            }
            base += source_line.len();
            continue;
        }
        let heading = !code && clean.starts_with("# ");
        for (byte, text) in wrapped_source(source, width.saturating_sub(4)) {
            let tone = if code { theme::INFO } else { INK };
            rows.push(MessageRow::text(
                Line::styled(
                    format!("  {text}"),
                    if heading {
                        theme::label(tone)
                    } else {
                        theme::body(tone)
                    },
                ),
                base + byte,
            ));
        }
        base += source_line.len();
    }
    rows
}

fn message_body(value: &str, width: usize, tone: Color) -> Vec<MessageRow> {
    wrapped_source(value, width)
        .into_iter()
        .map(|(byte, text)| MessageRow::text(Line::styled(text, Style::default().fg(tone)), byte))
        .collect()
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

    fn visible_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn rendered_lines(event: &SessionEvent, width: u16) -> Vec<Line<'static>> {
        render(event, width)
            .into_iter()
            .map(|row| row.line)
            .collect()
    }

    #[test]
    fn source_rows_preserve_sanitized_unicode_wrapping() {
        for value in [
            "",
            "\u{1b}",
            "a\r\nb",
            "\t中e\u{301}",
            "🇺\u{200e}🇸 tail",
            "a\n",
            "\n\n",
        ] {
            for width in [1, 2, 5, 12] {
                let rows = wrapped_source(value, width);
                let starts = crate::ui::selection::source_starts(value, width);
                assert_eq!(rows.len(), starts.len());
                for (index, (_, rendered)) in rows.iter().enumerate() {
                    let range = crate::ui::selection::row_range(value, &starts, index);
                    assert_eq!(rendered, &crate::ui::selection::display_text(&value[range]));
                }
                assert!(rows.iter().all(|(byte, _)| value.is_char_boundary(*byte)));
                assert!(rows.windows(2).all(|pair| pair[0].0 <= pair[1].0));
            }
        }
    }

    #[test]
    fn user_rows_own_padding_and_text_anchor_identity() {
        let text = "ab中cd";
        let event = SessionEvent::InputSubmitted {
            input: InputId(1),
            request_id: RequestId::new(),
            text: text.into(),
            reply_to: None,
        };
        let rows = render(&event, 6);
        assert_eq!(
            rows.iter().map(|row| row.part).collect::<Vec<_>>(),
            [
                AnchorPart::UserTop,
                AnchorPart::Text,
                AnchorPart::Text,
                AnchorPart::Text,
                AnchorPart::UserBottom,
            ]
        );
        assert_eq!(
            rows.iter().map(|row| row.byte).collect::<Vec<_>>(),
            [0, 0, 2, 5, text.len()]
        );
    }

    #[test]
    fn reply_omits_a_sanitized_empty_final_fragment() {
        let reply = |text: &str| SessionEvent::Reply {
            job: bone_app::JobRef {
                runtime: RuntimeId::new(),
                id: 1,
            },
            inputs: vec![InputId(1)],
            text: text.into(),
        };
        assert!(render(&reply("\u{1b}"), 80).is_empty());
        assert_eq!(render(&reply("body\n\u{200e}"), 80).len(), 1);
    }

    #[test]
    fn conversation_messages_have_no_speaker_labels() {
        let user = SessionEvent::InputSubmitted {
            input: InputId(1),
            request_id: RequestId::new(),
            text: "UNIQUE_USER_BODY".into(),
            reply_to: None,
        };
        let assistant = SessionEvent::Reply {
            job: bone_app::JobRef {
                runtime: RuntimeId::new(),
                id: 1,
            },
            inputs: vec![InputId(1)],
            text: "UNIQUE_ASSISTANT_BODY".into(),
        };

        let user_text = visible_text(&rendered_lines(&user, 80));
        let assistant_text = visible_text(&rendered_lines(&assistant, 80));
        assert!(user_text.contains("UNIQUE_USER_BODY"));
        assert!(assistant_text.contains("UNIQUE_ASSISTANT_BODY"));
        for speaker in ["YOU", "BONE"] {
            assert!(!user_text.contains(speaker));
            assert!(!assistant_text.contains(speaker));
        }
    }

    #[test]
    fn long_user_message_is_not_capped() {
        let event = SessionEvent::InputSubmitted {
            input: InputId(1),
            request_id: RequestId::new(),
            text: "一二三四五六七八九十".repeat(20),
            reply_to: None,
        };
        assert!(render(&event, 12).len() > 12);
    }

    #[test]
    fn runtime_lifecycle_does_not_become_a_message() {
        let event = SessionEvent::RuntimeClosed {
            runtime: RuntimeId::new(),
        };
        assert!(render(&event, 80).is_empty());
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
            let rows = render(&event, width);
            assert!(rows.len() >= 2 && rows.len() <= 4);
            for row in rows.iter().skip(1) {
                let text: String = row
                    .line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(!text.chars().any(char::is_control));
                assert!(UnicodeWidthStr::width(text.as_str()) <= usize::from(width));
                assert!(!text.contains("TAIL"));
            }
            assert_eq!(rows[0].byte, 0);
            assert!(
                rows[1..]
                    .iter()
                    .all(|row| message.is_char_boundary(row.byte - 1))
            );
            assert!(rows.windows(2).all(|pair| pair[0].byte <= pair[1].byte));
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
            let rows = render(&event, 40);
            assert!(rows.len() <= 4);
            assert!(rows.iter().all(|row| row.byte <= 4097));
            assert!(
                rows.last()
                    .unwrap()
                    .line
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
            let text: String = render(&event, 40)
                .into_iter()
                .skip(1)
                .flat_map(|row| {
                    row.line
                        .spans
                        .into_iter()
                        .map(|span| span.content.into_owned())
                })
                .collect();
            assert!(!text.contains('…'));
        }
        assert_eq!(render(&failure("one\ntwo\nthree"), 40).len(), 4);
    }
}
