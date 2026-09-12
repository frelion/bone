//! Model choices come from saved App configuration, not a provider catalogue.
//! A configured model is not a promise that credentials or remote access work.

use bone_app::{
    App, ConfigChange, ConfigScope, ModelSelection, Profile, RuntimeOverrides, SessionId,
    WorkspaceId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    pub selection: ModelSelection,
    pub profile_label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelFacts {
    pub saved: Result<bone_app::ResolvedModel, bone_app::ConfigProblem>,
    pub running: Option<bone_app::ResolvedModel>,
}

impl From<bone_app::ResolvedConfig> for ModelFacts {
    fn from(config: bone_app::ResolvedConfig) -> Self {
        Self {
            saved: config.desired.map(|config| config.worker),
            running: config.running.map(|config| config.worker),
        }
    }
}

impl ModelFacts {
    pub fn saved_model_label(&self) -> Option<&str> {
        self.saved
            .as_ref()
            .ok()
            .map(|model| model.selection.model.as_str())
    }

    pub fn applied_to(&self, running: Option<&bone_app::ResolvedModel>) -> bool {
        matches!((&self.saved, running), (Ok(saved), Some(running)) if saved == running)
    }
}

pub(crate) async fn facts(
    app: &App,
    workspace: WorkspaceId,
    session: Option<SessionId>,
) -> Option<ModelFacts> {
    let config = match session {
        Some(id) => app.resolved_config(id).await,
        None => app.resolved_workspace_config(workspace).await,
    };
    config.ok().map(ModelFacts::from)
}

pub(crate) async fn profiles(app: &App) -> bone_app::Result<Vec<Profile>> {
    app.profiles().await
}

/// Each stage uses App's public boundary. Errors deliberately contain no
/// provider/credential error text and never imply rollback of persisted facts.
pub(crate) async fn save_connection(
    app: &App,
    workspace: WorkspaceId,
    session: Option<SessionId>,
    profile: Profile,
    key: Option<bone_app::ApiKey>,
    selection: Option<ModelSelection>,
) -> Result<Option<&'static str>, &'static str> {
    let writes_key = key.is_some();
    profile
        .validate()
        .map_err(|_| "Invalid connection settings; nothing was saved")?;
    if key.is_some()
        && matches!(
            profile.endpoint,
            bone_app::EndpointConfig::ChatGptSubscription
        )
    {
        return Err("Subscription connections do not accept API keys; nothing was saved");
    }
    if let Some(selection) = &selection {
        selection
            .validate()
            .map_err(|_| "Invalid model settings; nothing was saved")?;
        if selection.profile != profile.id {
            return Err("Model and connection identities differ; nothing was saved");
        }
        if let Some(options) = &selection.options {
            options
                .validate_for(&profile.endpoint)
                .map_err(|_| "Model options do not match the connection; nothing was saved")?;
        }
    }
    let unchanged = profile_saved(app, &profile)
        .await
        .map_err(|_| "Unable to read connection settings; nothing was changed")?;
    let reload_failed = !unchanged && app.save_profile(profile.clone()).await.is_err();
    // save_profile persists before reloading live sessions. Even Err can mean
    // the requested endpoint is saved, so inspect facts before touching keys.
    if !profile_saved(app, &profile)
        .await
        .map_err(|_| "Unable to confirm saved connection; key and model were not changed")?
    {
        return Err("Connection was not saved as requested; key and model were not changed");
    }
    if let Some(key) = key {
        app.set_api_key_for_profile(profile.clone(), key)
            .await
            .map_err(|_| "Connection saved, but API key storage failed; model was not changed")?;
        // A different App may have edited the profile during credential work.
        if !profile_saved(app, &profile).await.map_err(
            |_| "API key saved, but connection verification failed; model was not changed",
        )? {
            return Err(
                "Connection changed while saving the API key; verify its endpoint before retrying",
            );
        }
    }
    if let Some(selection) = selection {
        let scope = session.map_or(ConfigScope::Workspace(workspace), ConfigScope::Session);
        app.update_config(scope, ConfigChange::Worker(Some(selection))).await
            .map_err(|_| "Connection saved; model configuration may be saved but could not be applied. Review current settings")?;
    }
    if reload_failed {
        Err(
            "Connection saved, but an existing session could not reload it. Review current settings",
        )
    } else {
        let mut notice = None;
        if writes_key {
            // The TUI owns sessions in this workspace. A saved key does not
            // rebuild existing clients when their RuntimeConfig is unchanged.
            if let Ok(sessions) = app.list_sessions(workspace).await {
                for session in sessions {
                    if let Ok(config) = app.resolved_config(session.id).await
                        && config.running.as_ref().is_some_and(|running| {
                            running.worker.selection.profile == profile.id
                                || running.coordinator.selection.profile == profile.id
                        })
                    {
                        notice = Some(
                            "Credentials saved; running connections may still use the previous key. Restart the app to reload them.",
                        );
                        break;
                    }
                }
            }
        }
        Ok(notice)
    }
}

async fn profile_saved(app: &App, expected: &Profile) -> bone_app::Result<bool> {
    Ok(app
        .profiles()
        .await?
        .iter()
        .any(|profile| profile == expected))
}

/// Explicit model names are validated by App's public type. They are not
/// advertised as remotely verified: App currently exposes no model catalogue.
pub(crate) async fn explicit(
    app: &App,
    profile: &str,
    model: &str,
) -> bone_app::Result<ModelChoice> {
    let profiles = profiles(app).await?;
    let profile = profiles
        .iter()
        .find(|saved| saved.id.as_str() == profile)
        .ok_or_else(|| bone_app::Error::InvalidState("unknown saved profile".into()))?;
    let selection = ModelSelection::new(profile.id.clone(), model)
        .map_err(|error| bone_app::Error::InvalidState(error.to_string()))?;
    Ok(ModelChoice {
        selection,
        profile_label: profile.label.clone(),
    })
}

/// Most local choices first; retain model options when selecting saved entries.
pub(crate) async fn load(
    app: &App,
    workspace: WorkspaceId,
    session: Option<SessionId>,
) -> bone_app::Result<Vec<ModelChoice>> {
    let profiles = profiles(app).await?;
    let mut choices = Vec::new();
    if let Some(session) = session {
        append(
            &mut choices,
            app.config(ConfigScope::Session(session)).await?.worker,
            &profiles,
        );
    }
    append(
        &mut choices,
        app.config(ConfigScope::Workspace(workspace)).await?.worker,
        &profiles,
    );
    append(
        &mut choices,
        app.config(ConfigScope::User).await?.worker,
        &profiles,
    );
    Ok(choices)
}

fn append(choices: &mut Vec<ModelChoice>, selection: Option<ModelSelection>, profiles: &[Profile]) {
    let Some(selection) = selection else { return };
    let Some(profile) = profiles
        .iter()
        .find(|profile| profile.id == selection.profile)
    else {
        return;
    };
    if choices.iter().any(|choice| choice.selection == selection) {
        return;
    }
    choices.push(ModelChoice {
        selection,
        profile_label: profile.label.clone(),
    });
}

/// Only override this Session's worker. App owns persistence and runtime reload.
/// An App error may occur after persistence; callers must reload configuration
/// rather than interpreting an error as proof that nothing was saved.
pub(crate) async fn apply(
    app: &App,
    session: SessionId,
    choice: &ModelChoice,
) -> bone_app::Result<RuntimeOverrides> {
    // Recheck a possibly stale menu before persisting a missing profile.
    if !profiles(app)
        .await?
        .iter()
        .any(|profile| profile.id == choice.selection.profile)
    {
        return Err(bone_app::Error::InvalidState(
            "unknown saved profile".into(),
        ));
    }
    app.update_config(
        ConfigScope::Session(session),
        ConfigChange::Worker(Some(choice.selection.clone())),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_app::{AppOptions, ProfileId};

    #[test]
    fn deduplication_preserves_distinct_options_and_omits_missing_profiles() {
        let profile = Profile::chatgpt();
        let selection = ModelSelection::new(profile.id.clone(), "configured-model").unwrap();
        let mut choices = Vec::new();
        append(
            &mut choices,
            Some(selection.clone()),
            std::slice::from_ref(&profile),
        );
        append(
            &mut choices,
            Some(selection.clone()),
            std::slice::from_ref(&profile),
        );
        let mut with_options = selection;
        with_options.options = Some(
            serde_json::from_value(serde_json::json!({
                "type": "openai_responses", "reasoning": { "effort": "high" }
            }))
            .unwrap(),
        );
        append(
            &mut choices,
            Some(with_options.clone()),
            std::slice::from_ref(&profile),
        );
        append(
            &mut choices,
            Some(ModelSelection::new(ProfileId::new("missing").unwrap(), "model").unwrap()),
            &[profile],
        );
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[1].selection, with_options);
    }

    #[tokio::test]
    async fn public_app_configuration_round_trip_is_scoped_and_has_no_fake_models() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        assert!(load(&app, workspace.id, None).await.unwrap().is_empty());
        assert_eq!(
            facts(&app, workspace.id, None)
                .await
                .as_ref()
                .and_then(ModelFacts::saved_model_label),
            None
        );
        assert!(explicit(&app, "missing", "model").await.is_err());
        app.save_profile(Profile::chatgpt()).await.unwrap();
        assert!(explicit(&app, "chatgpt", " ").await.is_err());
        let global = explicit(&app, "chatgpt", "configured-global")
            .await
            .unwrap();
        app.update_config(
            ConfigScope::User,
            ConfigChange::Worker(Some(global.selection.clone())),
        )
        .await
        .unwrap();
        let session = app
            .create_session(workspace.id, "model test")
            .await
            .unwrap();
        let coordinator =
            ModelSelection::new(ProfileId::chatgpt(), "configured-coordinator").unwrap();
        app.update_config(
            ConfigScope::Session(session.id()),
            ConfigChange::Coordinator(Some(coordinator.clone())),
        )
        .await
        .unwrap();
        let selected = explicit(&app, "chatgpt", "explicit-local").await.unwrap();
        let saved = apply(&app, session.id(), &selected).await.unwrap();
        assert_eq!(saved.worker.as_ref(), Some(&selected.selection));
        assert_eq!(
            facts(&app, workspace.id, Some(session.id()))
                .await
                .as_ref()
                .and_then(ModelFacts::saved_model_label),
            Some("explicit-local")
        );
        assert_eq!(
            facts(&app, workspace.id, None)
                .await
                .as_ref()
                .and_then(ModelFacts::saved_model_label),
            Some("configured-global")
        );
        assert_eq!(saved.coordinator, Some(coordinator));
        assert_eq!(
            app.config(ConfigScope::User).await.unwrap().worker,
            Some(global.selection.clone())
        );
        assert_eq!(
            load(&app, workspace.id, Some(session.id())).await.unwrap(),
            vec![selected, global]
        );
        app.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;

    fn profile() -> Profile {
        Profile::new(
            bone_app::ProfileId::new("configured-api").unwrap(),
            "Configured API",
            bone_app::EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.invalid/v1".into()),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn secret_free_connection_round_trip_updates_only_requested_scope() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let profile = profile();
        let selection = ModelSelection::new(profile.id.clone(), "user-entered-model").unwrap();
        save_connection(
            &app,
            workspace.id,
            None,
            profile.clone(),
            None,
            Some(selection.clone()),
        )
        .await
        .unwrap();
        assert!(profile_saved(&app, &profile).await.unwrap());
        assert_eq!(
            app.config(ConfigScope::Workspace(workspace.id))
                .await
                .unwrap()
                .worker,
            Some(selection)
        );
        assert_eq!(app.config(ConfigScope::User).await.unwrap().worker, None);
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_selection_is_rejected_before_any_profile_write() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let profile = profile();
        let selection = ModelSelection::new(
            bone_app::ProfileId::new("different-profile").unwrap(),
            "model",
        )
        .unwrap();
        let result = save_connection(
            &app,
            workspace.id,
            None,
            profile.clone(),
            None,
            Some(selection),
        )
        .await;
        assert_eq!(
            result,
            Err("Model and connection identities differ; nothing was saved")
        );
        assert!(!profile_saved(&app, &profile).await.unwrap());
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_scope_application_does_not_claim_profile_rollback() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let profile = profile();
        let selection = ModelSelection::new(profile.id.clone(), "model").unwrap();
        let result = save_connection(
            &app,
            workspace.id,
            Some(SessionId::new()),
            profile.clone(),
            None,
            Some(selection),
        )
        .await;
        assert!(result.unwrap_err().starts_with("Connection saved;"));
        assert!(profile_saved(&app, &profile).await.unwrap());
        app.shutdown().await.unwrap();
    }
}
