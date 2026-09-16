use std::sync::Arc;

use crate::{
    layout::{ClickTarget, LayoutPlan, TranscriptMetrics},
    ui::interaction::{FrameHits, ScrollTarget},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ComposerIdentity {
    Orphan,
    Session {
        session: bone_app::SessionId,
    },
    Answer {
        session: bone_app::SessionId,
        question: bone_app::QuestionId,
    },
}

/// Renderer-owned scroll memory for editor buffers that are not currently on
/// screen. It is deliberately separate from domain state and pointer geometry.
#[derive(Debug, Default)]
pub(crate) struct ViewState {
    composer_origins: Vec<(ComposerIdentity, usize)>,
}

impl ViewState {
    const MAX_COMPOSERS: usize = 128;

    pub(crate) fn composer_origin(&self, identity: ComposerIdentity) -> usize {
        self.composer_origins
            .iter()
            .rev()
            .find(|(candidate, _)| *candidate == identity)
            .map_or(0, |(_, origin)| *origin)
    }

    pub(crate) fn remember_composer(&mut self, identity: ComposerIdentity, origin: usize) {
        self.composer_origins
            .retain(|(candidate, _)| *candidate != identity);
        self.composer_origins.push((identity, origin));
        if self.composer_origins.len() > Self::MAX_COMPOSERS {
            self.composer_origins.remove(0);
        }
    }

    pub(crate) fn retain_composers(&mut self, mut keep: impl FnMut(ComposerIdentity) -> bool) {
        self.composer_origins
            .retain(|(identity, _)| keep(*identity));
    }
}

/// Geometry and interactions produced by one completed render.
#[derive(Debug, PartialEq)]
pub struct FrameSnapshot {
    pub layout: LayoutPlan,
    hits: FrameHits,
    pub transcript_metrics: Option<Arc<TranscriptMetrics>>,
    pub details_max_scroll: usize,
    composer_row_origin: Option<usize>,
    title_byte_origin: Option<usize>,
}

impl FrameSnapshot {
    pub(crate) fn new(
        layout: LayoutPlan,
        hits: FrameHits,
        transcript_metrics: Option<Arc<TranscriptMetrics>>,
        details_max_scroll: usize,
        composer_row_origin: Option<usize>,
        title_byte_origin: Option<usize>,
    ) -> Self {
        Self {
            layout,
            hits,
            transcript_metrics,
            details_max_scroll,
            composer_row_origin,
            title_byte_origin,
        }
    }

    pub(crate) fn text_at(&self, x: u16, y: u16) -> Option<crate::ui::selection::TextPoint> {
        self.hits.text_at(x, y)
    }

    pub(crate) fn text_point_in(
        &self,
        source: crate::ui::selection::CopySource,
        x: u16,
        y: u16,
    ) -> Option<crate::ui::selection::TextPoint> {
        self.hits.text_point_in(source, x, y)
    }

    pub(crate) fn source_content(
        &self,
        point: crate::ui::selection::TextPoint,
    ) -> Option<Arc<str>> {
        self.hits.source_content(point)
    }

    pub(crate) fn copy_between(
        &self,
        a: crate::ui::selection::TextPoint,
        b: crate::ui::selection::TextPoint,
    ) -> Option<String> {
        self.hits.copy_between(a, b)
    }

    pub(crate) fn selection_valid(&self, selection: &crate::state::TextSelection) -> bool {
        self.source_content(selection.anchor)
            .is_some_and(|text| text == selection.original)
    }

    pub(crate) fn pointer_shape(
        &self,
        state: &crate::state::UiState,
    ) -> crate::terminal::PointerShape {
        use crate::{state::PointerCapture, terminal::PointerShape};
        match state.pointer.capture {
            Some(PointerCapture::Divider(_)) => return PointerShape::ResizeHorizontal,
            Some(PointerCapture::Editor(_)) => return PointerShape::Text,
            Some(PointerCapture::Content)
                if state
                    .pointer
                    .press
                    .as_ref()
                    .is_some_and(|press| press.dragged)
                    && state.pointer.selection.is_some() =>
            {
                return PointerShape::Text;
            }
            _ => {}
        }
        let Some((x, y)) = state.pointer.position else {
            return PointerShape::Default;
        };
        match self.hit(x, y) {
            Some(ClickTarget::PaneDivider(_)) => PointerShape::ResizeHorizontal,
            Some(ClickTarget::Editor(_)) => PointerShape::Text,
            Some(ClickTarget::Action(_) | ClickTarget::Session(_)) => PointerShape::Pointer,
            None if self.text_at(x, y).is_some() => PointerShape::Text,
            None => PointerShape::Default,
        }
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<ClickTarget> {
        self.hits.hit(x, y)
    }

    pub fn scroll_hit(&self, x: u16, y: u16) -> Option<ScrollTarget> {
        self.hits.scroll_hit(x, y)
    }

    #[cfg(test)]
    pub(crate) fn overlay_area(&self) -> Option<ratatui::layout::Rect> {
        self.hits.overlay_area()
    }

    #[cfg(test)]
    pub(crate) fn hit_regions(&self) -> Vec<&crate::layout::ClickRegion> {
        self.hits.regions()
    }

    pub(crate) fn composer_row_origin(&self) -> Option<usize> {
        self.composer_row_origin
    }

    pub(crate) fn title_byte_origin(&self) -> Option<usize> {
        self.title_byte_origin
    }
}

#[cfg(test)]
mod pointer_tests {
    use super::*;
    use crate::{
        state::{PointerCapture, UiState},
        terminal::PointerShape,
    };
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn pointer_shape_comes_from_the_current_frame_and_active_capture() {
        let mut state = UiState::default();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
            .unwrap();
        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.pointer_shape(&state), PointerShape::Default);
        let divider = snapshot
            .hit_regions()
            .into_iter()
            .find(|hit| matches!(hit.target, ClickTarget::PaneDivider(_)))
            .unwrap();
        state.pointer.position = Some((divider.area.x, divider.area.y));
        assert_eq!(
            snapshot.pointer_shape(&state),
            PointerShape::ResizeHorizontal
        );
        let ClickTarget::PaneDivider(kind) = divider.target else {
            unreachable!()
        };
        state.pointer.capture = Some(PointerCapture::Divider(kind));
        let input = crate::layout::composer_text_area(snapshot.layout.composer.unwrap());
        state.pointer.position = Some((input.x, input.y));
        assert_eq!(
            snapshot.pointer_shape(&state),
            PointerShape::ResizeHorizontal
        );
        state.pointer.capture = None;
        assert_eq!(snapshot.pointer_shape(&state), PointerShape::Text);
        let model = snapshot
            .hit_regions()
            .into_iter()
            .find(|hit| {
                matches!(
                    hit.target,
                    ClickTarget::Action(crate::state::Action::OpenModels)
                )
            })
            .unwrap();
        state.pointer.position = Some((model.area.x, model.area.y));
        assert_eq!(snapshot.pointer_shape(&state), PointerShape::Pointer);
    }
}
