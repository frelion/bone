//! Durable local-command effects for the product TUI.
//!
//! This module translates typed slash commands into settings and session-store
//! effects. It intentionally does not own runtime connection or terminal-loop
//! state; results return through the same reducer event boundary as every
//! other product effect.

use std::collections::HashMap;

use bone_store::{ProviderAuthError, ProviderId};

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
    pub(super) settings: &'a SettingsService,
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
                .resolve_model(session.record.workspace_id, session.record.id)
                .ok()
                .and_then(|resolution| match resolution {
                    ModelResolution::Ready { resolved, .. } => Some(resolved.selection.model),
                    ModelResolution::NeedsModel => None,
                })
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
                context.settings,
            );
            false
        }
        LocalCommand::Rename(title) => {
            match context.application.sessions().rename(
                session
                    .writer_lease
                    .as_ref()
                    .expect("writer lease was required"),
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
            match context.application.sessions().replace(
                session
                    .writer_lease
                    .as_ref()
                    .expect("writer lease was required"),
                record,
                session.record.revision,
            ) {
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
            report_notice(context.app, "Settings storage is available");
            false
        }
        LocalCommand::Login => true,
        LocalCommand::Logout => {
            match context
                .application
                .store()
                .provider_auth()
                .clear(ProviderId::ChatGptSubscription)
            {
                Ok(()) => report_notice(
                    context.app,
                    "Local ChatGPT sign-in cache removed. The next connection will require login.",
                ),
                Err(ProviderAuthError::Busy) => report_notice(
                    context.app,
                    "Cannot log out while an active runtime owns the ChatGPT connection. Stop it or exit BONE, then retry.",
                ),
                Err(ProviderAuthError::Unavailable) => report_notice(
                    context.app,
                    "Could not access the local ChatGPT sign-in cache. Run /config doctor and repair storage before retrying.",
                ),
            }
            false
        }
        // These two are intercepted by the input reducer before they reach
        // this executor, but retaining explicit arms keeps the registry total.
        LocalCommand::Stop | LocalCommand::Exit => false,
    }
}

fn apply_model_command(
    application: &WorkspaceApplication,
    settings: &SettingsService,
    session: &mut DurableUiSession,
    ui_id: UiSessionId,
    command: ModelCommand,
    app: &mut App,
) -> bool {
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
            match application.sessions().set_solver_model_override(
                session
                    .writer_lease
                    .as_ref()
                    .expect("writer lease was required"),
                session.record.id,
                session.record.revision,
                None,
            ) {
                Ok(record) => {
                    session.record = record;
                    let resolution = match settings
                        .resolve_model(session.record.workspace_id, session.record.id)
                    {
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
    let selection = match ModelSelection::new(model, None, None) {
        Ok(selection) => selection,
        Err(error) => {
            report_notice(app, error.to_string());
            return false;
        }
    };
    let change = match scope {
        Scope::Session(_) => match application.sessions().set_solver_model_override(
            session
                .writer_lease
                .as_ref()
                .expect("writer lease was required"),
            session.record.id,
            session.record.revision,
            Some(selection),
        ) {
            Ok(record) => {
                session.record = record;
                match settings.resolve_model(session.record.workspace_id, session.record.id) {
                    Ok(ModelResolution::Ready { resolved, .. }) => {
                        crate::ModelChange { scope, resolved }
                    }
                    Ok(ModelResolution::NeedsModel) => {
                        report_notice(app, "Could not resolve the saved Session model");
                        return false;
                    }
                    Err(error) => {
                        report_notice(
                            app,
                            format!("Could not resolve the saved Session model: {error}"),
                        );
                        return false;
                    }
                }
            }
            Err(error) => {
                report_notice(app, format!("Could not save model selection: {error}"));
                return false;
            }
        },
        Scope::User | Scope::Workspace(_) => match settings.set_solver_model(
            session.record.workspace_id,
            session.record.id,
            scope,
            selection,
        ) {
            Ok(change) => change,
            Err(error) => {
                report_notice(app, format!("Could not save model selection: {error}"));
                return false;
            }
        },
    };
    if let Err(error) = refresh_session_record(application, session) {
        report_notice(
            app,
            format!(
                "Model selection was saved, but this conversation could not refresh its SQLite revision: {error}"
            ),
        );
        return false;
    }
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

/// A session-scoped model mutation writes the same SQLite document that the
/// TUI later uses for draft/status CAS. Refresh the local projection before
/// returning so `/model` cannot leave an otherwise healthy composer holding a
/// stale document revision and reject the next user turn.
fn refresh_session_record(
    application: &WorkspaceApplication,
    session: &mut DurableUiSession,
) -> Result<(), String> {
    let id = session.record.id;
    let record = application
        .sessions()
        .get(id)
        .map_err(|error| format!("could not reload conversation: {error}"))?
        .ok_or_else(|| "conversation disappeared after its model setting was saved".to_owned())?;
    session.record = record;
    Ok(())
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

#[cfg(test)]
mod tests {
    use bone_store::{BoneStore, StoreRoots};

    use super::*;
    use crate::{ModelSelection, SettingsService, WorkspaceApplication};

    fn application() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        WorkspaceApplication,
        SettingsService,
    ) {
        let data = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            data.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = BoneStore::open_at(
            StoreRoots::new(data.path().join("data"), data.path().join("config")).unwrap(),
        )
        .unwrap();
        let application =
            WorkspaceApplication::open_with_store(project.path(), store.clone()).unwrap();
        let settings = SettingsService::open(store).unwrap();
        (data, project, application, settings)
    }

    #[test]
    fn model_document_mutation_refreshes_the_tui_session_revision() {
        let (_data, _project, application, _settings) = application();
        let opened = application.open_or_create_writer_draft().unwrap();
        let record = opened.draft.record;
        let lease = opened.lease;
        let initial_revision = record.revision;
        application
            .sessions()
            .set_solver_model_override(
                &lease,
                record.id,
                record.revision,
                Some(ModelSelection::new("gpt-test", None, None).unwrap()),
            )
            .unwrap();

        let mut session = DurableUiSession {
            record,
            journal: None,
            writer_lease: Some(lease),
            next_turn: 1,
            active_turn: None,
            runtime_record_cursor: 0,
        };
        assert_eq!(session.record.revision, initial_revision);
        refresh_session_record(&application, &mut session).unwrap();
        assert!(session.record.revision > initial_revision);
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
}
