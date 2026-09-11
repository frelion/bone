use std::{ops::Deref, sync::Arc};

use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan, TranscriptMetrics},
    ui::interaction::HitMap,
};

/// Geometry and interactions produced by one completed render.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameSnapshot {
    pub layout: LayoutPlan,
    hits: HitMap,
    pub transcript_metrics: Option<Arc<TranscriptMetrics>>,
    pub reader_max_scroll: usize,
}

impl FrameSnapshot {
    pub(crate) fn new(
        layout: LayoutPlan,
        hits: HitMap,
        transcript_metrics: Option<Arc<TranscriptMetrics>>,
        reader_max_scroll: usize,
    ) -> Self {
        Self {
            layout,
            hits,
            transcript_metrics,
            reader_max_scroll,
        }
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<HitTarget> {
        self.hits.hit(x, y)
    }

    pub fn hit_regions(&self) -> &[HitRegion] {
        self.hits.regions()
    }
}

impl Deref for FrameSnapshot {
    type Target = LayoutPlan;

    fn deref(&self) -> &Self::Target {
        &self.layout
    }
}
