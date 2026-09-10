use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutMode {
    Wide,
    Medium,
    Compact,
    Narrow,
    TooSmall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitTarget {
    Workbench,
    Sessions,
    Attention,
    Settings,
    NewSession,
    Session(usize),
    RenameSession,
    ArchiveSession,
    CloseDetail,
    Composer,
    Submit,
    Stop,
    Quit,
    DialogConfirm,
    Acceptance,
    DetailWork,
    DetailChanges,
    DetailContext,
    DetailArtifacts,
    DetailRecords,
    DetailAcceptance,
    Accept,
    PartiallyAccept,
    AcceptWithRisk,
    Reject,
    OlderResults,
    NewerResults,
    OlderAcceptances,
    NewerAcceptances,
    SettingsProfile(usize),
    ConfigureWorker,
    ConfigureCoordinator,
    Login,
    Logout,
    AttentionItem(usize),
    WriteApplied,
    WriteNotApplied,
    WorkspaceChange(usize),
    OlderWorkspaceChanges,
    NewerWorkspaceChanges,
    RefreshWorkspaceChanges,
    CloseWorkspaceFile,
    PreviousWorkspaceFile,
    MoreWorkspaceFile,
    Evidence(usize),
    OlderEvidence,
    NewerEvidence,
    RefreshArtifact,
    PreviousEvidenceSource,
    MoreEvidenceSource,
    CloseEvidenceSource,
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
    pub global_bar: Rect,
    pub session_rail: Option<Rect>,
    pub main_surface: Rect,
    pub detail_pane: Option<Rect>,
    pub action_bar: Rect,
    pub dialog_layer: Option<Rect>,
    pub hit_regions: Vec<HitRegion>,
}

impl LayoutPlan {
    pub fn calculate(screen: Rect, detail_open: bool, dialog_open: bool) -> Self {
        let mode = mode_for(screen);
        let (global_height, action_height) = match mode {
            LayoutMode::TooSmall => (1, 1),
            LayoutMode::Narrow => (2, 2),
            _ => (2, 2),
        };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(global_height),
                Constraint::Min(0),
                Constraint::Length(action_height),
            ])
            .split(screen);
        let global_bar = rows[0];
        let body = rows[1];
        let action_bar = rows[2];

        let (session_rail, main_surface, detail_pane) = match mode {
            LayoutMode::Wide => {
                if detail_open {
                    let columns = columns(body, &[24, body.width.saturating_sub(64), 40]);
                    (Some(columns[0]), columns[1], Some(columns[2]))
                } else {
                    let columns = columns(body, &[24, body.width.saturating_sub(24)]);
                    (Some(columns[0]), columns[1], None)
                }
            }
            LayoutMode::Medium => {
                if detail_open {
                    let columns = columns(body, &[body.width.saturating_sub(40), 40]);
                    (None, columns[0], Some(columns[1]))
                } else {
                    let columns = columns(body, &[24, body.width.saturating_sub(24)]);
                    (Some(columns[0]), columns[1], None)
                }
            }
            LayoutMode::Compact | LayoutMode::Narrow if detail_open => {
                (None, Rect::new(body.x, body.y, 0, body.height), Some(body))
            }
            LayoutMode::Compact | LayoutMode::Narrow | LayoutMode::TooSmall => (None, body, None),
        };

        let dialog_layer =
            dialog_open.then(|| centered(screen, 60.min(screen.width), 13.min(screen.height)));
        let mut plan = Self {
            mode,
            screen,
            global_bar,
            session_rail,
            main_surface,
            detail_pane,
            action_bar,
            dialog_layer,
            hit_regions: Vec::new(),
        };
        plan.build_hit_regions();
        plan
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<HitTarget> {
        self.hit_regions
            .iter()
            .rev()
            .find(|region| contains(region.area, x, y))
            .map(|region| region.target)
    }

    fn build_hit_regions(&mut self) {
        if self.mode == LayoutMode::TooSmall {
            self.push_action_regions();
            if let Some(area) = self.dialog_layer {
                self.hit_regions.push(HitRegion {
                    area: dialog_confirm_area(area),
                    target: HitTarget::DialogConfirm,
                });
            }
            return;
        }

        // These rectangles are also used by the renderer for the visible controls.
        let nav_widths = match self.mode {
            LayoutMode::Narrow => [10, 10, 10, 9],
            _ => [12, 12, 14, 10],
        };
        let mut x = self.global_bar.x;
        for (width, target) in nav_widths.into_iter().zip([
            HitTarget::Workbench,
            HitTarget::Sessions,
            HitTarget::Attention,
            HitTarget::Settings,
        ]) {
            let remaining = self.global_bar.right().saturating_sub(x);
            if remaining == 0 {
                break;
            }
            let area = Rect::new(
                x,
                self.global_bar.y,
                width.min(remaining),
                self.global_bar.height,
            );
            self.hit_regions.push(HitRegion { area, target });
            x = x.saturating_add(width);
        }
        if let Some(area) = self.session_rail {
            self.hit_regions.push(HitRegion {
                area: bottom_row(area),
                target: HitTarget::NewSession,
            });
        }
        if let Some(area) = self.detail_pane {
            self.hit_regions.push(HitRegion {
                area: Rect::new(
                    area.right().saturating_sub(6),
                    area.y,
                    6.min(area.width),
                    2.min(area.height),
                ),
                target: HitTarget::CloseDetail,
            });
        }
        if self.main_surface.width > 0 && self.main_surface.height > 0 {
            self.hit_regions.push(HitRegion {
                area: composer_area(self.main_surface),
                target: HitTarget::Composer,
            });
        }
        self.push_action_regions();
        if let Some(area) = self.dialog_layer {
            self.hit_regions.push(HitRegion {
                area: dialog_confirm_area(area),
                target: HitTarget::DialogConfirm,
            });
        }
    }

    fn push_action_regions(&mut self) {
        let widths = [18, 12, 10, 10];
        let targets = [
            HitTarget::Acceptance,
            HitTarget::Submit,
            HitTarget::Stop,
            HitTarget::Quit,
        ];
        let mut right = self.action_bar.right();
        for (width, target) in widths.into_iter().zip(targets).rev() {
            let actual = width.min(right.saturating_sub(self.action_bar.x));
            right = right.saturating_sub(actual);
            self.hit_regions.push(HitRegion {
                area: Rect::new(right, self.action_bar.y, actual, self.action_bar.height),
                target,
            });
        }
    }
}

pub fn composer_area(main: Rect) -> Rect {
    let height = main.height.min(5);
    Rect::new(
        main.x,
        main.bottom().saturating_sub(height),
        main.width,
        height,
    )
}

pub fn dialog_confirm_area(dialog: Rect) -> Rect {
    let width = dialog.width.min(18);
    Rect::new(
        dialog.x + dialog.width.saturating_sub(width) / 2,
        dialog.bottom().saturating_sub(3),
        width,
        dialog.height.min(2),
    )
}

fn mode_for(area: Rect) -> LayoutMode {
    if area.width < 40 || area.height < 12 {
        LayoutMode::TooSmall
    } else if area.width >= 160 {
        LayoutMode::Wide
    } else if area.width >= 120 {
        LayoutMode::Medium
    } else if area.width >= 80 {
        LayoutMode::Compact
    } else {
        LayoutMode::Narrow
    }
}

fn columns(area: Rect, widths: &[u16]) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(widths.iter().copied().map(Constraint::Length))
        .split(area)
        .to_vec()
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn bottom_row(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.bottom().saturating_sub(2),
        area.width,
        area.height.min(2),
    )
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsive_breakpoints_follow_the_product_contract() {
        for (width, expected) in [
            (160, LayoutMode::Wide),
            (159, LayoutMode::Medium),
            (120, LayoutMode::Medium),
            (119, LayoutMode::Compact),
            (80, LayoutMode::Compact),
            (79, LayoutMode::Narrow),
            (40, LayoutMode::Narrow),
            (39, LayoutMode::TooSmall),
        ] {
            let plan = LayoutPlan::calculate(Rect::new(0, 0, width, 24), false, false);
            assert_eq!(plan.mode, expected, "width {width}");
        }
        assert_eq!(
            LayoutPlan::calculate(Rect::new(0, 0, 160, 11), false, false).mode,
            LayoutMode::TooSmall
        );
    }

    #[test]
    fn wide_has_three_columns_and_medium_trades_rail_for_detail() {
        let wide = LayoutPlan::calculate(Rect::new(0, 0, 180, 30), true, false);
        assert_eq!(wide.session_rail.unwrap().width, 24);
        assert_eq!(wide.detail_pane.unwrap().width, 40);
        assert_eq!(wide.main_surface.width, 116);

        let medium = LayoutPlan::calculate(Rect::new(0, 0, 140, 30), true, false);
        assert!(medium.session_rail.is_none());
        assert_eq!(medium.detail_pane.unwrap().width, 40);
        assert_eq!(medium.main_surface.width, 100);
    }

    #[test]
    fn compact_detail_uses_the_single_available_surface() {
        let plan = LayoutPlan::calculate(Rect::new(0, 0, 100, 24), true, false);
        assert_eq!(plan.main_surface.width, 0);
        assert_eq!(plan.detail_pane, Some(Rect::new(0, 2, 100, 20)));
    }

    #[test]
    fn hit_testing_uses_rendered_rectangles_and_respects_overlays() {
        let plan = LayoutPlan::calculate(Rect::new(0, 0, 180, 30), true, true);
        let close = plan
            .hit_regions
            .iter()
            .find(|region| region.target == HitTarget::CloseDetail)
            .unwrap();
        assert_eq!(
            plan.hit(close.area.x, close.area.y),
            Some(HitTarget::CloseDetail)
        );
        let dialog = plan.dialog_layer.unwrap();
        let confirm = dialog_confirm_area(dialog);
        assert_eq!(
            plan.hit(confirm.x, confirm.y),
            Some(HitTarget::DialogConfirm)
        );
        assert_eq!(plan.hit(dialog.x, dialog.y), None);
    }
}
