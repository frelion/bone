pub(crate) mod reader;
pub use crate::ui::frame::FrameSnapshot;

use crate::{
    layout::{ClickRegion, ClickTarget, LayoutPlan},
    state::UiState,
    ui::{
        frame::{ComposerIdentity, ViewState},
        interaction::{ClickTarget as PointerTarget, FrameHits, ScrollTarget, SurfaceHits},
        theme,
    },
};
use ratatui::{Frame, widgets::Block};

mod composer;
mod connection;
mod conversation;
mod message;
mod overlay;
mod right_rail;
mod session_rail;
mod slash_palette;

/// Render the conversational shell and return its exact pointer hit map.
#[cfg(test)]
pub(crate) fn render(frame: &mut Frame<'_>, state: &UiState) -> FrameSnapshot {
    render_with_view_state(frame, state, &mut ViewState::default())
}

pub(crate) fn render_with_view_state(
    frame: &mut Frame<'_>,
    state: &UiState,
    view_state: &mut ViewState,
) -> FrameSnapshot {
    let slash_visible = state.slash_palette_visible();
    let slash_matches = if slash_visible {
        state.slash_matches()
    } else {
        Vec::new()
    };
    let input_width = state
        .pane_widths
        .content_width(frame.area())
        .saturating_sub(4);
    let draft_lines = crate::editor::editor_rows(state.draft(), input_width);
    let selected_session = state
        .session_candidate
        .or(state.selected)
        .and_then(|id| state.session_rows.iter().position(|row| row.id() == id));
    let plan = LayoutPlan::calculate_with_widths(
        frame.area(),
        state.single_pane(),
        state.session_rows.len(),
        selected_session,
        state.session_scroll,
        draft_lines,
        state.pane_widths,
    );
    let mut hits = SurfaceHits::default();
    frame.render_widget(
        Block::default().style(theme::surface(theme::PANEL)),
        plan.screen,
    );
    session_rail::render(frame, &plan, &mut hits, state);
    if let Some(area) = plan.extension_blank {
        right_rail::render(frame, area);
    }
    let composer_identity = composer_identity(state);
    view_state.retain_composers(|identity| composer_identity_exists(state, identity));
    let previous_composer_row = view_state.composer_origin(composer_identity);
    let conversation = conversation::render(frame, &plan, &mut hits, state, previous_composer_row);
    let transcript_metrics = conversation.metrics.map(std::sync::Arc::new);
    if let Some(origin) = conversation.composer_row_origin {
        view_state.remember_composer(composer_identity, origin);
    }
    if plan.mode == crate::layout::LayoutMode::TooSmall {
        return FrameSnapshot::new(
            plan,
            FrameHits::default(),
            transcript_metrics,
            0,
            conversation.composer_row_origin,
            conversation.title_byte_origin,
        );
    }
    let mut details_max_scroll = 0;
    if let Some(details) = &state.details
        && let Some(area) = plan.details_area()
    {
        let metrics = reader::render(
            frame,
            area,
            &details.content,
            state.selected_ui().and_then(|ui| ui.snapshot.as_deref()),
            details.scroll,
        );
        details_max_scroll = metrics.max_scroll;
        for text in metrics.texts {
            hits.push_text(text);
        }
        if matches!(
            details.content.source,
            crate::state::reader::ReaderSource::Job(_)
        ) && let Some(snapshot) = state.selected_ui().and_then(|ui| ui.snapshot.as_ref())
        {
            hits.push_job_inputs(
                crate::ui::selection::CopySource::Details {
                    session: details.content.session,
                    source: details.content.source,
                },
                snapshot.clone(),
            );
        }
        hits.push_scroll(area, ScrollTarget::Details);
        frame.render_widget(
            ratatui::widgets::Paragraph::new("close details").style(theme::body(theme::MUTED)),
            metrics.back,
        );
        hits.push(ClickRegion {
            area: metrics.back,
            target: ClickTarget::Action(crate::state::Action::CloseDetails),
        });
    }
    render_dividers(frame, &plan, &mut hits, state);

    let editor = match state.keyboard {
        crate::state::KeyboardOwner::Workspace(crate::state::WorkspaceTarget::Composer) => {
            plan.composer
        }
        crate::state::KeyboardOwner::Workspace(crate::state::WorkspaceTarget::SessionTitle) => {
            plan.session_header
        }
        _ => None,
    };
    let mut overlay_hits = SurfaceHits::default();
    let overlay_area = if state.overlay.is_some() {
        overlay::render(frame, &plan, &mut overlay_hits, editor, state)
    } else if slash_visible {
        plan.composer.map(|composer| {
            slash_palette::render(
                frame,
                plan.screen,
                composer,
                &mut overlay_hits,
                state,
                &slash_matches,
            )
        })
    } else {
        None
    };
    let hits = FrameHits::new(hits, overlay_area.map(|area| (area, overlay_hits)));
    render_hover(frame, &hits, state);
    if let Some(selection) = &state.pointer.selection
        && hits
            .source_content(selection.anchor)
            .is_some_and(|text| text == selection.original)
    {
        for area in hits.selection_cells(selection.anchor, selection.end) {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    frame.buffer_mut()[(x, y)].set_bg(theme::SELECTED);
                }
            }
        }
    }
    if let Some(position) = conversation.caret {
        crate::ui::caret::place(frame, position, state.caret_visible);
    }
    FrameSnapshot::new(
        plan,
        hits,
        transcript_metrics,
        details_max_scroll,
        conversation.composer_row_origin,
        conversation.title_byte_origin,
    )
}

fn render_hover(frame: &mut Frame<'_>, hits: &FrameHits, state: &UiState) {
    let Some((x, y)) = state.pointer.position else {
        return;
    };
    let Some(region) = hits.region(x, y) else {
        return;
    };
    if matches!(region.target, PointerTarget::Editor(_)) {
        return;
    }
    let divider = matches!(region.target, PointerTarget::PaneDivider(_));
    let area = region.area.intersection(frame.area());
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            // Preserve any foreground control that covers part of this region.
            if hits.region(x, y) != Some(region) {
                continue;
            }
            let cell = &mut frame.buffer_mut()[(x, y)];
            if cell.bg == theme::FOCUS_MARK {
                continue;
            }
            if divider {
                if cell.bg != theme::STRUCTURE_ACTIVE {
                    cell.set_bg(theme::STRUCTURE_HOVER)
                        .set_fg(theme::STRUCTURE_HOVER);
                }
            } else {
                cell.set_bg(if cell.bg == theme::SELECTED {
                    theme::HOVER_SELECTED
                } else {
                    theme::HOVER
                });
            }
        }
    }
}

fn composer_identity(state: &UiState) -> ComposerIdentity {
    let Some(ui) = state.selected_ui() else {
        return ComposerIdentity::Orphan;
    };
    if let Some(answer) = ui.active_answer() {
        ComposerIdentity::Answer {
            session: ui.id,
            question: answer.question,
        }
    } else {
        ComposerIdentity::Session { session: ui.id }
    }
}

fn composer_identity_exists(state: &UiState, identity: ComposerIdentity) -> bool {
    match identity {
        ComposerIdentity::Orphan => true,
        ComposerIdentity::Session { session } => state.session_ui.contains_key(&session),
        ComposerIdentity::Answer { session, question } => state
            .session_ui
            .get(&session)
            .is_some_and(|ui| ui.answer_drafts.contains_key(&question)),
    }
}

fn render_dividers(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    hits: &mut SurfaceHits,
    state: &UiState,
) {
    // Reserve existing edge whitespace; never take width from the reading or input area.
    let left = plan
        .session_rail
        .filter(|_| plan.conversation.is_some())
        .map(|area| area.right() - 1);
    let right = plan.extension_blank.map(|area| area.x);
    for (position, divider) in [
        (left, crate::layout::PaneDivider::Left),
        (right, crate::layout::PaneDivider::Right),
    ] {
        let Some(x) = position else {
            continue;
        };
        hits.push(ClickRegion {
            area: ratatui::layout::Rect::new(x, plan.screen.y, 1, plan.screen.height),
            target: ClickTarget::PaneDivider(divider),
        });
        let color = if state.pointer.capture == Some(crate::state::PointerCapture::Divider(divider))
        {
            theme::STRUCTURE_ACTIVE
        } else {
            theme::STRUCTURE
        };
        for y in plan.screen.y..plan.screen.bottom() {
            frame.buffer_mut()[(x, y)]
                .set_symbol(" ")
                .set_fg(color)
                .set_bg(color);
        }
    }
}

pub use crate::text::sanitize_external;

fn single_line_external(value: &str) -> String {
    sanitize_external(value).replace(['\n', '\t'], " ")
}

#[cfg(test)]
mod editor_view_state_tests {
    use super::*;
    use crate::{
        editor::{CursorMove, EditCommand},
        state::{SessionNavRow, SessionUi},
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn draw(state: &UiState, view_state: &mut ViewState, width: u16, height: u16) -> FrameSnapshot {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| {
                snapshot = Some(render_with_view_state(frame, state, view_state));
            })
            .unwrap();
        snapshot.unwrap()
    }

    fn long_draft(label: &str) -> crate::editor::EditorBuffer {
        (0..20)
            .map(|row| format!("{label} row {row}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into()
    }

    fn fixture() -> (UiState, bone_app::SessionId, bone_app::SessionId) {
        let workspace = bone_app::WorkspaceId::new();
        let first = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace,
            title: "first".into(),
            archived: false,
        };
        let second = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace,
            title: "second".into(),
            archived: false,
        };
        let mut first_ui = SessionUi::new(first.id, 1);
        first_ui.draft = long_draft("first");
        let mut second_ui = SessionUi::new(second.id, 1);
        second_ui.draft = long_draft("second");
        let mut state = UiState::default();
        state.session_rows = vec![
            SessionNavRow::provisional(first.clone()),
            SessionNavRow::provisional(second.clone()),
        ];
        state.selected = Some(first.id);
        state.session_candidate = Some(first.id);
        state.session_ui.insert(first.id, first_ui);
        state.session_ui.insert(second.id, second_ui);
        (state, first.id, second.id)
    }

    fn move_up_within_viewport(state: &mut UiState, snapshot: &FrameSnapshot) {
        let width = crate::layout::composer_text_area(snapshot.layout.composer.unwrap()).width;
        state.editor_mut().apply(EditCommand::Move {
            cursor: CursorMove::Up { width },
            select: false,
        });
    }

    #[test]
    fn each_session_and_answer_keeps_its_own_bounded_viewport_origin() {
        let (mut state, first, second) = fixture();
        let mut view_state = ViewState::default();

        let first_frame = draw(&state, &mut view_state, 80, 12);
        let first_origin = first_frame.composer_row_origin().unwrap();
        assert!(first_origin > 0);
        move_up_within_viewport(&mut state, &first_frame);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(first_origin)
        );

        state.selected = Some(second);
        state.session_candidate = Some(second);
        let second_frame = draw(&state, &mut view_state, 80, 12);
        move_up_within_viewport(&mut state, &second_frame);
        state.selected = Some(first);
        state.session_candidate = Some(first);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(first_origin)
        );

        let question = bone_app::QuestionId {
            runtime: bone_app::RuntimeId::new(),
            record: 1,
            reply_to: bone_app::InputId(1),
        };
        let ui = state.session_ui.get_mut(&first).unwrap();
        let mut answer = crate::state::answer::AnswerDraft::new(question);
        answer.editor = long_draft("answer");
        ui.answer_drafts.insert(question, answer);
        ui.selected_answer = Some(question);
        let answer_frame = draw(&state, &mut view_state, 80, 12);
        let answer_origin = answer_frame.composer_row_origin().unwrap();
        move_up_within_viewport(&mut state, &answer_frame);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(answer_origin)
        );

        state.session_ui.get_mut(&first).unwrap().selected_answer = None;
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(first_origin)
        );
        state.session_ui.get_mut(&first).unwrap().selected_answer = Some(question);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(answer_origin)
        );
    }

    #[test]
    fn too_small_frame_does_not_erase_the_active_buffer_origin() {
        let (mut state, first, _) = fixture();
        let mut view_state = ViewState::default();
        let frame = draw(&state, &mut view_state, 80, 12);
        let origin = frame.composer_row_origin().unwrap();
        move_up_within_viewport(&mut state, &frame);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(origin)
        );

        let small = draw(&state, &mut view_state, 39, 11);
        assert_eq!(small.layout.mode, crate::layout::LayoutMode::TooSmall);
        state.session_ui.get_mut(&first).unwrap().generation += 1;
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(origin)
        );
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;
    use crate::state::{
        Action, EditorTarget, KeyboardOwner, PointerCapture, UiEvent, WorkspaceTarget, update,
    };
    use crate::ui::interaction::ScrollTarget;
    use ratatui::{Terminal, backend::TestBackend, style::Modifier};

    fn draw(terminal: &mut Terminal<TestBackend>, state: &UiState) -> FrameSnapshot {
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(render(frame, state)))
            .unwrap();
        snapshot.unwrap()
    }

    #[test]
    fn details_preserve_the_visible_input_at_wide_and_narrow_widths() {
        for width in [80, 160] {
            let mut state = UiState::default();
            state.details = Some(crate::state::ReaderState {
                content: crate::state::reader::ReaderContent {
                    session: bone_app::SessionId::new(),
                    source: crate::state::reader::ReaderSource::History(bone_app::SessionSeq(1)),
                    title: "reader".into(),
                    text: "content".into(),
                    layout_cache: std::cell::RefCell::new(None),
                },
                scroll: 0,
            });
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            let snapshot = draw(&mut terminal, &state);
            let composer = snapshot.layout.composer.unwrap();
            let details = snapshot.layout.details_area().unwrap();
            assert!(composer.intersection(details).is_empty());
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|cell| cell.bg == theme::FOCUS_MARK)
            );
            assert_eq!(
                state.keyboard,
                KeyboardOwner::Workspace(WorkspaceTarget::Composer)
            );
            assert_eq!(
                snapshot.scroll_hit(details.x, details.y),
                Some(ScrollTarget::Details)
            );
        }
    }

    #[test]
    fn pane_boundaries_survive_overlays_and_disappear_with_hidden_panes() {
        for (width, boundaries) in [(160, vec![31, 120]), (120, vec![31]), (80, vec![])] {
            for overlay in [
                None,
                Some(crate::state::Overlay::Models(
                    crate::state::ModelPanel::new(None),
                )),
            ] {
                let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
                let mut state = UiState::default();
                state.overlay = overlay;
                let snapshot = draw(&mut terminal, &state);
                let buffer = terminal.backend().buffer();
                let text_cells: Vec<_> = buffer
                    .content
                    .iter()
                    .filter(|cell| cell.symbol().chars().any(char::is_alphanumeric))
                    .collect();
                assert!(
                    text_cells
                        .iter()
                        .any(|cell| cell.modifier.contains(Modifier::BOLD))
                );
                assert!(
                    text_cells
                        .iter()
                        .any(|cell| !cell.modifier.contains(Modifier::BOLD))
                );
                for x in 0..width {
                    assert_eq!(
                        buffer[(x, 0)].bg == theme::STRUCTURE,
                        boundaries.contains(&x)
                    );
                }
                for x in &boundaries {
                    assert!(matches!(
                        snapshot.hit(*x, 0),
                        Some(ClickTarget::PaneDivider(_))
                    ));
                    for y in 0..40 {
                        if !snapshot
                            .overlay_area()
                            .is_some_and(|area| area.contains((*x, y).into()))
                        {
                            assert_eq!(buffer[(*x, y)].symbol(), " ");
                            assert_eq!(buffer[(*x, y)].bg, theme::STRUCTURE);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn overlays_only_occlude_their_rectangle_and_keep_the_active_editor_visible() {
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let mut state = UiState::default();
        state.overlay = Some(crate::state::Overlay::Models(
            crate::state::ModelPanel::new(None),
        ));
        let snapshot = draw(&mut terminal, &state);
        let overlay = snapshot.overlay_area().unwrap();
        let composer = snapshot.layout.composer.unwrap();
        assert!(overlay.intersection(composer).is_empty());
        assert_eq!(
            snapshot.hit(31, 0),
            Some(ClickTarget::PaneDivider(crate::layout::PaneDivider::Left))
        );
        let input = crate::layout::composer_text_area(composer);
        assert_eq!(
            snapshot.hit(input.x, input.y),
            Some(ClickTarget::Editor(EditorTarget::Composer))
        );
        assert!(!matches!(
            snapshot.hit(overlay.x, overlay.y),
            Some(ClickTarget::Editor(_) | ClickTarget::PaneDivider(_))
        ));
        assert_ne!(
            snapshot.scroll_hit(overlay.x, overlay.y),
            Some(ScrollTarget::Conversation)
        );
    }

    #[test]
    fn divider_drag_uses_neutral_structure_color_instead_of_focus_orange() {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut state = UiState::default();
        state.pointer.capture = Some(PointerCapture::Divider(crate::layout::PaneDivider::Left));
        draw(&mut terminal, &state);
        for y in 0..40 {
            let cell = &terminal.backend().buffer()[(31, y)];
            assert_eq!(cell.bg, theme::STRUCTURE_ACTIVE);
            assert_ne!(cell.bg, theme::FOCUS_MARK);
        }
    }

    #[test]
    fn hover_uses_current_geometry_and_preserves_keyboard_and_edit_history() {
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let mut state = UiState::default();
        update(
            &mut state,
            UiEvent::Action(Action::Edit {
                target: EditorTarget::Composer,
                command: crate::editor::EditCommand::Insert {
                    text: "a".into(),
                    typing: true,
                },
            }),
        );
        let snapshot = draw(&mut terminal, &state);
        let button = snapshot
            .hit_regions()
            .into_iter()
            .find(|region| region.target == ClickTarget::Action(Action::OpenModels))
            .unwrap()
            .area;
        let original = terminal.backend().buffer()[(button.x, button.y)].clone();
        state.caret_visible = false;
        update(
            &mut state,
            UiEvent::PointerMoved {
                column: button.x,
                row: button.y,
            },
        );
        draw(&mut terminal, &state);
        let hovered = &terminal.backend().buffer()[(button.x, button.y)];
        assert_eq!(hovered.bg, theme::HOVER);
        assert_eq!(hovered.fg, original.fg);
        assert!(!state.caret_visible);
        assert_eq!(
            state.keyboard,
            KeyboardOwner::Workspace(WorkspaceTarget::Composer)
        );
        update(
            &mut state,
            UiEvent::Action(Action::Edit {
                target: EditorTarget::Composer,
                command: crate::editor::EditCommand::Insert {
                    text: "b".into(),
                    typing: true,
                },
            }),
        );
        update(
            &mut state,
            UiEvent::Action(Action::Edit {
                target: EditorTarget::Composer,
                command: crate::editor::EditCommand::Undo,
            }),
        );
        assert_eq!(state.draft(), "");
        update(&mut state, UiEvent::PointerMoved { column: 31, row: 0 });
        draw(&mut terminal, &state);
        assert_eq!(
            terminal.backend().buffer()[(31, 0)].bg,
            theme::STRUCTURE_HOVER
        );
        state.pane_widths.left = 40;
        draw(&mut terminal, &state);
        assert_ne!(
            terminal.backend().buffer()[(31, 0)].bg,
            theme::STRUCTURE_HOVER
        );
        update(&mut state, UiEvent::PointerLeft);
        draw(&mut terminal, &state);
        assert_eq!(state.pointer.position, None);
    }
}
