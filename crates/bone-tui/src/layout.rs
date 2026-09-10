use std::{collections::BTreeMap, sync::Arc};

use bone_app::SessionSeq;
use ratatui::layout::Rect;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptMetrics {
    pub total_rows: usize,
    pub viewport_rows: usize,
    pub event_rows: BTreeMap<SessionSeq, usize>,
}

impl TranscriptMetrics {
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
    SessionRail,
    Session(usize),
    Conversation,
    Composer,
    SlashCommand(usize),
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
    pub conversation: Option<Rect>,
    pub extension_blank: Option<Rect>,
    pub session_header: Option<Rect>,
    pub transcript: Option<Rect>,
    pub composer: Option<Rect>,
    pub slash_palette: Option<Rect>,
    pub transcript_metrics: Option<Arc<TranscriptMetrics>>,
    pub hit_regions: Vec<HitRegion>,
}

impl LayoutPlan {
    pub fn calculate(
        screen: Rect,
        single_pane: SinglePane,
        slash_items: usize,
        session_count: usize,
        selected_session: Option<usize>,
    ) -> Self {
        let mode = mode_for(screen);
        let (session_rail, conversation, extension_blank) = match mode {
            LayoutMode::Wide => {
                let rail = 24.min(screen.width);
                let extension = 40.min(screen.width.saturating_sub(rail));
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
                let rail = 24.min(screen.width);
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
                let header_h = area.height.min(3);
                let composer_h = area.height.saturating_sub(header_h).min(6);
                let body_h = area
                    .height
                    .saturating_sub(header_h)
                    .saturating_sub(composer_h);
                let header = Rect::new(area.x, area.y, area.width, header_h);
                let transcript = Rect::new(area.x, area.y + header_h, area.width, body_h);
                let composer = Rect::new(
                    area.x,
                    area.bottom().saturating_sub(composer_h),
                    area.width,
                    composer_h,
                );
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
        if let Some(area) = session_rail {
            hit_regions.push(HitRegion {
                area,
                target: HitTarget::SessionRail,
            });
            let first_y = area.y.saturating_add(3);
            let list_bottom = area.bottom().saturating_sub(1);
            let visible = list_bottom.saturating_sub(first_y) as usize / 2;
            session_start = selected_session
                .unwrap_or(0)
                .saturating_sub(visible.saturating_sub(1))
                .min(session_count.saturating_sub(visible));
            for index in session_start..session_count.min(session_start + visible) {
                hit_regions.push(HitRegion {
                    area: Rect::new(
                        area.x,
                        first_y + (index - session_start) as u16 * 2,
                        area.width,
                        2,
                    ),
                    target: HitTarget::Session(index),
                });
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
            conversation,
            extension_blank,
            session_header,
            transcript,
            composer,
            slash_palette,
            transcript_metrics: None,
            hit_regions,
        }
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
    fn responsive_layout_removes_blank_extension_first() {
        let wide = LayoutPlan::calculate(
            Rect::new(0, 0, 160, 40),
            SinglePane::Conversation,
            0,
            3,
            None,
        );
        assert_eq!(wide.session_rail.unwrap().width, 24);
        assert_eq!(wide.extension_blank.unwrap().width, 40);
        assert_eq!(wide.conversation.unwrap().width, 96);
        let medium = LayoutPlan::calculate(
            Rect::new(0, 0, 120, 40),
            SinglePane::Conversation,
            0,
            3,
            None,
        );
        assert!(medium.extension_blank.is_none());
        assert_eq!(medium.conversation.unwrap().width, 96);
    }

    #[test]
    fn composer_is_always_inside_conversation() {
        for width in [40, 60, 99, 100, 139, 140, 180] {
            let plan = LayoutPlan::calculate(
                Rect::new(0, 0, width, 40),
                SinglePane::Conversation,
                5,
                8,
                None,
            );
            let center = plan.conversation.unwrap();
            let composer = plan.composer.unwrap();
            assert!(composer.x >= center.x && composer.right() <= center.right());
            assert!(composer.y >= center.y && composer.bottom() <= center.bottom());
        }
    }

    #[test]
    fn blank_extension_has_no_hit_targets() {
        let plan = LayoutPlan::calculate(
            Rect::new(0, 0, 180, 40),
            SinglePane::Conversation,
            4,
            4,
            None,
        );
        let blank = plan.extension_blank.unwrap();
        assert_eq!(plan.hit(blank.x + 1, blank.y + 1), None);
    }
}
