//! Mouse mapping. Clicks and scrolling query independent regions in the current frame.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::{
    layout::{ClickTarget, composer_text_area},
    state::{Action, EditorTarget, KeyboardOwner, PointerCapture, UiState, WorkspaceTarget},
    ui::interaction::ScrollTarget,
    view::FrameSnapshot,
};

pub(super) fn action(
    mouse: MouseEvent,
    snapshot: Option<&FrameSnapshot>,
    state: &UiState,
) -> Option<Action> {
    if let Some(capture) = state.pointer.capture {
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            return Some(match capture {
                PointerCapture::Divider(divider) => {
                    snapshot.map_or(Action::EndPointerCapture, |frame| Action::DragPane {
                        widths: state
                            .pane_widths
                            .dragged(&frame.layout, divider, mouse.column),
                        finish: true,
                    })
                }
                PointerCapture::Editor(target) => match snapshot
                    .and_then(|frame| point_editor(mouse, frame, state, target, false))
                {
                    Some(Action::PointEditor { byte, .. }) => {
                        Action::FinishEditorSelection { target, byte }
                    }
                    _ => Action::EndPointerCapture,
                },
                PointerCapture::Content => release_content(mouse, snapshot, state),
            });
        }
        if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            return match capture {
                PointerCapture::Divider(divider) => {
                    Some(snapshot.map_or(Action::EndPointerCapture, |frame| {
                        Action::DragPane {
                            widths: state
                                .pane_widths
                                .dragged(&frame.layout, divider, mouse.column),
                            finish: false,
                        }
                    }))
                }
                PointerCapture::Editor(target) => {
                    point_editor(mouse, snapshot?, state, target, false)
                }
                PointerCapture::Content => {
                    let point = selection_point(mouse, snapshot?, state);
                    let moved = match &state.pointer.selection {
                        Some(selection) => {
                            point.is_some_and(|point| point != selection.end)
                                || point.is_none()
                                    && state.pointer.press.as_ref().is_some_and(|press| {
                                        press.position != (mouse.column, mouse.row)
                                    })
                        }
                        None => state
                            .pointer
                            .press
                            .as_ref()
                            .is_some_and(|press| press.position != (mouse.column, mouse.row)),
                    };
                    moved.then_some(Action::DragTextSelection { point })
                }
            };
        }
    }

    let frame = snapshot?;
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let amount = if mouse.kind == MouseEventKind::ScrollUp {
                -3
            } else {
                3
            };
            match frame.scroll_hit(mouse.column, mouse.row)? {
                ScrollTarget::Sessions => Some(Action::ScrollSessions {
                    start: frame
                        .layout
                        .session_start
                        .saturating_add_signed(amount)
                        .min(frame.layout.session_max_start),
                }),
                ScrollTarget::Conversation if amount < 0 => Some(Action::ScrollUp(3)),
                ScrollTarget::Conversation => Some(Action::ScrollDown(3)),
                ScrollTarget::Details => Some(Action::ScrollDetails {
                    amount,
                    max: frame.details_max_scroll,
                }),
                ScrollTarget::OverlayContent { max } => Some(Action::ScrollOverlay { amount, max }),
                ScrollTarget::Overlay => Some(if amount < 0 {
                    Action::PanelPrevious
                } else {
                    Action::PanelNext
                }),
                ScrollTarget::Commands => Some(if amount < 0 {
                    Action::SelectSlashPrevious
                } else {
                    Action::SelectSlashNext
                }),
            }
        }
        MouseEventKind::Down(MouseButton::Left) => match frame.hit(mouse.column, mouse.row) {
            Some(ClickTarget::PaneDivider(divider)) => Some(Action::BeginPaneResize(divider)),
            Some(ClickTarget::Editor(target)) if editor_is_active(state, target) => {
                point_editor(mouse, frame, state, target, true)
            }
            target => Some(Action::PressPointer {
                target: target.map(Box::new),
                position: (mouse.column, mouse.row),
                text: frame
                    .text_at(mouse.column, mouse.row)
                    .and_then(|point| frame.source_content(point).map(|text| (point, text))),
            }),
        },
        _ => None,
    }
}

fn selection_point(
    mouse: MouseEvent,
    frame: &FrameSnapshot,
    state: &UiState,
) -> Option<crate::ui::selection::TextPoint> {
    let selection = state.pointer.selection.as_ref()?;
    frame.selection_valid(selection).then_some(())?;
    frame.text_point_in(selection.anchor.source, mouse.column, mouse.row)
}

fn release_content(mouse: MouseEvent, frame: Option<&FrameSnapshot>, state: &UiState) -> Action {
    let point = frame.and_then(|frame| {
        selection_point(mouse, frame, state).or_else(|| {
            state
                .pointer
                .selection
                .as_ref()
                .filter(|selection| frame.selection_valid(selection))
                .map(|selection| selection.end)
        })
    });
    let dragged = state
        .pointer
        .press
        .as_ref()
        .is_some_and(|press| press.dragged)
        || state
            .pointer
            .selection
            .as_ref()
            .is_some_and(|selection| point.is_some_and(|point| point != selection.anchor));
    let text = if dragged {
        state
            .pointer
            .selection
            .as_ref()
            .and_then(|selection| frame?.copy_between(selection.anchor, point?))
    } else {
        None
    };
    let click = if !dragged {
        state.pointer.press.as_ref().and_then(|press| {
            let target = frame?.hit(mouse.column, mouse.row)?;
            if press.target.as_ref() != Some(&target) {
                return None;
            }
            match target {
                ClickTarget::Action(action) => Some(Box::new(action)),
                ClickTarget::Session(session) => Some(Box::new(Action::SelectSession(session))),
                _ => None,
            }
        })
    } else {
        None
    };
    Action::ReleasePointer { click, point, text }
}

fn editor_is_active(state: &UiState, target: EditorTarget) -> bool {
    let workspace = match target {
        EditorTarget::Composer => WorkspaceTarget::Composer,
        EditorTarget::SessionTitle => WorkspaceTarget::SessionTitle,
    };
    state.keyboard == KeyboardOwner::Workspace(workspace)
}

fn point_editor(
    mouse: MouseEvent,
    frame: &FrameSnapshot,
    state: &UiState,
    target: EditorTarget,
    begin: bool,
) -> Option<Action> {
    let byte = match target {
        EditorTarget::SessionTitle => {
            let area = frame.layout.session_header?;
            crate::editor::cursor_at_single_line(
                state.title_text()?,
                frame.title_byte_origin()?,
                mouse
                    .column
                    .saturating_sub(area.x)
                    .min(area.width.saturating_sub(1)),
            )
        }
        EditorTarget::Composer => {
            let area = composer_text_area(frame.layout.composer?);
            crate::editor::cursor_at_origin(
                state.draft(),
                area.width,
                frame.composer_row_origin()?,
                mouse
                    .column
                    .saturating_sub(area.x)
                    .min(area.width.saturating_sub(1)),
                mouse
                    .row
                    .saturating_sub(area.y)
                    .min(area.height.saturating_sub(1)),
            )
        }
    };
    Some(Action::PointEditor {
        target,
        byte,
        extend: !begin || mouse.modifiers.contains(KeyModifiers::SHIFT),
        begin,
    })
}
