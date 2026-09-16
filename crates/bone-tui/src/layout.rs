use bone_app::SessionSeq;
use ratatui::layout::Rect;

pub use crate::ui::interaction::{ClickRegion, ClickTarget};

const SESSION_RAIL_WIDTH: u16 = 32;
const EXTENSION_WIDTH: u16 = 40;
const CONTENT_INSET: u16 = 4;
const SESSION_ROW_HEIGHT: u16 = 4;

const MIN_SIDE_WIDTH: u16 = 24;
const MIN_CENTER_WIDTH: u16 = 56;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneDivider {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneWidths {
    pub left: u16,
    pub right: u16,
}

impl Default for PaneWidths {
    fn default() -> Self {
        Self {
            left: SESSION_RAIL_WIDTH,
            right: EXTENSION_WIDTH,
        }
    }
}

impl PaneWidths {
    /// Clamp displayed sizes without discarding the user's preferred widths on resize.
    pub fn fitted(self, screen: Rect) -> Self {
        match mode_for(screen) {
            LayoutMode::Wide => {
                let left = self.left.clamp(
                    MIN_SIDE_WIDTH,
                    screen.width - MIN_CENTER_WIDTH - MIN_SIDE_WIDTH,
                );
                let right = self
                    .right
                    .clamp(MIN_SIDE_WIDTH, screen.width - MIN_CENTER_WIDTH - left);
                Self { left, right }
            }
            LayoutMode::TwoColumn => Self {
                left: self
                    .left
                    .clamp(MIN_SIDE_WIDTH, screen.width - MIN_CENTER_WIDTH),
                right: 0,
            },
            _ => Self { left: 0, right: 0 },
        }
    }

    pub fn dragged(self, plan: &LayoutPlan, divider: PaneDivider, column: u16) -> Self {
        let mut widths = self;
        match divider {
            PaneDivider::Left if plan.session_rail.is_some() && plan.conversation.is_some() => {
                let right = plan.extension_blank.map_or(0, |area| area.width);
                if plan.extension_blank.is_some() {
                    widths.right = right;
                }
                widths.left = column
                    .saturating_sub(plan.screen.x)
                    .saturating_add(1)
                    .clamp(MIN_SIDE_WIDTH, plan.screen.width - MIN_CENTER_WIDTH - right);
            }
            PaneDivider::Right if plan.extension_blank.is_some() => {
                let left = plan.session_rail.map_or(0, |area| area.width);
                widths.left = left;
                widths.right = plan
                    .screen
                    .right()
                    .saturating_sub(column)
                    .clamp(MIN_SIDE_WIDTH, plan.screen.width - MIN_CENTER_WIDTH - left);
            }
            _ => {}
        }
        widths
    }

    pub fn content_width(self, screen: Rect) -> u16 {
        let widths = self.fitted(screen);
        screen
            .width
            .saturating_sub(widths.left + widths.right + CONTENT_INSET * 2)
    }
}

pub(crate) fn comfortable(area: Rect) -> bool {
    area.height >= 24
}

pub(crate) fn session_list_top(area: Rect) -> u16 {
    area.y + if comfortable(area) { 4 } else { 3 }
}

pub(crate) fn composer_text_area(area: Rect) -> Rect {
    Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    )
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptMetrics {
    pub total_rows: usize,
    pub viewport_rows: usize,
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
    packed: Box<[u32]>,
}
impl AnchorRun {
    fn len(&self) -> usize {
        self.packed.len()
    }
    fn packed(&self, index: usize) -> u32 {
        self.packed[index]
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
            let packed = anchors[start..end]
                .iter()
                .map(|anchor| {
                    anchor
                        .byte
                        .checked_mul(4)
                        .and_then(|byte| byte.checked_add(anchor.part as usize))
                        .and_then(|value| u32::try_from(value).ok())
                        .expect("transcript source offset exceeds the bounded App event size")
                })
                .collect();
            runs.push(AnchorRun {
                sequence,
                start,
                packed,
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
    fn len(&self) -> usize {
        self.len
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
                packed & 3 == anchor.part as u32 && (packed >> 2) as usize <= anchor.byte
            })
            .map(|index| run.start + index)
    }
    pub fn row_count(&self, sequence: SessionSeq) -> usize {
        self.runs
            .iter()
            .filter(|run| run.sequence == sequence)
            .map(AnchorRun::len)
            .sum()
    }
    pub fn allocated_bytes(&self) -> usize {
        self.runs.capacity() * std::mem::size_of::<AnchorRun>()
            + self
                .runs
                .iter()
                .map(|run| std::mem::size_of_val(&*run.packed))
                .sum::<usize>()
    }
}

impl TranscriptMetrics {
    pub fn allocated_bytes(&self) -> usize {
        self.anchors.allocated_bytes()
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
pub struct SessionRow {
    pub index: usize,
    pub area: Rect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutPlan {
    pub mode: LayoutMode,
    pub screen: Rect,
    pub session_rail: Option<Rect>,
    pub session_start: usize,
    pub session_max_start: usize,
    pub session_rows: Vec<SessionRow>,
    pub conversation: Option<Rect>,
    pub extension_blank: Option<Rect>,
    pub session_header: Option<Rect>,
    pub transcript: Option<Rect>,
    pub composer: Option<Rect>,
}

/// Vertical rhythm shared by floating menus. Comfortable terminals give each
/// choice a blank row; compact terminals keep every choice one row tall.
pub(crate) fn floating_menu_stride(screen: Rect) -> u16 {
    if comfortable(screen) { 2 } else { 1 }
}

/// Place a floating surface immediately above the Composer. `body_height`
/// includes the title, choices and footer, while this function adds the shared
/// comfortable-layout breathing room. Panels and the attached slash palette
/// use this exact geometry so their shells cannot drift apart.
pub(crate) fn floating_panel_area(screen: Rect, composer: Rect, body_height: u16) -> Rect {
    let chrome = if comfortable(screen) { 4 } else { 0 };
    let height = body_height
        .saturating_add(chrome)
        .min(screen.height.saturating_sub(2))
        .max(3);
    Rect::new(
        composer.x,
        composer.y.saturating_sub(height + 1).max(screen.y + 1),
        composer.width,
        height,
    )
}

/// Composer-owned overlays use the same panel geometry but never paint over
/// the editor that continues to receive text and display its caret.
pub(crate) fn attached_floating_panel_area(screen: Rect, composer: Rect, body_height: u16) -> Rect {
    let area = floating_panel_area(screen, composer, body_height);
    Rect::new(
        area.x,
        area.y,
        area.width,
        area.height
            .min(composer.y.saturating_sub(1).saturating_sub(area.y)),
    )
}

impl LayoutPlan {
    /// Details belong to the workspace even when there is no right column.
    pub(crate) fn details_area(&self) -> Option<Rect> {
        self.extension_blank.or(self.transcript)
    }

    /// Fit the single overlay into the workspace while exposing its keyboard editor.
    pub(crate) fn overlay_area(&self, body_height: u16, editor: Option<Rect>) -> Rect {
        let column = self.conversation.unwrap_or(self.screen);
        let mut available = Rect::new(
            column.x + 1,
            self.screen.y + 1,
            column.width.saturating_sub(2),
            self.screen.height.saturating_sub(2),
        );
        if let Some(editor) = editor {
            let above = Rect::new(
                available.x,
                available.y,
                available.width,
                editor.y.saturating_sub(available.y),
            );
            let below_y = editor.bottom().min(available.bottom());
            let below = Rect::new(
                available.x,
                below_y,
                available.width,
                available.bottom().saturating_sub(below_y),
            );
            available = if above.height >= below.height {
                above
            } else {
                below
            };
        }
        let height = body_height
            .saturating_add(if comfortable(self.screen) { 4 } else { 0 })
            .min(available.height);
        Rect::new(
            available.x,
            available.bottom().saturating_sub(height),
            available.width,
            height,
        )
    }

    pub fn calculate_with_widths(
        screen: Rect,
        single_pane: SinglePane,
        session_count: usize,
        selected_session: Option<usize>,
        requested_session_start: Option<usize>,
        draft_lines: u16,
        widths: PaneWidths,
    ) -> Self {
        let widths = widths.fitted(screen);
        let mode = mode_for(screen);
        let (session_rail, conversation, extension_blank) = match mode {
            LayoutMode::Wide => {
                let rail = widths.left;
                let extension = widths.right;
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
                let rail = widths.left;
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
        let (session_header, transcript, composer) =
            conversation.map_or((None, None, None), |area| {
                let inset = CONTENT_INSET;
                let width = area.width.saturating_sub(CONTENT_INSET * 2);
                let x = area.x + inset;
                let header = Rect::new(x + 2, area.y + 1, width.saturating_sub(2), 1);
                let compact = area.height < 18;
                let top = if comfortable(area) {
                    5
                } else if compact {
                    2
                } else {
                    3
                };
                let bottom = if compact { 0 } else { 2 };
                let status_gap = if compact { 1 } else { 2 };
                let max_lines = area
                    .height
                    .saturating_sub(top + bottom + status_gap + 7)
                    .clamp(1, 5);
                let min_lines = if comfortable(area) { 2 } else { 1 };
                let composer_h = draft_lines.clamp(min_lines.min(max_lines), max_lines) + 4;
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
                (Some(header), Some(transcript), Some(composer))
            });
        let (session_start, session_max_start, visible_session_rows) =
            session_rail.map_or((0, 0, Vec::new()), |area| {
                session_window(
                    area,
                    session_count,
                    selected_session,
                    requested_session_start,
                )
            });
        Self {
            mode,
            screen,
            session_rail,
            session_start,
            session_max_start,
            session_rows: visible_session_rows,
            conversation,
            extension_blank,
            session_header,
            transcript,
            composer,
        }
    }
}

fn session_window(
    area: Rect,
    session_count: usize,
    selected_session: Option<usize>,
    requested_start: Option<usize>,
) -> (usize, usize, Vec<SessionRow>) {
    let first_y = session_list_top(area);
    let bottom = area.bottom().saturating_sub(2);
    let capacity = usize::from(bottom.saturating_sub(first_y) / SESSION_ROW_HEIGHT);
    let max_start = session_count.saturating_sub(capacity.max(1));
    let selected = selected_session
        .unwrap_or(0)
        .min(session_count.saturating_sub(1));
    let selected_start = selected.saturating_sub(capacity.saturating_sub(1));
    let start = requested_start.unwrap_or(selected_start).min(max_start);
    let rows = (start..session_count)
        .take(capacity)
        .enumerate()
        .map(|(offset, index)| SessionRow {
            index,
            area: Rect::new(
                area.x + 1,
                first_y + offset as u16 * SESSION_ROW_HEIGHT,
                area.width.saturating_sub(3),
                SESSION_ROW_HEIGHT - 1,
            ),
        })
        .collect();
    (start, max_start, rows)
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
        assert_eq!(rows.row_count(SessionSeq(42)), count);
        assert_eq!(rows.row_count(SessionSeq(7)), 0);
        assert_eq!(
            rows.get(count - 1),
            Some(ContentAnchor {
                sequence: SessionSeq(42),
                byte: count - 1,
                part: AnchorPart::Text
            })
        );
    }

    #[test]
    fn responsive_layout_removes_blank_extension_first() {
        let wide = LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, 160, 40),
            SinglePane::Conversation,
            3,
            None,
            None,
            1,
            PaneWidths::default(),
        );
        assert_eq!(wide.session_rail.unwrap().width, 32);
        assert_eq!(wide.extension_blank.unwrap().width, 40);
        assert_eq!(wide.conversation.unwrap().width, 88);
        let medium = LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, 120, 40),
            SinglePane::Conversation,
            3,
            None,
            None,
            1,
            PaneWidths::default(),
        );
        assert!(medium.extension_blank.is_none());
        assert_eq!(medium.conversation.unwrap().width, 88);
    }

    #[test]
    fn composer_is_always_inside_conversation() {
        for width in [40, 60, 99, 100, 139, 140, 180] {
            let plan = LayoutPlan::calculate_with_widths(
                Rect::new(0, 0, width, 40),
                SinglePane::Conversation,
                8,
                None,
                None,
                1,
                PaneWidths::default(),
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
    fn blank_extension_has_no_session_geometry() {
        let plan = LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, 180, 40),
            SinglePane::Conversation,
            4,
            None,
            None,
            1,
            PaneWidths::default(),
        );
        let blank = plan.extension_blank.unwrap();
        assert!(plan.session_rows.iter().all(|row| row.area.x < blank.x));
    }
}

#[cfg(test)]
mod session_scroll_tests {
    use super::*;
    #[test]
    fn manual_scroll_updates_visible_rows_and_clamps_to_last_full_page() {
        let plan = LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, 80, 20),
            SinglePane::Sessions,
            20,
            Some(0),
            Some(3),
            1,
            PaneWidths::default(),
        );
        assert_eq!(plan.session_start, 3);
        assert_eq!(
            plan.session_rows.first(),
            Some(&SessionRow {
                index: 3,
                area: Rect::new(1, 3, 77, 3),
            })
        );
        let plan = LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, 80, 20),
            SinglePane::Sessions,
            20,
            Some(0),
            Some(usize::MAX),
            1,
            PaneWidths::default(),
        );
        assert_eq!(plan.session_start, 17);
        assert_eq!(plan.session_rows.last().map(|row| row.index), Some(19));
        assert_eq!(plan.session_rows.last().map(|row| row.area.y), Some(11));
    }
}
