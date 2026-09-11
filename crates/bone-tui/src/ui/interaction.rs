use ratatui::layout::Rect;

use crate::layout::PaneDivider;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitTarget {
    PaneDivider(PaneDivider),
    ConnectionKind(usize),
    SetupField(crate::state::SetupField),
    SaveConnection,
    SessionRail,
    NewSession,
    Commands,
    Models,
    Back,
    Reader,
    Model(usize),
    Object(usize),
    History(bone_app::SessionSeq),
    Job(bone_app::JobRef),
    Answer(bone_app::QuestionId),
    LeaveAnswer,
    ConvertAnswer,
    Restore(bone_app::InputId),
    Retry(bone_app::InputId),
    RetrySubmission,
    Session(bone_app::SessionId),
    Conversation,
    Composer,
    SlashCommand(crate::state::CommandKind),
    Submit,
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HitRegion {
    pub area: Rect,
    pub target: HitTarget,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HitMap {
    regions: Vec<HitRegion>,
}

impl HitMap {
    pub(crate) fn push(&mut self, region: HitRegion) {
        self.regions.push(region);
    }

    pub(crate) fn clear(&mut self) {
        self.regions.clear();
    }

    pub(crate) fn regions(&self) -> &[HitRegion] {
        &self.regions
    }

    pub(crate) fn hit(&self, x: u16, y: u16) -> Option<HitTarget> {
        self.regions
            .iter()
            .rev()
            .find(|region| contains(region.area, x, y))
            .map(|region| region.target)
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_registered_region_owns_an_overlap() {
        let mut hits = HitMap::default();
        hits.push(HitRegion {
            area: Rect::new(0, 0, 10, 10),
            target: HitTarget::Conversation,
        });
        hits.push(HitRegion {
            area: Rect::new(4, 0, 1, 10),
            target: HitTarget::PaneDivider(PaneDivider::Left),
        });

        assert_eq!(
            hits.hit(4, 5),
            Some(HitTarget::PaneDivider(PaneDivider::Left))
        );
        assert_eq!(hits.hit(3, 5), Some(HitTarget::Conversation));
        assert_eq!(hits.hit(11, 5), None);
    }
}
