//! Durable local-command effects for the product TUI.
//!
//! This module translates typed slash commands into settings and session-store
//! effects. It intentionally does not own runtime connection or terminal-loop
//! state; results return through the same reducer event boundary as every
//! other product effect.

use std::collections::HashMap;

use crate::{
    ModelResolution, ModelSelection, Scope, SessionLifecycle, SettingsService, WorkspaceApplication,
};

use super::{
    app::{App, AppEvent, UiSessionId},
    commands::{ConfigCommand, LocalCommand, ModelCommand},
    report_notice,
    session_controller::{
        DurableUiSession, create_session, persist_model_readiness, require_writer_lease,
    },
};

/// The dependencies of a local-command effect. Keeping them together makes
/// command execution an explicit boundary instead of a growing positional
/// parameter list.
pub(super) struct CommandContext<'a> {
    pub(super) application: &'a WorkspaceApplication,
    pub(super) settings: &'a Option<SettingsService>,
    pub(super) durable: &'a mut HashMap<UiSessionId, DurableUiSession>,
    pub(super) show_progress: bool,
    pub(super) next_ui_id: &'a mut u64,
    pub(super) app: &'a mut App,
}

pub(super) fn handle_command(
    context: &mut CommandContext<'_>,
    id: UiSessionId,
    command: LocalCommand,
) -> bool {
    let Some(session) = context.durable.get_mut(&id) else {
        report_notice(
            context.app,
            "This conversation is no longer available in this workspace",
        );
        return false;
    };
    if let Err(error) = require_writer_lease(session) {
        report_notice(context.app, error);
        return false;
    }
    match command {
        LocalCommand::Help => {
            report_notice(
                context.app,
                "/model <id> · /model default <id> · /model global <id> · /model inherit · /new · /rename <title> · /status",
            );
            false
        }
        LocalCommand::Status => {
            let model = context
                .settings
                .as_ref()
                .and_then(|service| {
                    service
                        .resolve_model(session.record.workspace_id, session.record.id)
                        .ok()
                })
                .and_then(|resolution| resolution.task_config())
                .and_then(|task| task.model)
                .unwrap_or_else(|| "model not configured".into());
            report_notice(
                context.app,
                format!("Workspace session {} · solver {model}", session.record.id),
            );
            false
        }
        LocalCommand::Workspace => {
            report_notice(
                context.app,
                format!(
                    "Workspace: {}",
                    context.application.workspace().display_root().display()
                ),
            );
            false
        }
        LocalCommand::Sessions => {
            let _ = context.app.reduce(AppEvent::FocusSessions);
            false
        }
        LocalCommand::New => {
            create_session(
                context.application,
                context.durable,
                context.app,
                context.show_progress,
                context.next_ui_id,
                context.settings.as_ref(),
            );
            false
        }
        LocalCommand::Rename(title) => {
            match context.application.sessions().rename(
                session.record.id,
                session.record.revision,
                title,
            ) {
                Ok(record) => {
                    let _ = context.app.reduce(AppEvent::DurableTitleChanged {
                        id,
                        title: record.metadata.title.clone(),
                    });
                    session.record = record;
                    report_notice(context.app, "Conversation renamed");
                }
                Err(error) => {
                    report_notice(
                        context.app,
                        format!("Could not rename conversation: {error}"),
                    );
                }
            }
            false
        }
        LocalCommand::Archive => {
            let mut record = session.record.clone();
            record.status.lifecycle = SessionLifecycle::Archived;
            match context
                .application
                .sessions()
                .replace(record, session.record.revision)
            {
                Ok(record) => {
                    session.record = record;
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
            false
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
            false
        }
        LocalCommand::Model(command) => {
            let changed =
                apply_model_command(context.settings.as_ref(), session, id, command, context.app);
            if changed && let Some(settings) = context.settings.as_ref() {
                refresh_model_readiness(
                    context.application,
                    settings,
                    context.durable,
                    context.app,
                );
            }
            false
        }
        LocalCommand::Config(ConfigCommand::Open) => {
            report_notice(
                context.app,
                "Settings are live: use /model, /model default, /model global, or /model inherit",
            );
            false
        }
        LocalCommand::Config(ConfigCommand::Doctor) => {
            report_notice(
                context.app,
                if context.settings.is_some() {
                    "Settings storage is available"
                } else {
                    "Settings need repair; runtime attachment is disabled"
                },
            );
            false
        }
        LocalCommand::Login => {
            if context.settings.is_none() {
                report_notice(context.app, "Settings need repair before login can start");
                false
            } else {
                true
            }
        }
        LocalCommand::Logout => {
            report_notice(
                context.app,
                "Logout will be added with the credential lifecycle; active sessions are never disconnected silently",
            );
            false
        }
        // These two are intercepted by the input reducer before they reach
        // this executor, but retaining explicit arms keeps the registry total.
        LocalCommand::Stop | LocalCommand::Exit => false,
    }
}

fn apply_model_command(
    settings: Option<&SettingsService>,
    session: &mut DurableUiSession,
    ui_id: UiSessionId,
    command: ModelCommand,
    app: &mut App,
) -> bool {
    let Some(settings) = settings else {
        report_notice(
            app,
            "Settings need repair before model selection is available",
        );
        return false;
    };
    let (scope, model) = match command {
        ModelCommand::Open { .. } => {
            report_notice(
                app,
                "Choose with /model <id>; use /model default <id> or /model global <id> for inherited defaults",
            );
            return false;
        }
        ModelCommand::SetSession { model } => (Scope::Session(session.record.id), model),
        ModelCommand::SetWorkspaceDefault { model } => {
            (Scope::Workspace(session.record.workspace_id), model)
        }
        ModelCommand::SetUserDefault { model } => (Scope::User, model),
        ModelCommand::Inherit => {
            match settings.inherit_session_model(session.record.workspace_id, session.record.id) {
                Ok(resolution) => {
                    match resolution {
                        ModelResolution::Ready { resolved, .. } => {
                            let _ = app.reduce(AppEvent::SessionModelReadiness {
                                id: ui_id,
                                ready: true,
                            });
                            report_notice(
                                app,
                                format!(
                                    "Session now inherits {}. An already attached runtime keeps its pinned model; a new runtime uses this saved selection.",
                                    resolved.selection.model,
                                ),
                            );
                        }
                        ModelResolution::NeedsModel { .. } => {
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
    let selection = match ModelSelection::new(model, None, None) {
        Ok(selection) => selection,
        Err(error) => {
            report_notice(app, error.to_string());
            return false;
        }
    };
    match settings.set_solver_model(
        session.record.workspace_id,
        session.record.id,
        scope,
        selection,
    ) {
        Ok(change) => {
            let _ = app.reduce(AppEvent::SessionModelReadiness {
                id: ui_id,
                ready: true,
            });
            report_notice(
                app,
                format!(
                    "Saved solver {} at {:?} scope. An already attached runtime keeps its pinned model; a new or recreated runtime uses this saved selection.",
                    change.resolved.selection.model,
                    change.scope.kind()
                ),
            );
            true
        }
        Err(error) => {
            report_notice(app, format!("Could not save model selection: {error}"));
            false
        }
    }
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
                .resolve_model(session.record.workspace_id, session.record.id)
                .is_ok_and(|resolution| matches!(resolution, ModelResolution::Ready { .. }));
            (*id, ready, session.writer_lease.is_some())
        })
        .collect::<Vec<_>>();
    for (id, ready, owns_writer_lease) in readiness {
        let _ = app.reduce(AppEvent::SessionModelReadiness { id, ready });
        // The setting itself is durable and globally visible immediately, but
        // a background SessionRecord belongs to its writer process. Its small
        // readiness projection will be reconciled when this process later
        // acquires that session's lease.
        if owns_writer_lease
            && let Err(error) = persist_model_readiness(application, durable, id, ready)
        {
            report_notice(
                app,
                format!("Could not save model readiness for a conversation: {error}"),
            );
        }
    }
}
