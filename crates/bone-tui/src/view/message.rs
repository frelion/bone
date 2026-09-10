use bone_app::SessionEvent;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::{ACCENT, ATTENTION, DANGER, INK, MUTED, primitives::wrap_text};

pub(super) fn rows(event: &SessionEvent, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    match event {
        SessionEvent::InputSubmitted { text, .. } => user_rows(text, width),
        SessionEvent::Reply { text, .. } => body_rows(text, width, INK),
        SessionEvent::QuestionAsked { text, .. } => body_rows(text, width, ATTENTION),
        SessionEvent::ToolFinished { tool, outcome, .. } => vec![compact(
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
                MUTED
            } else {
                DANGER
            },
        )],
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

fn user_rows(value: &str, width: usize) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(3).max(1);
    wrap_text(value, content_width)
        .into_iter()
        .map(|content| {
            let used = UnicodeWidthStr::width(content.as_str()).min(content_width);
            Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Rgb(78, 145, 139))),
                Span::styled(
                    format!(
                        "{content}{} ",
                        " ".repeat(content_width.saturating_sub(used))
                    ),
                    Style::default().fg(INK).bg(Color::Rgb(34, 40, 47)),
                ),
            ])
        })
        .collect()
}

fn body_rows(value: &str, width: usize, tone: Color) -> Vec<Line<'static>> {
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
