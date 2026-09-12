use std::{ops::Deref, sync::Arc};

use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan, TranscriptMetrics},
    ui::interaction::HitMap,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameSnapshot {
    pub layout: LayoutPlan,
    hits: HitMap,
    pub transcript_metrics: Option<Arc<TranscriptMetrics>>,
    pub reader_max_scroll: usize,
    composer_row_origin: Option<usize>,
    title_byte_origin: Option<usize>,
}

impl FrameSnapshot {
    pub(crate) fn new(
        layout: LayoutPlan,
        hits: HitMap,
        transcript_metrics: Option<Arc<TranscriptMetrics>>,
        reader_max_scroll: usize,
        composer_row_origin: Option<usize>,
        title_byte_origin: Option<usize>,
    ) -> Self {
        Self {
            layout,
            hits,
            transcript_metrics,
            reader_max_scroll,
            composer_row_origin,
            title_byte_origin,
        }
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<HitTarget> {
        self.hits.hit(x, y)
    }

    pub fn hit_regions(&self) -> &[HitRegion] {
        self.hits.regions()
    }

    pub(crate) fn composer_row_origin(&self) -> Option<usize> {
        self.composer_row_origin
    }

    pub(crate) fn title_byte_origin(&self) -> Option<usize> {
        self.title_byte_origin
    }
}

impl Deref for FrameSnapshot {
    type Target = LayoutPlan;

    fn deref(&self) -> &Self::Target {
        &self.layout
    }
}
