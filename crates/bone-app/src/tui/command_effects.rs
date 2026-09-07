//! Durable local-command effects for the product TUI.
//!
//! This module translates typed slash commands into settings and session-store
//! effects. It intentionally does not own runtime connection or terminal-loop
//! state; results return through the same reducer event boundary as every
//! other product effect.

use std::collections::{BTreeSet, HashMap};

use bone_llm::{EndpointConfig, ModelOptions, protocol::openai_responses::Reasoning};

use crate::{
    ApiKeyCredentialError, ApiKeyCredentials, LlmProfile, LlmProfileId, ModelResolution,
    ModelSelection, Scope, SessionLifecycle, SettingsService, WorkspaceApplication,
};

use super::{
    app::{App, AppEvent, UiSessionId},
    commands::{
        ConfigCommand, LocalCommand, ModelCommand, ModelTuning, ProviderCommand, ProviderProtocol,
    },
    report_notice,
    session_controller::{
        DurableUiSession, create_session, persist_model_readiness, require_writer,
    },
};

/// The dependencies of a local-command effect. Keeping them together makes
/// command execution an explicit boundary instead of a growing positional
/// parameter list.
pub(super) struct CommandContext<'a> {
    pub(super) application: &'a WorkspaceApplication,
    pub(super) settings: &'a SettingsService,
    pub(super) durable: &'a mut HashMap<UiSessionId, DurableUiSession>,
    pub(super) show_progress: bool,
    pub(super) next_ui_id: &'a mut u64,
    pub(super) app: &'a mut App,
}

/// Runtime work requested by a local command after its durable changes have
/// completed. The terminal loop owns these asynchronous effects.
pub(super) enum CommandRuntimeEffect {
    None,
    /// Retry only the invoking conversation's already durable pending turn.
    /// A local command must never replay another conversation's text.
    RetryPending {
        id: UiSessionId,
    },
    /// Authenticate once, then retry the invoking conversation if it has a
    /// pending turn. The terminal loop may coalesce simultaneous logins.
    AuthenticateChatGpt {
        id: UiSessionId,
        profile: LlmProfile,
    },
    /// Clear the ChatGPT credential cache in the terminal runtime layer.
    LogoutChatGpt,
}

pub(super) fn handle_command(
    context: &mut CommandContext<'_>,
    id: UiSessionId,
    command: LocalCommand,
) -> CommandRuntimeEffect {
    let Some(session) = context.durable.get_mut(&id) else {
        report_notice(
            context.app,
            "This conversation is no longer available in this workspace",
        );
        return CommandRuntimeEffect::None;
    };
    match command {
        LocalCommand::Help => {
            report_notice(
                context.app,
                "/provider · /model [profile] <id> [--timeout …] [--reasoning-…] · /model default · /model global · /model coordinator · /model inherit · /login · /logout · /new · /rename <title> · /status",
            );
            CommandRuntimeEffect::None
        }
        LocalCommand::Status => {
            let models = context
                .settings
                .resolve_model(&session.record)
                .ok()
                .and_then(|resolution| match resolution {
                    ModelResolution::Ready(runtime) => Some(format!(
                        "coordinator {} via {} · solver {} via {}",
                        runtime.coordinator.selection.model,
                        runtime.coordinator.profile.id,
                        runtime.solver.selection.model,
                        runtime.solver.profile.id,
                    )),
                    ModelResolution::NeedsModel => None,
                })
                .unwrap_or_else(|| "models not configured".into());
            report_notice(
                context.app,
                format!("Workspace session {} · {models}", session.record.id),
            );
            CommandRuntimeEffect::None
        }
        LocalCommand::Workspace => {
            report_notice(
                context.app,
                format!(
                    "Workspace: {}",
                    context.application.workspace().display_root().display()
                ),
            );
            CommandRuntimeEffect::None
        }
        LocalCommand::Sessions => {
            let _ = context.app.reduce(AppEvent::FocusSessions);
            CommandRuntimeEffect::None
        }
        LocalCommand::New => {
            create_session(
                context.application,
                context.durable,
                context.app,
                context.show_progress,
                context.next_ui_id,
                context.settings,
            );
            CommandRuntimeEffect::None
        }
        LocalCommand::Rename(title) => {
            if let Err(error) = require_writer(session) {
                report_notice(context.app, error);
                return CommandRuntimeEffect::None;
            }
            match session
                .writer
                .as_mut()
                .expect("writer was required")
                .rename(title)
            {
                Ok(()) => {
                    session.record = session
                        .writer
                        .as_ref()
                        .expect("writer was required")
                        .record()
                        .clone();
                    let _ = context.app.reduce(AppEvent::DurableTitleChanged {
                        id,
                        title: session.record.metadata.title.clone(),
                    });
                    report_notice(context.app, "Conversation renamed");
                }
                Err(error) => {
                    report_notice(
                        context.app,
                        format!("Could not rename conversation: {error}"),
                    );
                }
            }
            CommandRuntimeEffect::None
        }
        LocalCommand::Archive => {
            if let Err(error) = require_writer(session) {
                report_notice(context.app, error);
                return CommandRuntimeEffect::None;
            }
            let mut record = session.record.clone();
            record.status.lifecycle = SessionLifecycle::Archived;
            match session
                .writer
                .as_mut()
                .expect("writer was required")
                .replace(record)
            {
                Ok(()) => {
                    session.record = session
                        .writer
                        .as_ref()
                        .expect("writer was required")
                        .record()
                        .clone();
                    report_notice(
                        context.app,
                        "Conversation archived; it will be hidden on next launch",
                    );
                }
                Err(error) => {
                    report_notice(
                        context.app,
                        format!("Could not archive conversation: {error}"),
                    );
                }
            }
            CommandRuntimeEffect::None
        }
        LocalCommand::Resume(query) => {
            let needle = query.unwrap_or_default().to_ascii_lowercase();
            if let Some((candidate, _)) = context.durable.iter().find(|(_, candidate)| {
                needle.is_empty()
                    || candidate
                        .record
                        .metadata
                        .title
                        .to_ascii_lowercase()
                        .contains(&needle)
                    || candidate.record.id.to_string().starts_with(&needle)
            }) {
                let _ = context
                    .app
                    .reduce(AppEvent::SessionSelected { id: *candidate });
            } else {
                report_notice(
                    context.app,
                    "No matching saved conversation in this workspace",
                );
            }
            CommandRuntimeEffect::None
        }
        LocalCommand::Model(command) => {
            let changed = apply_model_command(
                context.application,
                context.settings,
                session,
                id,
                command,
                context.app,
            );
            if changed {
                refresh_model_readiness(
                    context.application,
                    context.settings,
                    context.durable,
                    context.app,
                );
            }
            CommandRuntimeEffect::None
        }
        LocalCommand::Provider(command) => {
            apply_provider_command(context.settings, command, context.app);
            CommandRuntimeEffect::None
        }
        LocalCommand::Config(ConfigCommand::Open) => {
            report_notice(
                context.app,
                "Settings are live: use /provider, /provider add, /model, /model default, /model global, /model coordinator, or /model inherit",
            );
            CommandRuntimeEffect::None
        }
        LocalCommand::Config(ConfigCommand::Doctor) => {
            report_notice(context.app, "Settings storage is available");
            CommandRuntimeEffect::None
        }
        LocalCommand::Login => match context.settings.resolve_model(&session.record) {
            Ok(ModelResolution::Ready(runtime)) => {
                let profiles = [&runtime.solver.profile, &runtime.coordinator.profile];
                if let Some(message) = api_key_prerequisite_notice(profiles) {
                    report_notice(context.app, message);
                    CommandRuntimeEffect::None
                } else if let Some(profile) = profiles.into_iter().find(|profile| {
                    matches!(&profile.endpoint, EndpointConfig::ChatGptSubscription)
                }) {
                    CommandRuntimeEffect::AuthenticateChatGpt {
                        id,
                        profile: profile.clone(),
                    }
                } else {
                    let mut profiles = profiles
                        .into_iter()
                        .map(|profile| profile.id.to_string())
                        .collect::<Vec<_>>();
                    profiles.sort();
                    profiles.dedup();
                    report_notice(
                        context.app,
                        format!(
                            "Selected API-key profile{}: {}. Set missing keys with {}; /login retries this conversation's saved message.",
                            if profiles.len() == 1 { "" } else { "s" },
                            profiles.join(", "),
                            profiles
                                .iter()
                                .map(|profile| format!("`bone credentials set {profile}`"))
                                .collect::<Vec<_>>()
                                .join(" or "),
                        ),
                    );
                    CommandRuntimeEffect::RetryPending { id }
                }
            }
            Ok(ModelResolution::NeedsModel) => {
                // Authentication belongs to the User scope, not to a chosen
                // model or conversation. The built-in subscription profile is
                // always available even on a first-run empty configuration.
                CommandRuntimeEffect::AuthenticateChatGpt {
                    id,
                    profile: LlmProfile::chatgpt_subscription(),
                }
            }
            Err(error) => {
                report_notice(
                    context.app,
                    format!("Could not resolve selected profiles: {error}"),
                );
                CommandRuntimeEffect::None
            }
        },
        LocalCommand::Logout => CommandRuntimeEffect::LogoutChatGpt,
        // These two are intercepted by the input reducer before they reach
        // this executor, but retaining explicit arms keeps the registry total.
        LocalCommand::Stop | LocalCommand::Exit => CommandRuntimeEffect::None,
    }
}

/// Return a repair instruction before `/login` starts any OAuth flow. A mixed
/// coordinator/solver configuration needs every API key first; otherwise a
/// successful ChatGPT login would only lead to a second, misleading failure.
fn api_key_prerequisite_notice(profiles: [&LlmProfile; 2]) -> Option<String> {
    let mut missing = BTreeSet::new();
    let mut unavailable = BTreeSet::new();
    for profile in profiles {
        if matches!(&profile.endpoint, EndpointConfig::ChatGptSubscription) {
            continue;
        }
        match ApiKeyCredentials::for_profile(profile).and_then(|credentials| credentials.read()) {
            Ok(_) => {}
            Err(ApiKeyCredentialError::MissingApiKey) => {
                missing.insert(profile.id.to_string());
            }
            Err(ApiKeyCredentialError::InvalidApiKey | ApiKeyCredentialError::Unavailable) => {
                unavailable.insert(profile.id.to_string());
            }
        }
    }
    if !missing.is_empty() {
        let profiles = missing.into_iter().collect::<Vec<_>>();
        let commands = profiles
            .iter()
            .map(|profile| format!("`bone credentials set {profile}`"))
            .collect::<Vec<_>>()
            .join(" or ");
        return Some(format!(
            "API key{} missing for {}. Run {}, then use /login to retry this conversation.",
            if profiles.len() == 1 { " is" } else { "s are" },
            profiles.join(", "),
            commands,
        ));
    }
    if !unavailable.is_empty() {
        return Some(format!(
            "Could not read the API key for {}. Check the operating-system credential store before retrying.",
            unavailable.into_iter().collect::<Vec<_>>().join(", "),
        ));
    }
    None
}

fn apply_provider_command(settings: &SettingsService, command: ProviderCommand, app: &mut App) {
    match command {
        ProviderCommand::List => match settings.llm_profiles() {
            Ok(profiles) if profiles.profiles.is_empty() => {
                report_notice(app, "No saved LLM profiles");
            }
            Ok(profiles) => {
                let summary = profiles
                    .profiles
                    .iter()
                    .map(|profile| {
                        let protocol = match &profile.endpoint {
                            EndpointConfig::ChatGptSubscription => "ChatGPT subscription".into(),
                            EndpointConfig::OpenAiResponses { base_url: None } => {
                                "OpenAI Responses".into()
                            }
                            EndpointConfig::OpenAiResponses {
                                base_url: Some(base_url),
                            } => format!("OpenAI Responses · {base_url}"),
                            EndpointConfig::OpenAiChatCompletions { base_url: None } => {
                                "OpenAI Chat Completions".into()
                            }
                            EndpointConfig::OpenAiChatCompletions {
                                base_url: Some(base_url),
                            } => format!("OpenAI Chat Completions · {base_url}"),
                            EndpointConfig::AnthropicMessages { base_url: None } => {
                                "Anthropic Messages".into()
                            }
                            EndpointConfig::AnthropicMessages {
                                base_url: Some(base_url),
                            } => format!("Anthropic Messages · {base_url}"),
                        };
                        format!("{} ({protocol})", profile.id)
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                report_notice(app, format!("Profiles: {summary}"));
            }
            Err(error) => report_notice(app, format!("Could not list profiles: {error}")),
        },
        ProviderCommand::Add {
            id,
            protocol,
            base_url,
        } => {
            let id = match LlmProfileId::new(id) {
                Ok(id) => id,
                Err(error) => {
                    report_notice(app, error.to_string());
                    return;
                }
            };
            let endpoint = match protocol {
                ProviderProtocol::OpenAiResponses => EndpointConfig::OpenAiResponses { base_url },
                ProviderProtocol::OpenAiChatCompletions => {
                    EndpointConfig::OpenAiChatCompletions { base_url }
                }
                ProviderProtocol::AnthropicMessages => {
                    EndpointConfig::AnthropicMessages { base_url }
                }
            };
            let profile = match LlmProfile::new(id.clone(), id.as_str(), endpoint) {
                Ok(profile) => profile,
                Err(error) => {
                    report_notice(app, error.to_string());
                    return;
                }
            };
            match settings.add_llm_profile(profile) {
                Ok(()) => report_notice(
                    app,
                    format!(
                        "Saved `{id}`. Set its API key securely with `bone credentials set {id}`, then select it with /model {id} <model>."
                    ),
                ),
                Err(error) => report_notice(app, format!("Could not save profile: {error}")),
            }
        }
    }
}

fn apply_model_command(
    _application: &WorkspaceApplication,
    settings: &SettingsService,
    session: &mut DurableUiSession,
    ui_id: UiSessionId,
    command: ModelCommand,
    app: &mut App,
) -> bool {
    let (scope, profile, model, tuning, is_coordinator) = match command {
        ModelCommand::Open { .. } => {
            report_notice(
                app,
                "Choose with /model [profile] <id> [--timeout seconds] [--reasoning-* value]; use /model default, /model global, or /model coordinator for saved defaults",
            );
            return false;
        }
        ModelCommand::SetSession {
            profile,
            model,
            tuning,
        } => (
            Scope::Session(session.record.id),
            profile,
            model,
            tuning,
            false,
        ),
        ModelCommand::SetWorkspaceDefault {
            profile,
            model,
            tuning,
        } => (
            Scope::Workspace(session.record.workspace_id),
            profile,
            model,
            tuning,
            false,
        ),
        ModelCommand::SetUserDefault {
            profile,
            model,
            tuning,
        } => (Scope::User, profile, model, tuning, false),
        ModelCommand::SetCoordinator {
            profile,
            model,
            tuning,
        } => (Scope::User, profile, model, tuning, true),
        ModelCommand::Inherit => {
            if let Err(error) = require_writer(session) {
                report_notice(app, error);
                return false;
            }
            match session
                .writer
                .as_mut()
                .expect("writer was required")
                .set_solver_model_override(None)
            {
                Ok(()) => {
                    session.record = session
                        .writer
                        .as_ref()
                        .expect("writer was required")
                        .record()
                        .clone();
                    let resolution = match settings.resolve_model(&session.record) {
                        Ok(resolution) => resolution,
                        Err(error) => {
                            report_notice(
                                app,
                                format!(
                                    "Model inheritance was saved, but the effective model could not be resolved: {error}"
                                ),
                            );
                            return false;
                        }
                    };
                    match resolution {
                        ModelResolution::Ready(runtime) => {
                            let _ = app.reduce(AppEvent::SessionModelReadiness {
                                id: ui_id,
                                ready: true,
                            });
                            report_notice(
                                app,
                                format!(
                                    "Session now inherits {}. An already attached runtime keeps its pinned model; a new runtime uses this saved selection.",
                                    runtime.solver.selection.model,
                                ),
                            );
                        }
                        ModelResolution::NeedsModel => {
                            let _ = app.reduce(AppEvent::SessionNeedsSetup {
                                id: ui_id,
                                message: "Choose a model with /model <id> to begin".into(),
                            });
                        }
                    }
                    return true;
                }
                Err(error) => {
                    report_notice(app, format!("Could not restore model inheritance: {error}"));
                    return false;
                }
            }
        }
    };
    let selection_result = match profile {
        Some(profile) => {
            let profile = match LlmProfileId::new(profile) {
                Ok(profile) => profile,
                Err(error) => {
                    report_notice(app, error.to_string());
                    return false;
                }
            };
            ModelSelection::new(
                profile,
                model,
                model_options_from_tuning(&tuning),
                tuning.timeout_seconds,
            )
        }
        None => ModelSelection::new(
            LlmProfileId::chatgpt(),
            model,
            model_options_from_tuning(&tuning),
            tuning.timeout_seconds,
        ),
    };
    let selection = match selection_result {
        Ok(selection) => selection,
        Err(error) => {
            report_notice(app, error.to_string());
            return false;
        }
    };
    if let Err(error) = settings.validate_model_selection(&selection) {
        report_notice(app, format!("Could not save model selection: {error}"));
        return false;
    }
    if is_coordinator {
        if let Err(error) = settings.set_coordinator_model(selection) {
            report_notice(
                app,
                format!("Could not save coordinator selection: {error}"),
            );
            return false;
        }
    } else {
        match scope {
            Scope::Session(_) => {
                if let Err(error) = require_writer(session) {
                    report_notice(app, error);
                    return false;
                }
                match session
                    .writer
                    .as_mut()
                    .expect("writer was required")
                    .set_solver_model_override(Some(selection))
                {
                    Ok(()) => {
                        session.record = session
                            .writer
                            .as_ref()
                            .expect("writer was required")
                            .record()
                            .clone();
                    }
                    Err(error) => {
                        report_notice(app, format!("Could not save model selection: {error}"));
                        return false;
                    }
                }
            }
            Scope::User | Scope::Workspace(_) => {
                match settings.set_solver_model(session.record.workspace_id, scope, selection) {
                    Ok(()) => {}
                    Err(error) => {
                        report_notice(app, format!("Could not save model selection: {error}"));
                        return false;
                    }
                }
            }
        }
    }
    let resolution = match settings.resolve_model(&session.record) {
        Ok(ModelResolution::Ready(runtime)) if is_coordinator => runtime.coordinator.clone(),
        Ok(ModelResolution::Ready(runtime)) => runtime.solver.clone(),
        Ok(ModelResolution::NeedsModel) => {
            report_notice(app, "Could not resolve the saved model selection");
            return false;
        }
        Err(error) => {
            report_notice(
                app,
                format!("Could not resolve the saved model selection: {error}"),
            );
            return false;
        }
    };
    let _ = app.reduce(AppEvent::SessionModelReadiness {
        id: ui_id,
        ready: true,
    });
    report_notice(
        app,
        format!(
            "Saved {} at {:?} scope; the effective {} is {}. An already attached runtime keeps its pinned model; a new or recreated runtime uses this saved selection.",
            if is_coordinator {
                "coordinator"
            } else {
                "solver"
            },
            scope.kind(),
            if is_coordinator {
                "coordinator"
            } else {
                "solver"
            },
            resolution.selection.model,
        ),
    );
    true
}

fn model_options_from_tuning(tuning: &ModelTuning) -> Option<ModelOptions> {
    tuning.has_reasoning().then(|| {
        let mut reasoning = Reasoning::new();
        if let Some(effort) = tuning.reasoning_effort {
            reasoning = reasoning.effort(effort);
        }
        if let Some(summary) = tuning.reasoning_summary {
            reasoning = reasoning.summary(summary);
        }
        if let Some(mode) = tuning.reasoning_mode {
            reasoning = reasoning.mode(mode);
        }
        if let Some(context) = tuning.reasoning_context {
            reasoning = reasoning.context(context);
        }
        ModelOptions::OpenAiResponses { reasoning }
    })
}

/// Model settings may be inherited by many logical sessions. Recompute their
/// model-derived setup gate immediately after a successful durable change so a
/// workspace/global selection does not leave other sessions falsely blocked.
fn refresh_model_readiness(
    application: &WorkspaceApplication,
    settings: &SettingsService,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    app: &mut App,
) {
    let readiness = durable
        .iter()
        .map(|(id, session)| {
            let ready = settings
                .resolve_model(&session.record)
                .is_ok_and(|resolution| matches!(resolution, ModelResolution::Ready(_)));
            (*id, ready, session.writer.is_some())
        })
        .collect::<Vec<_>>();
    for (id, ready, owns_writer) in readiness {
        let _ = app.reduce(AppEvent::SessionModelReadiness { id, ready });
        // The setting itself is durable and globally visible immediately, but
        // a background SessionRecord belongs to its writer process. Its small
        // readiness projection will be reconciled when this process later
        // acquires that session's lease.
        if owns_writer && let Err(error) = persist_model_readiness(application, durable, id, ready)
        {
            report_notice(
                app,
                format!("Could not save model readiness for a conversation: {error}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use bone_store::{BoneStore, StoreRoots};

    use super::*;
    use crate::{LlmProfileId, ModelSelection, WorkspaceApplication};

    fn install_mock_keyring() {
        static INSTALLED: Once = Once::new();
        INSTALLED.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    fn application() -> (tempfile::TempDir, tempfile::TempDir, WorkspaceApplication) {
        let data = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            data.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = BoneStore::open_at(StoreRoots::new(data.path().join("data")).unwrap()).unwrap();
        let application = WorkspaceApplication::open_with_store(project.path(), store).unwrap();
        (data, project, application)
    }

    #[test]
    fn session_model_mutation_keeps_the_ui_mirror_current() {
        let (_data, _project, application) = application();
        let opened = application.open_or_create_writer_draft().unwrap();
        let record = opened.draft.record;
        let mut writer = opened.writer;
        writer
            .set_solver_model_override(Some(ModelSelection::chatgpt("gpt-test", None).unwrap()))
            .unwrap();

        let mut session = DurableUiSession {
            record,
            journal: None,
            writer: Some(writer),
            next_turn: 1,
            active_turn: None,
            runtime_record_cursor: 0,
        };
        session.record = session.writer.as_ref().unwrap().record().clone();
        assert_eq!(
            session
                .record
                .metadata
                .solver_model_override
                .as_ref()
                .map(|selection| selection.model.as_str()),
            Some("gpt-test")
        );
    }

    #[test]
    fn login_preflight_names_a_missing_api_key_before_starting_oauth() {
        install_mock_keyring();
        let api_profile = LlmProfile::new(
            LlmProfileId::new("login-preflight-openai").unwrap(),
            "OpenAI",
            EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        let chatgpt = LlmProfile::chatgpt_subscription();

        let notice = api_key_prerequisite_notice([&api_profile, &chatgpt]).unwrap();
        assert!(notice.contains("login-preflight-openai"));
        assert!(notice.contains("bone credentials set login-preflight-openai"));
        assert!(notice.contains("then use /login"));
    }
}
