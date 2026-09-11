//! Mouse mapping and hit-target translation.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::{
    layout::{HitTarget, composer_text_area},
    state::{Action, Focus, UiState},
    view::FrameSnapshot,
};

pub(super) fn action(
    mouse: MouseEvent,
    snapshot: Option<&FrameSnapshot>,
    state: &UiState,
) -> Option<Action> {
    if let Some(divider) = state.dragging_divider
        && matches!(
            mouse.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        )
    {
        return Some(snapshot.map_or(Action::EndPaneResize, |frame| {
            Action::DragPane {
                widths: state
                    .pane_widths
                    .dragged(&frame.layout, divider, mouse.column),
                finish: matches!(mouse.kind, MouseEventKind::Up(_)),
            }
        }));
    }

    let target = snapshot.and_then(|frame| frame.hit(mouse.column, mouse.row));
    match mouse.kind {
        MouseEventKind::ScrollUp if target == Some(HitTarget::Reader) => {
            Some(Action::ScrollPanel { amount: -3, max: 0 })
        }
        MouseEventKind::ScrollDown if target == Some(HitTarget::Reader) => {
            Some(Action::ScrollPanel { amount: 3, max: 0 })
        }
        MouseEventKind::ScrollUp
            if matches!(target, Some(HitTarget::SessionRail | HitTarget::Session(_))) =>
        {
            snapshot.map(|frame| Action::ScrollSessions {
                start: frame.layout.session_start.saturating_sub(3),
            })
        }
        MouseEventKind::ScrollDown
            if matches!(target, Some(HitTarget::SessionRail | HitTarget::Session(_))) =>
        {
            snapshot.map(|frame| Action::ScrollSessions {
                start: frame
                    .layout
                    .session_start
                    .saturating_add(3)
                    .min(frame.layout.session_max_start),
            })
        }
        MouseEventKind::ScrollUp if target == Some(HitTarget::Conversation) => {
            Some(Action::ScrollUp {
                amount: 3,
                metrics: snapshot.and_then(|frame| frame.transcript_metrics.clone()),
            })
        }
        MouseEventKind::ScrollDown if target == Some(HitTarget::Conversation) => {
            Some(Action::ScrollDown(3))
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
            if target == Some(HitTarget::Composer) && state.panel.is_none() =>
        {
            composer_pointer(mouse, snapshot, state)
        }
        MouseEventKind::Down(MouseButton::Left) => hit_action(target),
        _ => None,
    }
}

fn composer_pointer(
    mouse: MouseEvent,
    snapshot: Option<&FrameSnapshot>,
    state: &UiState,
) -> Option<Action> {
    let area = snapshot
        .and_then(|frame| frame.layout.composer)
        .map(composer_text_area)?;
    let byte = crate::editor::cursor_at_origin(
        state.draft(),
        area.width,
        state.editor().viewport_origin(),
        mouse
            .column
            .saturating_sub(area.x)
            .min(area.width.saturating_sub(1)),
        mouse
            .row
            .saturating_sub(area.y)
            .min(area.height.saturating_sub(1)),
    );
    Some(
        if matches!(mouse.kind, MouseEventKind::Drag(_))
            || mouse.modifiers.contains(KeyModifiers::SHIFT)
        {
            Action::DragCursor(byte)
        } else {
            Action::PlaceCursor(byte)
        },
    )
}

fn hit_action(target: Option<HitTarget>) -> Option<Action> {
    match target {
        Some(HitTarget::PaneDivider(divider)) => Some(Action::BeginPaneResize(divider)),
        Some(HitTarget::Session(session)) => Some(Action::SelectSession(session)),
        Some(HitTarget::SessionRail) => Some(Action::Focus(Focus::Sessions)),
        Some(HitTarget::Conversation) => Some(Action::Focus(Focus::Conversation)),
        Some(HitTarget::Composer) => Some(Action::Focus(Focus::Composer)),
        Some(HitTarget::SlashCommand(command)) => Some(Action::ExecuteCommand(command)),
        Some(HitTarget::NewSession) => Some(Action::NewSession),
        Some(HitTarget::Commands) => Some(Action::OpenCommands),
        Some(HitTarget::Models) => Some(Action::OpenModels),
        Some(HitTarget::ConnectionKind(index)) => Some(Action::ChooseConnectionKind(index)),
        Some(HitTarget::SetupField(field)) => Some(Action::SelectField(field)),
        Some(HitTarget::SaveConnection) => Some(Action::SaveConnection),
        Some(HitTarget::Back) => Some(Action::Escape),
        Some(HitTarget::Reader) => None,
        Some(HitTarget::Model(index)) => Some(Action::SelectModel(index)),
        Some(HitTarget::Object(index)) => Some(Action::SelectObject(index)),
        Some(HitTarget::History(sequence)) => Some(Action::OpenHistory(sequence)),
        Some(HitTarget::Job(job)) => Some(Action::OpenJob(job)),
        Some(HitTarget::Answer(question)) => Some(Action::AnswerQuestion(question)),
        Some(HitTarget::LeaveAnswer) => Some(Action::LeaveAnswer),
        Some(HitTarget::ConvertAnswer) => Some(Action::ConvertAnswer),
        Some(HitTarget::Restore(input)) => Some(Action::RestoreInput(input)),
        Some(HitTarget::Retry(input)) => Some(Action::RetryInput(input)),
        Some(HitTarget::RetrySubmission) => Some(Action::RetrySubmission),
        Some(HitTarget::Submit) => Some(Action::ClickSubmit),
        Some(HitTarget::Stop) => Some(Action::Stop),
        None => None,
    }
}
