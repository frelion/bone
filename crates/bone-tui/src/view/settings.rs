use super::*;

pub(super) fn render_settings(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let settings = state
        .settings
        .as_ref()
        .filter(|settings| state.selected == Some(settings.session));
    let lines = match settings {
        Some(settings) => {
            let (desired_worker, desired_coordinator) = match &settings.resolved.desired {
                Ok(config) => (
                    format!(
                        "{} · {}",
                        sanitize_external(&config.worker.profile.label),
                        sanitize_external(&config.worker.selection.model)
                    ),
                    format!(
                        "{} · {}",
                        sanitize_external(&config.coordinator.profile.label),
                        sanitize_external(&config.coordinator.selection.model)
                    ),
                ),
                Err(problem) => {
                    let label = match problem {
                        bone_app::ConfigProblem::NeedsModel => "需要选择模型".into(),
                        bone_app::ConfigProblem::MissingProfile(profile) => {
                            format!("缺少连接配置：{profile}")
                        }
                        bone_app::ConfigProblem::Invalid(message) => {
                            format!("配置无效：{}", sanitize_external(message))
                        }
                    };
                    (label, "尚未解析".into())
                }
            };
            let running = settings.resolved.running.as_ref().map_or_else(
                || "当前没有运行中的配置".into(),
                |config| {
                    format!(
                        "当前生效：{} · {}",
                        sanitize_external(&config.worker.profile.label),
                        sanitize_external(&config.worker.selection.model)
                    )
                },
            );
            vec![
                Line::styled("保存的目标配置", Style::default().fg(MUTED)),
                Line::styled(
                    format!("Worker      {desired_worker}"),
                    Style::default().fg(INK),
                ),
                Line::styled(
                    format!("Coordinator {desired_coordinator}"),
                    Style::default().fg(INK),
                ),
                Line::raw(""),
                Line::styled(running, Style::default().fg(ACCENT)),
                Line::raw(""),
                Line::styled("连接档案（点击选择）", Style::default().fg(MUTED)),
                Line::raw(""),
                Line::styled(
                    "凭据只由 bone-app 管理，界面不会显示或回填完整值。",
                    Style::default().fg(MUTED),
                ),
            ]
        }
        _ => vec![Line::styled(
            "正在从 bone-app 读取设置…",
            Style::default().fg(MUTED),
        )],
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(section_block("设置")),
        area,
    );
    if let Some(profile) =
        settings.and_then(|settings| settings.profiles.get(state.settings_profile))
        && let Some(login) = state.login_states.get(&profile.id)
    {
        let prompt = match login {
            bone_app::LoginState::DeviceCode {
                verification_uri,
                user_code,
            } => Some((
                format!(
                    "登录地址：{}\n设备码：{}",
                    single_line_external(verification_uri),
                    single_line_external(user_code)
                ),
                ATTENTION,
            )),
            bone_app::LoginState::Failed { message } => Some((
                format!("登录失败：{}", single_line_external(message)),
                DANGER,
            )),
            _ => None,
        };
        if let Some((prompt, color)) = prompt {
            frame.render_widget(
                Paragraph::new(prompt)
                    .style(Style::default().fg(color).bg(PANEL))
                    .wrap(Wrap { trim: false }),
                settings_login_area(area),
            );
        }
    }
    if let Some(settings) = settings {
        for region in settings_action_regions(area, state) {
            let (label, selected, color) = match region.target {
                HitTarget::SettingsProfile(index) => {
                    let Some(profile) = settings.profiles.get(index) else {
                        continue;
                    };
                    let login = state.login_states.get(&profile.id).map(login_state_label);
                    (
                        format!(
                            "{} {}{}",
                            if index == state.settings_profile {
                                "›"
                            } else {
                                " "
                            },
                            single_line_external(&profile.label),
                            login.map(|value| format!(" · {value}")).unwrap_or_default()
                        ),
                        index == state.settings_profile,
                        ACCENT,
                    )
                }
                HitTarget::ConfigureWorker => ("[ 配置 Worker ]".into(), false, ACCENT),
                HitTarget::ConfigureCoordinator => ("[ 配置 Coordinator ]".into(), false, ACCENT),
                HitTarget::Login => {
                    let chatgpt =
                        settings
                            .profiles
                            .get(state.settings_profile)
                            .is_some_and(|profile| {
                                matches!(
                                    &profile.endpoint,
                                    bone_app::EndpointConfig::ChatGptSubscription
                                )
                            });
                    (
                        if chatgpt {
                            "[ 登录 ]".into()
                        } else {
                            "[ 设置 Key ]".into()
                        },
                        false,
                        ATTENTION,
                    )
                }
                HitTarget::Logout => ("[ 清除凭据 ]".into(), false, MUTED),
                _ => continue,
            };
            let keyboard_selected =
                state.focus == Focus::Timeline && state.focused_control == Some(region.target);
            frame.render_widget(
                Paragraph::new(label)
                    .alignment(if matches!(region.target, HitTarget::SettingsProfile(_)) {
                        Alignment::Left
                    } else {
                        Alignment::Center
                    })
                    .style(if keyboard_selected {
                        Style::default().fg(RAIL).bg(color)
                    } else {
                        Style::default().fg(color).bg(if selected {
                            Color::Rgb(38, 45, 53)
                        } else {
                            RAIL
                        })
                    }),
                region.area,
            );
        }
    }
}

pub(super) fn settings_action_regions(area: Rect, state: &UiState) -> Vec<HitRegion> {
    let Some(settings) = state
        .settings
        .as_ref()
        .filter(|settings| state.selected == Some(settings.session))
    else {
        return Vec::new();
    };
    let mut regions = Vec::new();
    let profile_start = settings_profile_start(area, state);
    let profile_bottom = area.bottom().saturating_sub(4);
    for (index, _) in settings.profiles.iter().enumerate() {
        let y = profile_start.saturating_add(index as u16);
        if y >= profile_bottom {
            break;
        }
        regions.push(HitRegion {
            area: Rect::new(area.x.saturating_add(2), y, area.width.saturating_sub(4), 1),
            target: HitTarget::SettingsProfile(index),
        });
    }
    if area.height >= 4 && !settings.profiles.is_empty() {
        let actions = Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 2);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(27),
                Constraint::Percentage(33),
                Constraint::Percentage(18),
                Constraint::Percentage(22),
            ])
            .split(actions);
        for (area, target) in columns.iter().copied().zip([
            HitTarget::ConfigureWorker,
            HitTarget::ConfigureCoordinator,
            HitTarget::Login,
            HitTarget::Logout,
        ]) {
            regions.push(HitRegion { area, target });
        }
    }
    regions
}

pub(super) fn settings_login_area(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(10),
        area.width.saturating_sub(4),
        3.min(area.height.saturating_sub(10)),
    )
}

pub(super) fn settings_profile_start(area: Rect, state: &UiState) -> u16 {
    let has_prompt = state
        .settings
        .as_ref()
        .filter(|settings| state.selected == Some(settings.session))
        .and_then(|settings| settings.profiles.get(state.settings_profile))
        .and_then(|profile| state.login_states.get(&profile.id))
        .is_some_and(|login| {
            matches!(
                login,
                bone_app::LoginState::DeviceCode { .. } | bone_app::LoginState::Failed { .. }
            )
        });
    area.y.saturating_add(if has_prompt { 14 } else { 10 })
}

pub(super) fn login_state_label(state: &bone_app::LoginState) -> &'static str {
    match state {
        bone_app::LoginState::Connecting => "正在连接",
        bone_app::LoginState::DeviceCode { .. } => "等待浏览器确认",
        bone_app::LoginState::Succeeded => "已登录",
        bone_app::LoginState::Failed { .. } => "登录失败",
        bone_app::LoginState::Cancelled => "未登录",
    }
}
