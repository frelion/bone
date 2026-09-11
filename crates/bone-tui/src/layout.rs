use std::{collections::BTreeMap, sync::Arc};

use bone_app::SessionSeq;
use ratatui::layout::Rect;

const SESSION_RAIL_WIDTH: u16 = 32;
const EXTENSION_WIDTH: u16 = 40;
const CONTENT_INSET: u16 = 4;

pub(crate) fn composer_text_area(area: Rect) -> Rect {
    Rect::new(
        area.x + 4,
        area.y + 1,
        area.width.saturating_sub(6),
        area.height.saturating_sub(4),
    )
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptMetrics {
    pub total_rows: usize,
    pub viewport_rows: usize,
    pub event_rows: BTreeMap<SessionSeq, usize>,
    pub anchors: AnchorRows,
    pub start_row: usize,
    pub scroll_from_tail: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentAnchor {
    pub sequence: SessionSeq,
    pub byte: usize,
    pub part: AnchorPart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnchorPart {
    Text,
    Separator,
    UserTop,
    UserBottom,
}

/// Sequence identity is stored once per event, not once per wrapped row.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnchorRows {
    runs: Vec<AnchorRun>,
    len: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct AnchorRun {
    sequence: SessionSeq,
    start: usize,
    small: Box<[u32]>,
    large: Box<[u64]>,
}
impl AnchorRun {
    fn len(&self) -> usize {
        self.small.len().max(self.large.len())
    }
    fn packed(&self, index: usize) -> u64 {
        if self.small.is_empty() {
            self.large[index]
        } else {
            u64::from(self.small[index])
        }
    }
}
impl From<Vec<ContentAnchor>> for AnchorRows {
    fn from(anchors: Vec<ContentAnchor>) -> Self {
        let mut runs = Vec::new();
        let mut start = 0;
        while start < anchors.len() {
            let sequence = anchors[start].sequence;
            let end = start
                + anchors[start..]
                    .iter()
                    .take_while(|anchor| anchor.sequence == sequence)
                    .count();
            let pack = |anchor: &ContentAnchor| (anchor.byte as u64) << 2 | anchor.part as u64;
            let (small, large) = if anchors[start..end]
                .iter()
                .all(|anchor| anchor.byte <= (u32::MAX >> 2) as usize)
            {
                (
                    anchors[start..end]
                        .iter()
                        .map(|anchor| pack(anchor) as u32)
                        .collect(),
                    Box::default(),
                )
            } else {
                (
                    Box::default(),
                    anchors[start..end].iter().map(pack).collect(),
                )
            };
            runs.push(AnchorRun {
                sequence,
                start,
                small,
                large,
            });
            start = end;
        }
        Self {
            runs,
            len: anchors.len(),
        }
    }
}
impl AnchorRows {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, row: usize) -> Option<ContentAnchor> {
        let index = self
            .runs
            .partition_point(|run| run.start <= row)
            .checked_sub(1)?;
        let run = &self.runs[index];
        let offset = row - run.start;
        if offset >= run.len() {
            return None;
        }
        let packed = run.packed(offset);
        let part = match packed & 3 {
            0 => AnchorPart::Text,
            1 => AnchorPart::Separator,
            2 => AnchorPart::UserTop,
            _ => AnchorPart::UserBottom,
        };
        Some(ContentAnchor {
            sequence: run.sequence,
            byte: (packed >> 2) as usize,
            part,
        })
    }
    pub fn row_for_anchor(&self, anchor: ContentAnchor) -> Option<usize> {
        let run = self
            .runs
            .iter()
            .find(|run| run.sequence == anchor.sequence)?;
        (0..run.len())
            .rfind(|&index| {
                let packed = run.packed(index);
                packed & 3 == anchor.part as u64 && packed >> 2 <= anchor.byte as u64
            })
            .map(|index| run.start + index)
    }
    pub fn allocated_bytes(&self) -> usize {
        self.runs.capacity() * std::mem::size_of::<AnchorRun>()
            + self
                .runs
                .iter()
                .map(|run| std::mem::size_of_val(&*run.small) + std::mem::size_of_val(&*run.large))
                .sum::<usize>()
    }
}

impl TranscriptMetrics {
    pub fn allocated_bytes(&self) -> usize {
        self.anchors.allocated_bytes() + self.event_rows.len() * 64
    }
    pub fn row_for_anchor(&self, anchor: ContentAnchor) -> Option<usize> {
        self.anchors.row_for_anchor(anchor)
    }
    pub fn anchor_at_start(&self, start: usize) -> Option<ContentAnchor> {
        self.anchors
            .get(start.min(self.anchors.len().saturating_sub(1)))
    }
    const PREFETCH_ROWS: usize = 10;

    pub fn max_scroll(&self) -> usize {
        self.total_rows.saturating_sub(self.viewport_rows)
    }

    pub fn near_start(&self, scroll_from_tail: usize) -> bool {
        scroll_from_tail
            .saturating_add(self.viewport_rows)
            .saturating_add(Self::PREFETCH_ROWS)
            >= self.total_rows
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutMode {
    Wide,
    TwoColumn,
    Single,
    TooSmall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SinglePane {
    Sessions,
    Conversation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitTarget {
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
    Session(usize),
    Conversation,
    Composer,
    SlashCommand(usize),
    Submit,
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HitRegion {
    pub area: Rect,
    pub target: HitTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutPlan {
    pub mode: LayoutMode,
    pub screen: Rect,
    pub session_rail: Option<Rect>,
    pub session_start: usize,
    pub session_max_start: usize,
    pub conversation: Option<Rect>,
    pub extension_blank: Option<Rect>,
    pub session_header: Option<Rect>,
    pub transcript: Option<Rect>,
    pub composer: Option<Rect>,
    pub slash_palette: Option<Rect>,
    pub slash_start: usize,
    pub transcript_metrics: Option<Arc<TranscriptMetrics>>,
    pub hit_regions: Vec<HitRegion>,
    pub reader_max_scroll: usize,
}

impl LayoutPlan {
    pub fn calculate(
        screen: Rect,
        single_pane: SinglePane,
        slash_items: usize,
        session_rows: &[u16],
        selected_session: Option<usize>,
        draft_lines: u16,
    ) -> Self {
        let mode = mode_for(screen);
        let (session_rail, conversation, extension_blank) = match mode {
            LayoutMode::Wide => {
                let rail = SESSION_RAIL_WIDTH.min(screen.width);
                let extension = EXTENSION_WIDTH.min(screen.width.saturating_sub(rail));
                let center = screen.width.saturating_sub(rail).saturating_sub(extension);
                (
                    Some(Rect::new(screen.x, screen.y, rail, screen.height)),
                    Some(Rect::new(screen.x + rail, screen.y, center, screen.height)),
                    Some(Rect::new(
                        screen.right().saturating_sub(extension),
                        screen.y,
                        extension,
                        screen.height,
                    )),
                )
            }
            LayoutMode::TwoColumn => {
                let rail = SESSION_RAIL_WIDTH.min(screen.width);
                (
                    Some(Rect::new(screen.x, screen.y, rail, screen.height)),
                    Some(Rect::new(
                        screen.x + rail,
                        screen.y,
                        screen.width.saturating_sub(rail),
                        screen.height,
                    )),
                    None,
                )
            }
            LayoutMode::Single | LayoutMode::TooSmall => match single_pane {
                SinglePane::Sessions => (Some(screen), None, None),
                SinglePane::Conversation => (None, Some(screen), None),
            },
        };
        let (session_header, transcript, composer, slash_palette) =
            conversation.map_or((None, None, None, None), |area| {
                let inset = CONTENT_INSET;
                let width = Self::content_width(screen);
                let x = area.x + inset;
                let header = Rect::new(x + 2, area.y + 1, width.saturating_sub(2), 1);
                let compact = area.height < 18;
                let top = if compact { 2 } else { 3 };
                let bottom = if compact { 0 } else { 2 };
                let status_gap = if compact { 1 } else { 2 };
                let max_lines = area
                    .height
                    .saturating_sub(top + bottom + status_gap + 7)
                    .clamp(1, 5);
                let composer_h = draft_lines.clamp(1, max_lines) + 4;
                let composer = Rect::new(
                    x,
                    area.bottom().saturating_sub(composer_h + bottom),
                    width,
                    composer_h,
                );
                let transcript = Rect::new(
                    x,
                    area.y + top,
                    width,
                    composer.y.saturating_sub(area.y + top + status_gap),
                );
                let body_h = transcript.height;
                let palette_h = (slash_items as u16).min(8).min(body_h);
                let palette = (palette_h > 0).then_some(Rect::new(
                    transcript.x,
                    transcript.bottom().saturating_sub(palette_h),
                    transcript.width,
                    palette_h,
                ));
                (Some(header), Some(transcript), Some(composer), palette)
            });
        let mut hit_regions = Vec::new();
        let mut session_start = 0;
        let mut session_max_start = 0;
        if let Some(area) = session_rail {
            hit_regions.push(HitRegion {
                area,
                target: HitTarget::SessionRail,
            });
            let first_y = area.y.saturating_add(4);
            let list_bottom = area.bottom().saturating_sub(2);
            let available = list_bottom.saturating_sub(first_y);
            session_max_start = session_rows.len().saturating_sub(1);
            let mut tail_height = session_rows.last().copied().unwrap_or(0);
            while session_max_start > 0
                && tail_height.saturating_add(session_rows[session_max_start - 1]) <= available
            {
                session_max_start -= 1;
                tail_height += session_rows[session_max_start];
            }
            let selected = selected_session
                .unwrap_or(0)
                .min(session_rows.len().saturating_sub(1));
            let mut used = session_rows.get(selected).copied().unwrap_or(0);
            session_start = selected;
            while session_start > 0 && used + session_rows[session_start - 1] <= available {
                session_start -= 1;
                used += session_rows[session_start];
            }
            let mut y = first_y;
            for (index, &height) in session_rows.iter().enumerate().skip(session_start) {
                if y + height > list_bottom {
                    break;
                }
                hit_regions.push(HitRegion {
                    area: Rect::new(area.x + 1, y, area.width.saturating_sub(2), height),
                    target: HitTarget::Session(index),
                });
                y += height;
            }
        }

        if let Some(area) = transcript {
            hit_regions.push(HitRegion {
                area,
                target: HitTarget::Conversation,
            });
        }
        if let Some(area) = composer {
            hit_regions.push(HitRegion {
                area,
                target: HitTarget::Composer,
            });
        }
        if let Some(area) = slash_palette {
            for index in 0..slash_items.min(area.height as usize) {
                hit_regions.push(HitRegion {
                    area: Rect::new(area.x, area.y + index as u16, area.width, 1),
                    target: HitTarget::SlashCommand(index),
                });
            }
        }
        Self {
            mode,
            screen,
            session_rail,
            session_start,
            session_max_start,
            conversation,
            extension_blank,
            session_header,
            transcript,
            composer,
            slash_palette,
            slash_start: 0,
            transcript_metrics: None,
            hit_regions,
            reader_max_scroll: 0,
        }
    }

    /// Manual list scrolling does not change the current conversation or candidate.
    pub fn scroll_sessions(&mut self, rows: &[u16], start: usize) {
        let Some(area) = self.session_rail else {
            return;
        };
        self.session_start = start.min(self.session_max_start);
        self.hit_regions
            .retain(|region| !matches!(region.target, HitTarget::Session(_)));
        let mut y = area.y.saturating_add(4);
        let bottom = area.bottom().saturating_sub(2);
        for (index, &height) in rows.iter().enumerate().skip(self.session_start) {
            if y.saturating_add(height) > bottom {
                break;
            }
            self.hit_regions.push(HitRegion {
                area: Rect::new(area.x + 1, y, area.width.saturating_sub(2), height),
                target: HitTarget::Session(index),
            });
            y += height;
        }
    }

    pub fn content_width(screen: Rect) -> u16 {
        let center = match mode_for(screen) {
            LayoutMode::Wide => screen.width - SESSION_RAIL_WIDTH - EXTENSION_WIDTH,
            LayoutMode::TwoColumn => screen.width - SESSION_RAIL_WIDTH,
            _ => screen.width,
        };
        center.saturating_sub(CONTENT_INSET * 2)
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<HitTarget> {
        self.hit_regions
            .iter()
            .rev()
            .find(|r| contains(r.area, x, y))
            .map(|r| r.target)
    }
}

fn mode_for(area: Rect) -> LayoutMode {
    if area.width < 40 || area.height < 12 {
        LayoutMode::TooSmall
    } else if area.width >= 140 {
        LayoutMode::Wide
    } else if area.width >= 100 {
        LayoutMode::TwoColumn
    } else {
        LayoutMode::Single
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_mib_newline_anchors_retain_compact_offsets_and_full_identity() {
        let count = 1024 * 1024 + 1;
        let rows: AnchorRows = (0..count)
            .map(|byte| ContentAnchor {
                sequence: SessionSeq(42),
                byte,
                part: AnchorPart::Text,
            })
            .collect::<Vec<_>>()
            .into();
        assert!(rows.allocated_bytes() <= count * 4 + 512);
        assert_eq!(
            rows.get(count - 1),
            Some(ContentAnchor {
                sequence: SessionSeq(42),
                byte: count - 1,
                part: AnchorPart::Text
            })
        );
        let wide = ContentAnchor {
            sequence: SessionSeq(u64::MAX),
            byte: u32::MAX as usize,
            part: AnchorPart::UserBottom,
        };
        let rows: AnchorRows = vec![wide].into();
        assert_eq!(rows.get(0), Some(wide));
    }

    #[test]
    fn responsive_layout_removes_blank_extension_first() {
        let wide = LayoutPlan::calculate(
            Rect::new(0, 0, 160, 40),
            SinglePane::Conversation,
            0,
            &[2; 3],
            None,
            1,
        );
        assert_eq!(wide.session_rail.unwrap().width, 32);
        assert_eq!(wide.extension_blank.unwrap().width, 40);
        assert_eq!(wide.conversation.unwrap().width, 88);
        let medium = LayoutPlan::calculate(
            Rect::new(0, 0, 120, 40),
            SinglePane::Conversation,
            0,
            &[2; 3],
            None,
            1,
        );
        assert!(medium.extension_blank.is_none());
        assert_eq!(medium.conversation.unwrap().width, 88);
    }

    #[test]
    fn composer_is_always_inside_conversation() {
        for width in [40, 60, 99, 100, 139, 140, 180] {
            let plan = LayoutPlan::calculate(
                Rect::new(0, 0, width, 40),
                SinglePane::Conversation,
                5,
                &[2; 8],
                None,
                1,
            );
            let center = plan.conversation.unwrap();
            let composer = plan.composer.unwrap();
            let transcript = plan.transcript.unwrap();
            assert_eq!(
                (composer.x, composer.width),
                (transcript.x, transcript.width)
            );
            assert_eq!(composer.x - center.x, CONTENT_INSET);
            assert_eq!(center.right() - composer.right(), CONTENT_INSET);
            assert!(composer.y >= center.y && composer.bottom() <= center.bottom());
        }
    }

    #[test]
    fn blank_extension_has_no_hit_targets() {
        let plan = LayoutPlan::calculate(
            Rect::new(0, 0, 180, 40),
            SinglePane::Conversation,
            4,
            &[2; 4],
            None,
            1,
        );
        let blank = plan.extension_blank.unwrap();
        assert_eq!(plan.hit(blank.x + 1, blank.y + 1), None);
    }
}

#[cfg(test)]
mod session_scroll_tests {
    use super::*;
    #[test]
    fn manual_scroll_updates_pointer_rows_and_clamps_to_last_full_page() {
        let rows = vec![2; 20];
        let mut plan = LayoutPlan::calculate(
            Rect::new(0, 0, 80, 20),
            SinglePane::Sessions,
            0,
            &rows,
            Some(0),
            1,
        );
        plan.scroll_sessions(&rows, 3);
        assert_eq!(plan.session_start, 3);
        assert_eq!(plan.hit(2, 4), Some(HitTarget::Session(3)));
        plan.scroll_sessions(&rows, usize::MAX);
        assert_eq!(plan.session_start, 13);
        assert_eq!(plan.hit(2, 16), Some(HitTarget::Session(19)));
    }
}
