use super::*;

pub(super) fn render_dialog(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    state: &UiState,
) -> Option<HitRegion> {
    let (Some(area), Some(dialog)) = (plan.dialog_layer, state.dialog.as_ref()) else {
        return None;
    };
    let (title, body, accent) = match dialog {
        Dialog::NewSession { title } => (
            "新建会话",
            format!(
                "名称\n\n{}\n\nEnter 创建 · Esc 取消",
                sanitize_external(title)
            ),
            ACCENT,
        ),
        Dialog::RenameSession {
            title, submitting, ..
        } => (
            "重命名会话",
            format!(
                "新名称\n\n{}\n\n点击“确认”保存 · Esc 取消{}",
                sanitize_external(title),
                if *submitting {
                    " · 正在等待 App…"
                } else {
                    ""
                }
            ),
            ACCENT,
        ),
        Dialog::ConfirmQuit => (
            "确认退出",
            "退出前会保存草稿并让 App 收尾。\n\nEnter 确认退出 · Esc 返回".into(),
            ATTENTION,
        ),
        Dialog::Error(message) => (
            "发生问题",
            format!("{}\n\nEnter 或 Esc 关闭", sanitize_external(message)),
            DANGER,
        ),
        Dialog::Acceptance {
            result,
            decision,
            reason,
            rework,
            editing_rework,
            submitting,
            ..
        } => {
            let decision_label = match decision {
                bone_app::AcceptanceDecision::Accepted => "接受",
                bone_app::AcceptanceDecision::PartiallyAccepted => "部分接受",
                bone_app::AcceptanceDecision::AcceptedWithRisk => "带风险接受",
                bone_app::AcceptanceDecision::Rejected => "退回返工",
            };
            let reason_marker = if !*editing_rework { "›" } else { " " };
            let rework_marker = if *editing_rework { "›" } else { " " };
            let reason_hint = if *decision == bone_app::AcceptanceDecision::Accepted {
                "可选"
            } else {
                "必填"
            };
            let body = if *decision == bone_app::AcceptanceDecision::Rejected {
                format!(
                    "结果版本：{}\n判定：{decision_label}\n\n{reason_marker} 理由（必填）  {}\n{rework_marker} 新的返工要求（必填）  {}\n\nTab 切换输入框 · Enter 保存 · Esc 取消{}",
                    result.version.0,
                    sanitize_external(reason),
                    sanitize_external(rework),
                    if *submitting {
                        " · 正在保存…"
                    } else {
                        ""
                    },
                )
            } else {
                format!(
                    "结果版本：{}\n判定：{decision_label}\n\n{reason_marker} 理由（{reason_hint}）  {}\n\n输入理由 · Enter 保存 · Esc 取消{}",
                    result.version.0,
                    sanitize_external(reason),
                    if *submitting {
                        " · 正在保存…"
                    } else {
                        ""
                    },
                )
            };
            (
                "用户验收",
                body,
                if *decision == bone_app::AcceptanceDecision::Rejected {
                    DANGER
                } else {
                    ACCENT
                },
            )
        }
        Dialog::ModelConfig {
            role,
            profile,
            model,
            submitting,
            ..
        } => (
            "配置模型",
            format!(
                "角色：{}\n连接：{}\n\n模型名称\n{}\n\nEnter 保存并应用 · Esc 取消{}",
                match role {
                    crate::state::ModelRole::Worker => "Worker",
                    crate::state::ModelRole::Coordinator => "Coordinator",
                },
                profile,
                sanitize_external(model),
                if *submitting {
                    " · 正在等待 App…"
                } else {
                    ""
                }
            ),
            ACCENT,
        ),
        Dialog::ApiKey {
            profile,
            key,
            submitting,
            ..
        } => (
            "设置 API Key",
            format!(
                "连接：{profile}\n\nAPI Key\n{}\n\n输入值不会显示、记录或回填 · Enter 安全保存 · Esc 取消{}",
                "•".repeat(key.grapheme_count().min(48)),
                if *submitting {
                    " · 正在等待 App…"
                } else {
                    ""
                }
            ),
            ATTENTION,
        ),
        Dialog::ResolveWrite {
            external_effect,
            evidence,
            submitting,
            ..
        } => (
            "保存核查结果",
            format!(
                "事实：{}\n\n核查依据（必填）\n{}\n\nEnter 保存 · Esc 取消{}",
                match external_effect {
                    bone_app::ExternalEffect::Applied => "外部写入已经发生",
                    bone_app::ExternalEffect::None => "外部写入没有发生",
                    bone_app::ExternalEffect::Unknown => "仍然未知",
                },
                sanitize_external(evidence),
                if *submitting {
                    " · 正在等待 App…"
                } else {
                    ""
                }
            ),
            ATTENTION,
        ),
    };
    Some(components::dialog_frame::render(
        frame,
        area,
        DialogFrameProps {
            title,
            body,
            accent,
            foreground: INK,
            background: RAIL,
        },
    ))
}
