use super::*;
use crate::state::DetailKind;

pub(super) fn render_detail(frame: &mut Frame<'_>, panel: Rect, state: &UiState) {
    let Some(detail) = &state.detail else { return };
    components::detail_tabs::render(
        frame,
        panel,
        DetailTabsProps {
            active: detail.kind,
            focused: state.focus == Focus::Detail
                && state.focused_control.is_none_or(|target| {
                    matches!(
                        target,
                        HitTarget::DetailWork
                            | HitTarget::DetailChanges
                            | HitTarget::DetailContext
                            | HitTarget::DetailArtifacts
                            | HitTarget::DetailRecords
                            | HitTarget::DetailAcceptance
                    )
                }),
            selection: state
                .focused_control
                .map_or(state.detail_tab_selection, |target| match target {
                    HitTarget::DetailWork => 0,
                    HitTarget::DetailChanges => 1,
                    HitTarget::DetailContext => 2,
                    HitTarget::DetailArtifacts => 3,
                    HitTarget::DetailRecords => 4,
                    HitTarget::DetailAcceptance => 5,
                    _ => usize::MAX,
                }),
            active_color: ACCENT,
            inactive_color: MUTED,
            background: RAIL,
        },
    );
    let content = Rect::new(
        panel.x,
        panel.y.saturating_add(4),
        panel.width,
        panel.height.saturating_sub(4),
    );
    match detail.kind {
        DetailKind::Work => render_work(frame, content, state, &detail.title),
        DetailKind::Changes => render_workspace_changes(frame, panel, state),
        DetailKind::Context => render_context(frame, content, state, &detail.title),
        DetailKind::Artifacts => render_artifact(frame, panel, state),
        DetailKind::Records => render_records(frame, content, state, &detail.title),
        DetailKind::Acceptance => render_acceptance(frame, content, panel, state, &detail.title),
        DetailKind::Decision => render_write_decision(frame, content, panel, state),
    }
}

pub(super) fn regions(panel: Rect, state: &UiState) -> Vec<HitRegion> {
    let Some(detail) = state.detail.as_ref() else {
        return Vec::new();
    };
    let mut regions = components::detail_tabs::regions(panel);
    match detail.kind {
        DetailKind::Acceptance => {
            if state.acceptance_target().is_some() {
                regions.extend(acceptance_action_regions(panel));
            }
            regions.extend(acceptance_navigation_regions(panel, state));
        }
        DetailKind::Decision => {
            let unresolved = state
                .attention_detail
                .as_ref()
                .is_some_and(|item| match item {
                    bone_app::AttentionItem::UnresolvedWrite { session, call, .. } => state
                        .unresolved_writes
                        .iter()
                        .any(|write| write.session == *session && write.call == *call),
                    _ => false,
                });
            if unresolved {
                regions.extend(write_resolution_regions(panel));
            }
        }
        DetailKind::Changes => regions.extend(workspace_change_regions(panel, state)),
        DetailKind::Artifacts => regions.extend(artifact_regions(panel, state)),
        DetailKind::Work | DetailKind::Context | DetailKind::Records => {}
    }
    regions
}

mod acceptance;
mod artifacts;
mod changes;
mod context;
mod records;
mod work;

use acceptance::render_acceptance;
use acceptance::{acceptance_action_regions, acceptance_navigation_regions};
use artifacts::artifact_regions;
use artifacts::render_artifact;
use changes::render_workspace_changes;
use changes::workspace_change_regions;
use context::render_context;
use records::render_records;
use work::write_resolution_regions;
use work::{render_work, render_write_decision};
