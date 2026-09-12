//! Mouse mapping and hit-target translation.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::{
    layout::{HitTarget, composer_text_area},
    state::{Action, EditCommand, EditorTarget, Focus, UiState},
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
            snapshot.map(|frame| Action::ScrollPanel {
                amount: -3,
                max: frame.reader_max_scroll,
            })
        }
        MouseEventKind::ScrollDown if target == Some(HitTarget::Reader) => {
            snapshot.map(|frame| Action::ScrollPanel {
                amount: 3,
                max: frame.reader_max_scroll,
            })
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
            Some(Action::ScrollUp(3))
        }
        MouseEventKind::ScrollDown if target == Some(HitTarget::Conversation) => {
            Some(Action::ScrollDown(3))
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
            if target == Some(HitTarget::Composer) && state.panel.is_none() =>
        {
            composer_pointer(mouse, snapshot, state)
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
            if target == Some(HitTarget::SessionTitle) && state.panel.is_none() =>
        {
            title_pointer(mouse, snapshot, state)
        }
        MouseEventKind::Down(MouseButton::Left) => hit_action(target),
        _ => None,
    }
}

fn title_pointer(
    mouse: MouseEvent,
    snapshot: Option<&FrameSnapshot>,
    state: &UiState,
) -> Option<Action> {
    let area = snapshot.and_then(|frame| frame.layout.session_header)?;
    let text = state.title_text()?;
    let origin = snapshot.and_then(FrameSnapshot::title_byte_origin)?;
    let byte = crate::editor::cursor_at_single_line(
        text,
        origin,
        mouse
            .column
            .saturating_sub(area.x)
            .min(area.width.saturating_sub(1)),
    );
    Some(Action::Edit {
        target: EditorTarget::SessionTitle,
        command: EditCommand::Point {
            byte,
            extend: matches!(mouse.kind, MouseEventKind::Drag(_))
                || mouse.modifiers.contains(KeyModifiers::SHIFT),
        },
    })
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
        snapshot.and_then(FrameSnapshot::composer_row_origin)?,
        mouse
            .column
            .saturating_sub(area.x)
            .min(area.width.saturating_sub(1)),
        mouse
            .row
            .saturating_sub(area.y)
            .min(area.height.saturating_sub(1)),
    );
    Some(Action::Edit {
        target: EditorTarget::Composer,
        command: EditCommand::Point {
            byte,
            extend: matches!(mouse.kind, MouseEventKind::Drag(_))
                || mouse.modifiers.contains(KeyModifiers::SHIFT),
        },
    })
}

fn hit_action(target: Option<HitTarget>) -> Option<Action> {
    match target {
        Some(HitTarget::Action(action)) => Some(action),
        Some(HitTarget::PaneDivider(divider)) => Some(Action::BeginPaneResize(divider)),
        Some(HitTarget::Session(session)) => Some(Action::SelectSession(session)),
        Some(HitTarget::SessionRail) => Some(Action::Focus(Focus::Sessions)),
        Some(HitTarget::SessionTitle) => Some(Action::Focus(Focus::SessionTitle)),
        Some(HitTarget::Conversation) => None,
        Some(HitTarget::Composer) => Some(Action::Focus(Focus::Composer)),
        Some(HitTarget::Capture | HitTarget::Reader) => None,
        None => None,
    }
}
