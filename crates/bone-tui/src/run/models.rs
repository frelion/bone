//! Model choices come from saved connections and the models currently in use.

use bone_app::{
    App, ConfigChange, ConfigScope, ModelSelection, Profile, RuntimeOverrides, SessionId,
    WorkspaceId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    pub selection: ModelSelection,
    pub profile_label: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelFacts {
    pub saved: Result<bone_app::ResolvedModel, bone_app::ConfigProblem>,
    pub running: Option<bone_app::ResolvedModel>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConnectionSaveError {
    pub(crate) message: &'static str,
    pub(crate) key_saved: bool,
}

impl ConnectionSaveError {
    fn before_key(message: &'static str) -> Self {
        Self {
            message,
            key_saved: false,
        }
    }
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
    workspace_default: bool,
    profile: Profile,
    key: Option<bone_app::ApiKey>,
    selection: Option<ModelSelection>,
) -> Result<(), ConnectionSaveError> {
    profile.validate().map_err(|_| {
        ConnectionSaveError::before_key("Invalid connection settings; nothing was saved")
    })?;
    if key.is_some()
        && matches!(
            profile.endpoint,
            bone_app::EndpointConfig::ChatGptSubscription
        )
    {
        return Err(ConnectionSaveError::before_key(
            "Subscription connections do not accept API keys; nothing was saved",
        ));
    }
    if let Some(selection) = &selection {
        selection.validate().map_err(|_| {
            ConnectionSaveError::before_key("Invalid model settings; nothing was saved")
        })?;
        if selection.profile != profile.id {
            return Err(ConnectionSaveError::before_key(
                "Model and connection identities differ; nothing was saved",
            ));
        }
        if let Some(options) = &selection.options {
            options.validate_for(&profile.endpoint).map_err(|_| {
                ConnectionSaveError::before_key(
                    "Model options do not match the connection; nothing was saved",
                )
            })?;
        }
    }
    let unchanged = profile_saved(app, &profile).await.map_err(|_| {
        ConnectionSaveError::before_key("Unable to read connection settings; nothing was changed")
    })?;
    if !unchanged {
        // save_profile may report a reload failure after persisting the new
        // endpoint. The key and final model application below are the recovery
        // path, so verify the durable profile instead of treating that
        // intermediate result as final.
        let _ = app.save_profile(profile.clone()).await;
    }
    // save_profile persists before reloading live sessions. Even Err can mean
    // the requested endpoint is saved, so inspect facts before touching keys.
    if !profile_saved(app, &profile).await.map_err(|_| {
        ConnectionSaveError::before_key(
            "Unable to confirm saved connection; key and model were not changed",
        )
    })? {
        return Err(ConnectionSaveError::before_key(
            "Connection was not saved as requested; key and model were not changed",
        ));
    }
    let mut key_saved = false;
    let mut reload_failed = false;
    if let Some(key) = key {
        match app.set_api_key_for_profile(profile.clone(), key).await {
            Ok(()) => key_saved = true,
            Err(bone_app::Error::CredentialsSaved { .. }) => {
                key_saved = true;
                reload_failed = true;
            }
            Err(_) => {
                return Err(ConnectionSaveError::before_key(
                    "Connection saved, but API key storage failed; model was not changed",
                ));
            }
        }
        // A different App may have edited the profile during credential work.
        if !profile_saved(app, &profile)
            .await
            .map_err(|_| ConnectionSaveError {
                message: "API key saved, but connection verification failed; model was not changed",
                key_saved,
            })?
        {
            return Err(ConnectionSaveError {
                message: "Connection changed while saving the API key; verify its endpoint before retrying",
                key_saved,
            });
        }
    }
    if let Some(selection) = selection {
        let scope = selection_scope(workspace, session, workspace_default);
        app.update_config(scope, ConfigChange::Model(Some(selection))).await
            .map_err(|_| ConnectionSaveError {
                message: "Connection saved; the model could not be applied. Select it again to retry",
                key_saved,
            })?;
        reload_failed = false;
    }
    if reload_failed {
        return Err(ConnectionSaveError {
            message: "API key saved, but running conversations did not reload. Select the model again to retry",
            key_saved: true,
        });
    }
    Ok(())
}

fn selection_scope(
    workspace: WorkspaceId,
    session: Option<SessionId>,
    workspace_default: bool,
) -> ConfigScope {
    if workspace_default {
        ConfigScope::Workspace(workspace)
    } else {
        session.map_or(ConfigScope::Workspace(workspace), ConfigScope::Session)
    }
}

async fn profile_saved(app: &App, expected: &Profile) -> bone_app::Result<bool> {
    Ok(app
        .profiles()
        .await?
        .iter()
        .any(|profile| profile == expected))
}

/// Every saved connection contributes its known models. A configured custom
/// model is retained even when it is outside that catalogue.
pub(crate) async fn load(
    app: &App,
    workspace: WorkspaceId,
    session: Option<SessionId>,
) -> bone_app::Result<Vec<ModelChoice>> {
    let profiles = profiles(app).await?;
    let mut choices = Vec::new();
    for profile in &profiles {
        for model in &profile.models {
            append_choice(
                &mut choices,
                ModelSelection::new(profile.id.clone(), model.clone())
                    .expect("validated profile model"),
                profile,
                model.clone(),
            );
        }
    }
    // Apply broader scopes first so the nearest runtime selection wins when
    // the same profile/model is configured with different options.
    append_scope(
        &mut choices,
        app.config(ConfigScope::User).await?,
        &profiles,
    );
    append_scope(
        &mut choices,
        app.config(ConfigScope::Workspace(workspace)).await?,
        &profiles,
    );
    if let Some(session) = session {
        append_scope(
            &mut choices,
            app.config(ConfigScope::Session(session)).await?,
            &profiles,
        );
    }
    Ok(choices)
}

fn append_scope(choices: &mut Vec<ModelChoice>, overrides: RuntimeOverrides, profiles: &[Profile]) {
    append_custom(choices, overrides.coordinator, profiles);
    append_custom(choices, overrides.worker, profiles);
}

fn append_custom(
    choices: &mut Vec<ModelChoice>,
    selection: Option<ModelSelection>,
    profiles: &[Profile],
) {
    let Some(selection) = selection else { return };
    let Some(profile) = profiles
        .iter()
        .find(|profile| profile.id == selection.profile)
    else {
        return;
    };
    append_choice(choices, selection.clone(), profile, selection.model);
}

fn append_choice(
    choices: &mut Vec<ModelChoice>,
    selection: ModelSelection,
    profile: &Profile,
    label: String,
) {
    if let Some(choice) = choices.iter_mut().find(|choice| {
        choice.selection.profile == selection.profile && choice.selection.model == selection.model
    }) {
        choice.selection = selection;
        return;
    }
    choices.push(ModelChoice {
        label,
        selection,
        profile_label: profile.label.clone(),
    });
}

/// Apply one model to both roles in this Session. App owns persistence and runtime reload.
/// An App error may occur after persistence; callers must reload configuration
/// rather than interpreting an error as proof that nothing was saved.
pub(crate) async fn apply(
    app: &App,
    session: SessionId,
    selection: &ModelSelection,
) -> bone_app::Result<RuntimeOverrides> {
    // Recheck a possibly stale menu before persisting a missing profile.
    if !profiles(app)
        .await?
        .iter()
        .any(|profile| profile.id == selection.profile)
    {
        return Err(bone_app::Error::InvalidState(
            "unknown saved profile".into(),
        ));
    }
    app.update_config(
        ConfigScope::Session(session),
        ConfigChange::Model(Some(selection.clone())),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_app::{AppOptions, ProfileId};

    #[test]
    fn unknown_models_for_known_profiles_are_kept_and_missing_profiles_are_omitted() {
        let profile = Profile::chatgpt();
        let profile_id = profile.id.clone();
        let selection = ModelSelection::new(profile.id.clone(), "configured-model").unwrap();
        let mut choices = Vec::new();
        append_custom(
            &mut choices,
            Some(selection.clone()),
            std::slice::from_ref(&profile),
        );
        append_custom(
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
        append_custom(
            &mut choices,
            Some(with_options.clone()),
            std::slice::from_ref(&profile),
        );
        append_custom(
            &mut choices,
            Some(ModelSelection::new(ProfileId::new("missing").unwrap(), "model").unwrap()),
            &[profile],
        );
        assert_eq!(choices.len(), 1);
        assert!(
            choices
                .iter()
                .all(|choice| choice.selection.profile == profile_id)
        );
    }

    #[test]
    fn nearer_scope_replaces_the_same_model_options() {
        let profile = Profile::new(
            ProfileId::new("openai").unwrap(),
            "OpenAI",
            bone_app::EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        let mut broad = ModelSelection::new(profile.id.clone(), "gpt-test").unwrap();
        broad.options = Some(
            serde_json::from_value(serde_json::json!({
                "type": "openai_responses", "reasoning": { "effort": "high" }
            }))
            .unwrap(),
        );
        let narrow = ModelSelection::new(profile.id.clone(), "gpt-test").unwrap();
        let mut choices = Vec::new();
        append_scope(
            &mut choices,
            RuntimeOverrides {
                worker: Some(broad),
                ..RuntimeOverrides::default()
            },
            std::slice::from_ref(&profile),
        );
        append_scope(
            &mut choices,
            RuntimeOverrides {
                worker: Some(narrow),
                ..RuntimeOverrides::default()
            },
            std::slice::from_ref(&profile),
        );

        assert_eq!(choices.len(), 1);
        assert!(choices[0].selection.options.is_none());
    }

    #[tokio::test]
    async fn load_starts_empty_and_keeps_the_selected_custom_model() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(AppOptions::isolated(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let initial = load(&app, workspace.id, None).await.unwrap();
        assert!(initial.is_empty());
        assert_eq!(
            facts(&app, workspace.id, None)
                .await
                .as_ref()
                .and_then(ModelFacts::saved_model_label),
            None
        );
        let custom = Profile::new(
            ProfileId::new("custom-api").unwrap(),
            "Custom API",
            bone_app::EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.invalid/v1".into()),
            },
        )
        .unwrap();
        app.save_profile(custom.clone()).await.unwrap();
        let selection = ModelSelection::new(custom.id, "private-model").unwrap();
        app.update_config(
            ConfigScope::Workspace(workspace.id),
            ConfigChange::Model(Some(selection.clone())),
        )
        .await
        .unwrap();
        let choices = load(&app, workspace.id, None).await.unwrap();
        let selected = choices
            .iter()
            .find(|choice| choice.selection == selection)
            .unwrap();
        assert_eq!(selected.label, "private-model");
        assert_eq!(selected.profile_label, "Custom API");
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn load_keeps_a_coordinator_only_custom_model_visible() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(AppOptions::isolated(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let custom = Profile::new(
            ProfileId::new("custom-api").unwrap(),
            "Custom API",
            bone_app::EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.invalid/v1".into()),
            },
        )
        .unwrap();
        app.save_profile(custom.clone()).await.unwrap();
        let selection = ModelSelection::new(custom.id, "coordinator-only").unwrap();
        app.update_config(
            ConfigScope::Workspace(workspace.id),
            ConfigChange::Coordinator(Some(selection.clone())),
        )
        .await
        .unwrap();

        let choices = load(&app, workspace.id, None).await.unwrap();
        assert!(choices.iter().any(|choice| choice.selection == selection));
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

    #[test]
    fn first_model_uses_workspace_scope_even_when_a_conversation_is_open() {
        let workspace = WorkspaceId::new();
        let session = SessionId::new();
        assert_eq!(
            selection_scope(workspace, Some(session), true),
            ConfigScope::Workspace(workspace)
        );
        assert_eq!(
            selection_scope(workspace, Some(session), false),
            ConfigScope::Session(session)
        );
    }

    #[tokio::test]
    async fn connection_round_trip_updates_only_requested_scope() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::isolated(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let profile = profile();
        app.save_profile(profile.clone()).await.unwrap();
        let selection = ModelSelection::new(profile.id.clone(), "user-entered-model").unwrap();
        save_connection(
            &app,
            workspace.id,
            None,
            false,
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
        let app = App::open(bone_app::AppOptions::isolated(root.path().join("data")))
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
            false,
            profile.clone(),
            None,
            Some(selection),
        )
        .await;
        assert_eq!(
            result.unwrap_err().message,
            "Model and connection identities differ; nothing was saved"
        );
        assert!(!profile_saved(&app, &profile).await.unwrap());
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_scope_application_does_not_claim_profile_rollback() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::isolated(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let profile = profile();
        let selection = ModelSelection::new(profile.id.clone(), "model").unwrap();
        let result = save_connection(
            &app,
            workspace.id,
            Some(SessionId::new()),
            false,
            profile.clone(),
            None,
            Some(selection),
        )
        .await;
        assert!(result.unwrap_err().message.starts_with("Connection saved;"));
        assert!(profile_saved(&app, &profile).await.unwrap());
        app.shutdown().await.unwrap();
    }
}
