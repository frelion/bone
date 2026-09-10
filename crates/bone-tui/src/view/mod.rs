use bone_app::{ActivityKind, JobState, RuntimeState, SessionEvent};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    layout::{HitRegion, HitTarget, LayoutMode, LayoutPlan, composer_area},
    state::{Dialog, Focus, MainView, UiState},
};

mod attention;
mod components;
mod details;
mod dialogs;
mod settings;
mod shell;
mod workbench;

use attention::*;
use components::action_bar::ActionBarProps;
use components::chrome::{ActionButton, render_action_button, section_block};
use components::detail_tabs::DetailTabsProps;
use components::dialog_frame::DialogFrameProps;
use components::global_bar::GlobalBarProps;
use components::session_rail::{SessionRailItem, SessionRailProps};
use details::*;
use dialogs::*;
use settings::*;
use shell::*;
use workbench::*;

const INK: Color = Color::Rgb(218, 224, 232);
const MUTED: Color = Color::Rgb(125, 137, 151);
const PANEL: Color = Color::Rgb(28, 33, 40);
const RAIL: Color = Color::Rgb(22, 26, 32);
const ACCENT: Color = Color::Rgb(101, 190, 171);
const ATTENTION: Color = Color::Rgb(237, 180, 83);
const DANGER: Color = Color::Rgb(229, 107, 107);

pub fn render(frame: &mut Frame<'_>, state: &UiState) -> LayoutPlan {
    let mut plan =
        LayoutPlan::calculate(frame.area(), state.detail.is_some(), state.dialog.is_some());
    if state.main != MainView::Workbench {
        plan.hit_regions.retain(|region| {
            !matches!(
                region.target,
                HitTarget::Composer | HitTarget::Submit | HitTarget::Stop | HitTarget::Acceptance
            )
        });
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(PANEL)),
        plan.screen,
    );

    if plan.mode == LayoutMode::TooSmall {
        render_too_small(frame, &plan, state);
        let dialog_hit = render_dialog(frame, &plan, state);
        if state.dialog.is_some() {
            plan.hit_regions.clear();
            plan.hit_regions.extend(dialog_hit);
        }
        return plan;
    }

    render_global_bar(frame, &plan, state);
    if let Some(area) = plan.session_rail {
        plan.hit_regions.retain(|region| {
            !matches!(region.target, HitTarget::Session(_) | HitTarget::NewSession)
        });
        plan.hit_regions
            .extend(render_session_rail(frame, area, state));
    }
    if plan.main_surface.width > 0 {
        render_main(frame, plan.main_surface, state);
        if state.main == MainView::Sessions {
            plan.hit_regions
                .extend(session_browser_regions(plan.main_surface, state));
        } else if state.main == MainView::Settings {
            plan.hit_regions
                .extend(settings_action_regions(plan.main_surface, state));
        } else if state.main == MainView::Attention {
            plan.hit_regions
                .extend(attention_item_regions(plan.main_surface, state));
        }
    }
    if let Some(area) = plan.detail_pane {
        plan.hit_regions.extend(details::regions(area, state));
        render_detail(frame, area, state);
        if let Some(close) = plan
            .hit_regions
            .iter()
            .find(|region| region.target == HitTarget::CloseDetail)
        {
            let keyboard_selected = state.focus == Focus::Detail
                && state.focused_control == Some(HitTarget::CloseDetail);
            frame.render_widget(
                Paragraph::new("关闭")
                    .alignment(Alignment::Center)
                    .style(if keyboard_selected {
                        Style::default().fg(PANEL).bg(ACCENT)
                    } else {
                        Style::default().fg(ACCENT).bg(PANEL)
                    }),
                close.area,
            );
        }
    }
    render_action_bar(frame, &plan, state);
    let dialog_hit = render_dialog(frame, &plan, state);
    if state.dialog.is_some() {
        plan.hit_regions.clear();
        plan.hit_regions.extend(dialog_hit);
    }
    plan
}

fn event_lines(event: &SessionEvent, width: u16) -> Vec<Line<'static>> {
    use std::borrow::Cow;
    let (marker, color, body): (&str, Color, Cow<'_, str>) = match event {
        SessionEvent::InputSubmitted { text, .. } => ("你", ACCENT, text.into()),
        SessionEvent::Reply { text, .. } => ("BONE", INK, text.into()),
        SessionEvent::QuestionAsked { text, .. } => ("需要你", ATTENTION, text.into()),
        SessionEvent::RoutingFailed { message, .. } => ("路由失败", DANGER, message.into()),
        SessionEvent::JobFinished { summary, .. } => ("工作结果", ACCENT, summary.into()),
        SessionEvent::AcceptanceRecorded {
            decision, reason, ..
        } => {
            let decision = match decision {
                bone_app::AcceptanceDecision::Accepted => "已接受",
                bone_app::AcceptanceDecision::PartiallyAccepted => "部分接受",
                bone_app::AcceptanceDecision::AcceptedWithRisk => "带风险接受",
                bone_app::AcceptanceDecision::Rejected => "已退回",
            };
            ("用户验收", ACCENT, format!("{decision} · {reason}").into())
        }
        SessionEvent::ToolFinished { tool, outcome, .. } => {
            let status = if outcome.result.is_ok() {
                "完成"
            } else {
                "失败"
            };
            let effect = match outcome.external_effect {
                bone_app::ExternalEffect::None => "",
                bone_app::ExternalEffect::Applied => " · 已写入外部环境",
                bone_app::ExternalEffect::Unknown => " · 写入状态未知，需核查",
            };
            ("工具", MUTED, format!("{tool} · {status}{effect}").into())
        }
        SessionEvent::Interrupted { .. } => {
            ("已中断", ATTENTION, "旧执行已丢失，不会自动重放。".into())
        }
        SessionEvent::InputRejected { message, .. } => ("未接受", DANGER, message.into()),
        SessionEvent::InputAccepted { .. } => ("已接收", MUTED, "要求已进入执行。".into()),
        SessionEvent::InputCancelled { .. } => ("已取消", MUTED, "要求已取消。".into()),
        SessionEvent::RuntimeStarted { .. } => ("系统", MUTED, "执行环境已启动。".into()),
        SessionEvent::RuntimeReconfigured { .. } => ("系统", MUTED, "运行配置已更新。".into()),
        SessionEvent::RuntimeClosed { .. } => ("系统", MUTED, "执行环境已关闭。".into()),
        SessionEvent::InputFinished { outcome, .. } => {
            let label = match outcome {
                bone_app::InputOutcome::Completed => "已完成",
                bone_app::InputOutcome::Failed => "失败",
                bone_app::InputOutcome::Cancelled => "已取消",
            };
            ("请求结束", ACCENT, label.into())
        }
        SessionEvent::WriteResolved { evidence, .. } => ("写入核查", ACCENT, evidence.into()),
    };
    let body = components::paged_reader::visible_lines(
        body.as_ref(),
        0,
        width.saturating_sub(marker.chars().count() as u16 + 2),
        1,
    )
    .into_iter()
    .next()
    .map_or_else(String::new, |line| line.to_string());
    vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!("{marker}  "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(body, Style::default().fg(INK)),
        ]),
    ]
}

/// Convert untrusted product text to inert display text.
///
/// Ratatui does not write embedded escape bytes as commands, but removing all
/// terminal controls here also keeps snapshots, exports and future backends safe.
pub fn sanitize_external(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            matches!(character, '\n' | '\t')
                || (!character.is_control()
                    && !matches!(
                        *character as u32,
                        0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
                    ))
        })
        .collect()
}

fn single_line_external(value: &str) -> String {
    sanitize_external(value).replace(['\n', '\t'], " ")
}

fn visible_tail(value: &str, max_graphemes: usize) -> &str {
    let start = value
        .grapheme_indices(true)
        .rev()
        .nth(max_graphemes.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    &value[start..]
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn external_terminal_and_bidi_controls_are_inert() {
        let value = sanitize_external("ok\u{1b}]8;;bad\u{7}link\u{1b}\\\u{202e}X\nnext\tcell");
        assert_eq!(value, "ok]8;;badlink\\X\nnext\tcell");
        assert!(!value.contains('\u{1b}'));
    }

    #[test]
    fn empty_state_renders_at_product_breakpoints() {
        for (width, height) in [(160, 50), (120, 40), (80, 24), (40, 12), (39, 12)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, &UiState::default());
                })
                .unwrap();
        }
    }

    #[test]
    fn settings_does_not_expose_hidden_submit_or_stop_targets() {
        let state = UiState {
            main: MainView::Settings,
            ..UiState::default()
        };
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        assert!(!plan.unwrap().hit_regions.iter().any(|region| matches!(
            region.target,
            HitTarget::Submit | HitTarget::Stop | HitTarget::Composer
        )));
    }

    #[test]
    fn session_browser_exposes_visible_mouse_management_controls() {
        let info = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "可管理会话".into(),
            archived: false,
        };
        let mut state = UiState {
            main: MainView::Sessions,
            sessions: vec![info.clone()],
            selected: Some(info.id),
            ..UiState::default()
        };
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info, 1));
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        for target in [HitTarget::RenameSession, HitTarget::ArchiveSession] {
            let region = plan
                .hit_regions
                .iter()
                .find(|region| region.target == target)
                .expect("visible management action must own a hit region");
            assert_eq!(plan.hit(region.area.x, region.area.y), Some(target));
        }
        let cells = terminal.backend().buffer().content();
        assert!(cells.iter().any(|cell| cell.symbol() == "重"));
        assert!(cells.iter().any(|cell| cell.symbol() == "归"));

        state.sessions[0].archived = true;
        state
            .session_ui
            .get_mut(&state.sessions[0].id)
            .unwrap()
            .info
            .archived = true;
        terminal
            .draw(|frame| {
                render(frame, &state);
            })
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol() == "恢")
        );
    }

    #[test]
    fn acceptance_decisions_are_visible_only_for_a_real_result() {
        let info = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "one".into(),
            archived: false,
        };
        let mut state = UiState {
            selected: Some(info.id),
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Acceptance,
                title: "结果与验收".into(),
            }),
            ..UiState::default()
        };
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info.clone(), 1));
        let render_plan = |state: &UiState| {
            let backend = TestBackend::new(160, 50);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut plan = None;
            terminal
                .draw(|frame| plan = Some(render(frame, state)))
                .unwrap();
            plan.unwrap()
        };
        let empty = render_plan(&state);
        assert!(!empty.hit_regions.iter().any(|region| matches!(
            region.target,
            HitTarget::Accept
                | HitTarget::PartiallyAccept
                | HitTarget::AcceptWithRisk
                | HitTarget::Reject
        )));

        state.results.insert(
            info.id,
            bone_app::ResultPage {
                items: vec![bone_app::ResultSummary {
                    result: bone_app::ResultRef {
                        session: info.id,
                        job: bone_app::JobRef {
                            runtime: bone_app::RuntimeId::new(),
                            id: 1,
                        },
                        version: bone_app::SessionSeq(3),
                    },
                    outcome: bone_app::OutcomeKind::Completed,
                    summary: "完成".into(),
                    remaining: Vec::new(),
                }],
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(3),
                projection_pending: false,
            },
        );
        let populated = render_plan(&state);
        for target in [
            HitTarget::Accept,
            HitTarget::PartiallyAccept,
            HitTarget::AcceptWithRisk,
            HitTarget::Reject,
        ] {
            assert!(
                populated
                    .hit_regions
                    .iter()
                    .any(|region| region.target == target)
            );
        }
        state.session_ui.get_mut(&info.id).unwrap().result_loading = true;
        let refreshing = render_plan(&state);
        assert!(!refreshing.hit_regions.iter().any(|region| matches!(
            region.target,
            HitTarget::Accept
                | HitTarget::PartiallyAccept
                | HitTarget::AcceptWithRisk
                | HitTarget::Reject
        )));
    }

    #[test]
    fn dialog_blocks_every_underlying_mouse_target() {
        for (width, height) in [(160, 50), (39, 11), (80, 11), (39, 24)] {
            let state = UiState {
                dialog: Some(Dialog::ConfirmQuit),
                ..UiState::default()
            };
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut plan = None;
            terminal
                .draw(|frame| plan = Some(render(frame, &state)))
                .unwrap();
            let plan = plan.unwrap();
            assert!(!plan.hit_regions.is_empty());
            assert!(
                plan.hit_regions
                    .iter()
                    .all(|region| region.target == HitTarget::DialogConfirm),
                "underlying hit target leaked at {width}x{height}"
            );
        }
    }

    #[test]
    fn settings_renders_clickable_profile_model_and_login_controls() {
        let info = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "settings".into(),
            archived: false,
        };
        let state = UiState {
            main: MainView::Settings,
            selected: Some(info.id),
            settings: Some(crate::state::SettingsData {
                session: info.id,
                generation: 1,
                query: 0,
                resolved: bone_app::ResolvedConfig {
                    desired: Err(bone_app::ConfigProblem::NeedsModel),
                    running: None,
                },
                profiles: vec![bone_app::Profile::chatgpt()],
            }),
            ..UiState::default()
        };
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        for target in [
            HitTarget::SettingsProfile(0),
            HitTarget::ConfigureWorker,
            HitTarget::ConfigureCoordinator,
            HitTarget::Login,
            HitTarget::Logout,
        ] {
            assert!(
                plan.hit_regions
                    .iter()
                    .any(|region| region.target == target)
            );
        }
    }

    #[test]
    fn stale_settings_have_no_visible_or_clickable_controls() {
        let stale = bone_app::SessionId::new();
        let selected = bone_app::SessionId::new();
        let state = UiState {
            main: MainView::Settings,
            selected: Some(selected),
            settings: Some(crate::state::SettingsData {
                session: stale,
                generation: 1,
                query: 0,
                resolved: bone_app::ResolvedConfig {
                    desired: Err(bone_app::ConfigProblem::NeedsModel),
                    running: None,
                },
                profiles: vec![bone_app::Profile::chatgpt()],
            }),
            ..UiState::default()
        };
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        assert!(!plan.unwrap().hit_regions.iter().any(|region| matches!(
            region.target,
            HitTarget::SettingsProfile(_)
                | HitTarget::ConfigureWorker
                | HitTarget::ConfigureCoordinator
                | HitTarget::Login
                | HitTarget::Logout
        )));
    }

    #[test]
    fn device_login_prompt_does_not_overlap_profile_rows() {
        let session = bone_app::SessionId::new();
        let profile = bone_app::Profile::chatgpt();
        let mut state = UiState {
            main: MainView::Settings,
            selected: Some(session),
            settings: Some(crate::state::SettingsData {
                session,
                generation: 1,
                query: 0,
                resolved: bone_app::ResolvedConfig {
                    desired: Err(bone_app::ConfigProblem::NeedsModel),
                    running: None,
                },
                profiles: vec![profile.clone()],
            }),
            ..UiState::default()
        };
        state.login_states.insert(
            profile.id,
            bone_app::LoginState::DeviceCode {
                verification_uri: "https://example.test/device".into(),
                user_code: "ABCD-EFGH".into(),
            },
        );
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, &state);
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("https://example.test/device"));
        assert!(text.contains("ABCD-EFGH"));
    }

    #[test]
    fn non_acceptance_details_never_expose_acceptance_navigation_targets() {
        let session = bone_app::SessionId::new();
        let mut state = UiState {
            selected: Some(session),
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Context,
                title: "上下文".into(),
            }),
            ..UiState::default()
        };
        state.results.insert(
            session,
            bone_app::ResultPage {
                items: Vec::new(),
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(1),
                projection_pending: true,
            },
        );
        let backend = TestBackend::new(160, 50);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        assert!(!plan.unwrap().hit_regions.iter().any(|region| matches!(
            region.target,
            HitTarget::OlderResults | HitTarget::OlderAcceptances
        )));
    }

    #[test]
    fn changes_detail_exposes_app_files_and_never_claims_task_ownership() {
        let workspace = bone_app::WorkspaceId::new();
        let mut state = UiState {
            workspace: Some((workspace, "project".into())),
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Changes,
                title: "工作区变更".into(),
            }),
            ..UiState::default()
        };
        state.workspace_changes.page = Some(bone_app::WorkspaceChangePage {
            baseline: bone_app::WorkspaceBaseline::Git {
                head: Some("1234567890abcdef".into()),
            },
            files: vec![bone_app::WorkspaceChangedFile {
                path: "src/changed.rs".into(),
                tracked: true,
                index: bone_app::GitFileState::Unchanged,
                worktree: bone_app::GitFileState::Modified,
            }],
            next_cursor: None,
        });
        let backend = TestBackend::new(160, 50);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        assert!(
            plan.hit_regions
                .iter()
                .any(|region| region.target == HitTarget::DetailChanges)
        );
        assert!(
            plan.hit_regions
                .iter()
                .any(|region| region.target == HitTarget::WorkspaceChange(0))
        );
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("src/changed.rs"));
        let visible = text.replace(' ', "");
        assert!(visible.contains("可能包含你或"));
        assert!(visible.contains("其他任务的修改"));
        assert!(visible.contains("不把文件变更"), "{visible}");
        assert!(visible.contains("猜成某个任务的产物"), "{visible}");
    }

    #[test]
    fn workspace_file_window_exposes_visible_keyboard_and_mouse_controls() {
        let mut state = UiState {
            focus: Focus::Detail,
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Changes,
                title: "工作区变更".into(),
            }),
            ..UiState::default()
        };
        state.workspace_changes.file = Some(bone_app::WorkspaceFilePage {
            baseline: bone_app::WorkspaceBaseline::NotGit,
            path: "large.txt".into(),
            source: bone_app::WorkspaceFileSource::WorkingTree,
            media: bone_app::WorkspaceFileMedia::Text,
            text: Some("second window".into()),
            offset: 262_144,
            bytes_read: 13,
            total_bytes: Some(262_157),
            next_cursor: None,
        });
        state.workspace_changes.file_back.push(None);
        state.workspace_changes.file_control_selection = 0;

        let backend = TestBackend::new(160, 50);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        assert!(
            plan.hit_regions
                .iter()
                .any(|region| region.target == HitTarget::PreviousWorkspaceFile)
        );
        assert!(
            plan.hit_regions
                .iter()
                .any(|region| region.target == HitTarget::CloseWorkspaceFile)
        );
        assert!(
            !plan
                .hit_regions
                .iter()
                .any(|region| region.target == HitTarget::MoreWorkspaceFile)
        );
        let visible = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .replace(' ', "");
        assert!(visible.contains("上一窗口"));
        assert!(visible.contains("继续读取"));
        assert!(visible.contains("返回文件列表"));
        assert!(visible.contains("262144–262157/262157"), "{visible}");
    }

    #[test]
    fn artifact_detail_renders_explicit_private_evidence_without_inventing_content() {
        let session = bone_app::SessionId::new();
        let result = bone_app::ResultRef {
            session,
            job: bone_app::JobRef {
                runtime: bone_app::RuntimeId::new(),
                id: 1,
            },
            version: bone_app::SessionSeq(9),
        };
        let mut state = UiState {
            selected: Some(session),
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Artifacts,
                title: "产物与证据".into(),
            }),
            ..UiState::default()
        };
        state.artifact.artifact = Some(bone_app::ResultArtifact {
            result,
            outcome: bone_app::OutcomeKind::Completed,
            summary: "持久结果".into(),
            remaining: Vec::new(),
            evidence_count: 1,
        });
        state.artifact.evidence = Some(bone_app::EvidencePage {
            result,
            items: vec![bone_app::EvidenceSummary {
                source: bone_app::EvidenceRef { session, record: 7 },
                availability: bone_app::EvidenceAvailability::Private,
            }],
            next_cursor: None,
            projection_pending: false,
        });
        let backend = TestBackend::new(160, 50);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(render(frame, &state)))
            .unwrap();
        assert!(plan.unwrap().hit_regions.iter().any(|region| {
            region.target == HitTarget::DetailArtifacts || region.target == HitTarget::Evidence(0)
        }));
        let visible = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .replace(' ', "");
        assert!(visible.contains("持久结果"));
        assert!(visible.contains("私有来源"));
    }
}
