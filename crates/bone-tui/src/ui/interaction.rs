use super::selection::{CopySource, SelectableText, TextPoint};
use ratatui::layout::Rect;
use std::sync::Arc;

use crate::{
    layout::PaneDivider,
    state::{Action, EditorTarget},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClickTarget {
    Action(Action),
    PaneDivider(PaneDivider),
    Session(bone_app::SessionId),
    Editor(EditorTarget),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClickRegion {
    pub area: Rect,
    pub target: ClickTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollTarget {
    Sessions,
    Conversation,
    Details,
    Overlay,
    OverlayContent { max: usize },
    Commands,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SurfaceHits {
    clicks: Vec<ClickRegion>,
    scrolls: Vec<(Rect, ScrollTarget)>,
    texts: Vec<SelectableText>,
    job_inputs: Vec<JobInputText>,
}

#[derive(Clone, Debug, PartialEq)]
struct JobInputText {
    source: CopySource,
    snapshot: Arc<bone_app::SessionView>,
}

impl JobInputText {
    fn content(&self, item: u64) -> Option<Arc<str>> {
        let CopySource::Details {
            session,
            source: crate::state::reader::ReaderSource::Job(job),
        } = self.source
        else {
            return None;
        };
        if self.snapshot.session.id != session {
            return None;
        }
        let inputs = &self
            .snapshot
            .jobs
            .iter()
            .find(|entry| entry.id == job)?
            .inputs;
        match item {
            0 => None,
            1 => Some(format!("Inputs ({})", inputs.len()).into()),
            _ => inputs
                .get(usize::try_from(item - 2).ok()?)
                .map(|input| input.0.to_string().into()),
        }
    }
}

impl SurfaceHits {
    pub(crate) fn push_job_inputs(
        &mut self,
        source: CopySource,
        snapshot: Arc<bone_app::SessionView>,
    ) {
        self.job_inputs.push(JobInputText { source, snapshot });
    }
    pub(crate) fn push_text(&mut self, text: SelectableText) {
        self.texts.push(text);
    }

    fn text_at(&self, x: u16, y: u16) -> Option<TextPoint> {
        self.texts.iter().rev().find_map(|text| text.point_at(x, y))
    }

    pub(crate) fn push(&mut self, region: ClickRegion) {
        self.clicks.push(region);
    }

    pub(crate) fn push_scroll(&mut self, area: Rect, target: ScrollTarget) {
        self.scrolls.push((area, target));
    }

    pub(crate) fn region(&self, x: u16, y: u16) -> Option<&ClickRegion> {
        self.clicks
            .iter()
            .rev()
            .find(|region| region.area.contains((x, y).into()))
    }

    #[cfg(test)]
    pub(crate) fn regions(&self) -> &[ClickRegion] {
        &self.clicks
    }

    fn scroll_hit(&self, x: u16, y: u16) -> Option<ScrollTarget> {
        self.scrolls
            .iter()
            .rev()
            .find(|(area, _)| area.contains((x, y).into()))
            .map(|(_, target)| *target)
    }
}

/// The root view composes the workspace and at most one locally opaque overlay.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FrameHits {
    workspace: SurfaceHits,
    overlay: Option<(Rect, SurfaceHits)>,
}

impl FrameHits {
    pub(crate) fn new(workspace: SurfaceHits, overlay: Option<(Rect, SurfaceHits)>) -> Self {
        Self { workspace, overlay }
    }

    fn surface(&self, x: u16, y: u16) -> &SurfaceHits {
        if let Some((area, hits)) = &self.overlay
            && area.contains((x, y).into())
        {
            hits
        } else {
            &self.workspace
        }
    }

    pub(crate) fn text_at(&self, x: u16, y: u16) -> Option<TextPoint> {
        self.surface(x, y).text_at(x, y)
    }

    pub(crate) fn source_content(&self, point: TextPoint) -> Option<Arc<str>> {
        self.texts()
            .find(|text| text.source == point.source && text.item == point.item)
            .map(|text| Arc::clone(&text.text))
            .or_else(|| {
                self.workspace
                    .job_inputs
                    .iter()
                    .chain(
                        self.overlay
                            .iter()
                            .flat_map(|(_, hits)| hits.job_inputs.iter()),
                    )
                    .find(|text| text.source == point.source)?
                    .content(point.item)
            })
    }

    fn texts(&self) -> impl Iterator<Item = &SelectableText> {
        self.workspace
            .texts
            .iter()
            .chain(self.overlay.iter().flat_map(|(_, hits)| hits.texts.iter()))
    }

    pub(crate) fn text_point_in(&self, source: CopySource, x: u16, y: u16) -> Option<TextPoint> {
        // An opaque overlay cannot expose the covered workspace text.
        if source != CopySource::Overlay
            && self
                .overlay
                .as_ref()
                .is_some_and(|(area, _)| area.contains((x, y).into()))
        {
            return self.text_at(x, y).filter(|point| point.source == source);
        }
        let surface = if source == CopySource::Overlay {
            &self.overlay.as_ref()?.1
        } else {
            &self.workspace
        };
        let (text, row) = surface
            .texts
            .iter()
            .filter(|text| text.source == source)
            .flat_map(|text| text.rows.iter().map(move |row| (text, row)))
            .min_by_key(|(_, row)| row.area.y.abs_diff(y))?;
        Some(TextPoint {
            source,
            item: text.item,
            byte: row.byte_at(x),
        })
    }

    pub(crate) fn copy_between(&self, a: TextPoint, b: TextPoint) -> Option<String> {
        if a.source != b.source {
            return None;
        }
        let (a, b) = if (a.item, a.byte) <= (b.item, b.byte) {
            (a, b)
        } else {
            (b, a)
        };
        let mut result = String::new();
        let mut had_text = false;
        for item in a.item..=b.item {
            let text = self.source_content(TextPoint {
                source: a.source,
                item,
                byte: 0,
            })?;
            let start = if item == a.item { a.byte } else { 0 };
            let end = if item == b.item { b.byte } else { text.len() };
            let part = text.get(start..end)?;
            if !text.is_empty() {
                if had_text {
                    result.push_str(
                        if matches!(a.source, CopySource::Details { .. }) && item > 1 {
                            "\n"
                        } else {
                            "\n\n"
                        },
                    );
                }
                result.push_str(&crate::text::sanitize_external(part));
                had_text = true;
            }
        }
        Some(result)
    }

    pub(crate) fn selection_cells(&self, a: TextPoint, b: TextPoint) -> Vec<Rect> {
        if a.source != b.source {
            return Vec::new();
        }
        let (a, b) = if (a.item, a.byte) <= (b.item, b.byte) {
            (a, b)
        } else {
            (b, a)
        };
        self.texts()
            .filter(|text| text.source == a.source && text.item >= a.item && text.item <= b.item)
            .flat_map(|text| {
                let start = if text.item == a.item { a.byte } else { 0 };
                let end = if text.item == b.item {
                    b.byte
                } else {
                    text.text.len()
                };
                text.rows
                    .iter()
                    .flat_map(move |row| row.selected_cells(start..end))
            })
            .filter(|cell| {
                (cell.x..cell.right()).all(|x| {
                    self.text_at(x, cell.y)
                        .is_some_and(|point| point.source == a.source)
                })
            })
            .collect()
    }

    pub(crate) fn region(&self, x: u16, y: u16) -> Option<&ClickRegion> {
        self.surface(x, y).region(x, y)
    }

    pub(crate) fn hit(&self, x: u16, y: u16) -> Option<ClickTarget> {
        self.region(x, y).map(|region| region.target.clone())
    }

    pub(crate) fn scroll_hit(&self, x: u16, y: u16) -> Option<ScrollTarget> {
        self.surface(x, y).scroll_hit(x, y)
    }

    #[cfg(test)]
    pub(crate) fn overlay_area(&self) -> Option<Rect> {
        self.overlay.as_ref().map(|(area, _)| *area)
    }

    #[cfg(test)]
    pub(crate) fn regions(&self) -> Vec<&ClickRegion> {
        self.workspace
            .clicks
            .iter()
            .chain(self.overlay.iter().flat_map(|(_, hits)| hits.clicks.iter()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_keep_their_scroll_owner_and_overlay_blanks_do_not_leak() {
        let mut workspace = SurfaceHits::default();
        workspace.push_scroll(Rect::new(0, 0, 30, 20), ScrollTarget::Conversation);
        workspace.push(ClickRegion {
            area: Rect::new(2, 2, 5, 1),
            target: ClickTarget::Action(Action::OpenModels),
        });
        let hits = FrameHits::new(
            workspace,
            Some((Rect::new(10, 5, 10, 8), SurfaceHits::default())),
        );
        assert_eq!(
            hits.hit(3, 2),
            Some(ClickTarget::Action(Action::OpenModels))
        );
        assert_eq!(hits.scroll_hit(3, 2), Some(ScrollTarget::Conversation));
        assert_eq!(hits.hit(12, 6), None);
        assert_eq!(hits.scroll_hit(12, 6), None);
        assert_eq!(hits.scroll_hit(2, 6), Some(ScrollTarget::Conversation));
    }
    fn text(
        source: CopySource,
        item: u64,
        value: &str,
        rows: Vec<super::super::selection::TextRow>,
    ) -> SelectableText {
        SelectableText {
            source,
            item,
            text: value.into(),
            rows,
        }
    }

    #[test]
    fn copied_range_survives_scroll_and_excludes_soft_wraps_and_decorations() {
        use super::super::selection::TextRow;
        let source = CopySource::Transcript(bone_app::SessionId::new());
        let value = "  alpha beta\n\t界e\u{301}";
        let mut workspace = SurfaceHits::default();
        workspace.push_text(text(
            source,
            1,
            value,
            vec![TextRow::new(Rect::new(8, 2, 8, 1), value, 0..value.len())],
        ));
        workspace.push_text(text(source, 2, "", vec![]));
        workspace.push_text(text(
            source,
            3,
            "last",
            vec![TextRow::new(Rect::new(8, 4, 8, 1), "last", 0..4)],
        ));
        let hits = FrameHits::new(workspace, None);
        let a = hits.text_at(8, 2).unwrap();
        let b = hits.text_at(12, 4).unwrap();
        assert_eq!(
            hits.copy_between(a, b).as_deref(),
            Some("  alpha beta\n\t界e\u{301}\n\nlast")
        );
        assert_eq!(hits.copy_between(b, a), hits.copy_between(a, b));
        assert_eq!(hits.copy_between(a, a).as_deref(), Some(""));
        assert_eq!(hits.text_at(7, 2), None);
        let mut scrolled = SurfaceHits::default();
        scrolled.push_text(text(source, 1, value, vec![]));
        scrolled.push_text(text(source, 2, "", vec![]));
        scrolled.push_text(text(
            source,
            3,
            "last",
            vec![TextRow::new(Rect::new(8, 2, 8, 1), "last", 0..4)],
        ));
        let scrolled = FrameHits::new(scrolled, None);
        assert_eq!(scrolled.copy_between(a, b), hits.copy_between(a, b));
        assert_eq!(scrolled.source_content(a).as_deref(), Some(value));
    }

    #[test]
    fn missing_history_is_not_silently_joined_and_overlay_occludes_text() {
        use super::super::selection::TextRow;
        let source = CopySource::Transcript(bone_app::SessionId::new());
        let mut workspace = SurfaceHits::default();
        workspace.push_text(text(
            source,
            1,
            "界abc",
            vec![TextRow::new(Rect::new(2, 3, 5, 1), "界abc", 0..6)],
        ));
        workspace.push_text(text(source, 3, "tail", vec![]));
        let hits = FrameHits::new(
            workspace,
            Some((Rect::new(3, 3, 2, 1), SurfaceHits::default())),
        );
        let a = TextPoint {
            source,
            item: 1,
            byte: 0,
        };
        let end = TextPoint {
            source,
            item: 1,
            byte: 6,
        };
        assert_eq!(hits.text_at(3, 3), None);
        assert_eq!(hits.text_point_in(source, 3, 3), None);
        assert_eq!(
            hits.selection_cells(a, end),
            [Rect::new(5, 3, 1, 1), Rect::new(6, 3, 1, 1)]
        );
        assert_eq!(
            hits.copy_between(
                a,
                TextPoint {
                    source,
                    item: 3,
                    byte: 4
                }
            ),
            None
        );
    }
}
