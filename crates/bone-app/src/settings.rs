use std::{collections::BTreeMap, fmt, path::Path};

use crate::{SessionId, WorkspaceId};
use bone_agent::{Effort, ModelSettings, SystemConfig, TaskConfig};
use bone_config::{ConfigError, ConfigManager, ConfigSection};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_REVISION_BYTES: usize = 512;
const CONFIG_RETRY_LIMIT: usize = 4;
const DEFAULT_MODEL_TIMEOUT_SECONDS: u32 = 120;
const DEFAULT_SOFT_DEADLINE_SECONDS: u32 = 30;
const DEFAULT_SHUTDOWN_GRACE_SECONDS: u32 = 5;

/// A public settings key. Product code uses typed constants or validated keys
/// instead of ad-hoc JSON paths.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SettingKey(String);

impl SettingKey {
    pub const MODEL_SOLVER: &'static str = "models.solver";
    pub const TUI_SHOW_PROGRESS: &'static str = "tui.display.show_progress";

    pub fn new(value: impl Into<String>) -> Result<Self, SettingKeyError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'_' | b'-')
                    })
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(SettingKeyError(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SettingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("invalid setting key: {0}")]
pub struct SettingKeyError(String);

/// The scope category a descriptor permits. The type deliberately excludes a
/// mutable WorkspaceRoot scope: a running BONE process has one fixed root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeKind {
    User,
    Workspace,
    Session,
}

/// A concrete setting scope. Values resolve in Session > Workspace > User >
/// built-in order; CLI choices are materialized into Session scope instead of
/// becoming a hidden fifth layer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Scope {
    User,
    Workspace(WorkspaceId),
    Session(SessionId),
}

impl Scope {
    pub fn kind(self) -> ScopeKind {
        match self {
            Self::User => ScopeKind::User,
            Self::Workspace(_) => ScopeKind::Workspace,
            Self::Session(_) => ScopeKind::Session,
        }
    }
}

/// The source which supplied an effective value. A value set with `--model`
/// is represented as `Session`, not as an opaque process override.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SettingSource {
    BuiltIn,
    User,
    Workspace,
    Session,
}

/// When a validated desired value may affect execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyBoundary {
    /// Safe display-only values can change in the current/next frame.
    Immediate,
    /// Solver, effort and agent behavior apply only to a new user turn.
    NextUserTurn,
    /// Connection-like resources require successful preparation before swap.
    PrepareAndSwap,
}

/// A descriptor is the shared product contract for settings UI, command
/// registry and runtime consumers. It prevents the UI from inventing settings
/// that have no validation or apply consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettingDescriptor {
    pub key: &'static str,
    pub label: &'static str,
    pub allowed_scopes: &'static [ScopeKind],
    pub default_scope: ScopeKind,
    pub apply_boundary: ApplyBoundary,
    pub model_visible: bool,
}

const USER_ONLY: &[ScopeKind] = &[ScopeKind::User];
const MODEL_SCOPES: &[ScopeKind] = &[ScopeKind::Session, ScopeKind::Workspace, ScopeKind::User];

/// Descriptors currently backed by a real product contract. More settings are
/// intentionally not listed until they have a validator and application
/// consumer, so the future Settings Center cannot show dead controls.
pub const PUBLIC_SETTINGS: &[SettingDescriptor] = &[
    SettingDescriptor {
        key: SettingKey::MODEL_SOLVER,
        label: "Solver model",
        allowed_scopes: MODEL_SCOPES,
        default_scope: ScopeKind::Session,
        apply_boundary: ApplyBoundary::NextUserTurn,
        model_visible: false,
    },
    SettingDescriptor {
        key: SettingKey::TUI_SHOW_PROGRESS,
        label: "Show activity details",
        allowed_scopes: USER_ONLY,
        default_scope: ScopeKind::User,
        apply_boundary: ApplyBoundary::Immediate,
        model_visible: false,
    },
];

/// Immediate terminal presentation preferences. This lives in the application
/// settings service instead of being a startup-only TUI struct, so changing it
/// can update the current frame without restarting BONE.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TuiDisplaySettings {
    /// Show model/tool starts, completions, and intermediate progress.
    pub show_progress: bool,
}

impl Default for TuiDisplaySettings {
    fn default() -> Self {
        Self {
            show_progress: true,
        }
    }
}

impl ConfigSection for TuiDisplaySettings {
    const KEY: &'static str = "tui.display";

    fn description() -> &'static str {
        "Terminal display preferences. Applied immediately by the active frontend."
    }

    fn schema() -> Value {
        schemars::schema_for!(Self).to_value()
    }
}

/// An opaque, immutable revision of the resolved (not merely persisted)
/// configuration. The persistence service will derive it from the scope
/// revisions it has actually validated and applied.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectiveRevision(String);

impl EffectiveRevision {
    pub fn new(value: impl Into<String>) -> Result<Self, RevisionError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_REVISION_BYTES {
            return Err(RevisionError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EffectiveRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error(
    "effective configuration revision must be non-empty and at most {MAX_REVISION_BYTES} bytes"
)]
pub struct RevisionError;

/// Validated solver parameters which can be pinned into a user turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub model: String,
    pub effort: Option<Effort>,
    pub timeout_seconds: Option<u32>,
}

impl ModelSelection {
    pub fn new(
        model: impl Into<String>,
        effort: Option<Effort>,
        timeout_seconds: Option<u32>,
    ) -> Result<Self, ModelSelectionError> {
        let model = model.into();
        if model.trim().is_empty() || model.trim() != model || model.len() > MAX_MODEL_ID_BYTES {
            return Err(ModelSelectionError::InvalidModel);
        }
        if timeout_seconds == Some(0) {
            return Err(ModelSelectionError::ZeroTimeout);
        }
        Ok(Self {
            model,
            effort,
            timeout_seconds,
        })
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ModelSelectionError {
    #[error("model identifier must be trimmed, non-empty, and at most {MAX_MODEL_ID_BYTES} bytes")]
    InvalidModel,
    #[error("model timeout must be greater than zero when configured")]
    ZeroTimeout,
}

/// Effective model value together with the inheritance source which supplied
/// it. UI can therefore say "inherited from this workspace" without parsing
/// raw config files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModel {
    pub selection: ModelSelection,
    pub source: SettingSource,
}

/// The product-level inheritance layer for solver selection. It is stored as
/// one typed private config section; values are still semantically scoped by
/// their workspace/session IDs rather than by a configuration-file path.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelOverrides {
    user: Option<ModelSelection>,
    workspaces: BTreeMap<WorkspaceId, ModelSelection>,
    sessions: BTreeMap<SessionId, ModelSelection>,
}

impl ConfigSection for ModelOverrides {
    const KEY: &'static str = "app.models";

    fn description() -> &'static str {
        "BONE solver-model overrides resolved as session, workspace, user, then system default."
    }

    fn schema() -> Value {
        // IDs are opaque local UUIDs and are deliberately described as map
        // keys rather than paths. Runtime validation below remains the source
        // of truth for model values and inherited scopes.
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "user": { "$ref": "#/$defs/model_selection" },
                "workspaces": {
                    "type": "object",
                    "additionalProperties": { "$ref": "#/$defs/model_selection" }
                },
                "sessions": {
                    "type": "object",
                    "additionalProperties": { "$ref": "#/$defs/model_selection" }
                }
            },
            "$defs": {
                "model_selection": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["model"],
                    "properties": {
                        "model": { "type": "string", "minLength": 1, "maxLength": MAX_MODEL_ID_BYTES },
                        "effort": { "type": ["string", "null"] },
                        "timeout_seconds": { "type": ["integer", "null"], "minimum": 1 }
                    }
                }
            }
        })
    }

    fn validate(&self) -> Result<(), String> {
        let validate = |selection: &ModelSelection| {
            ModelSelection::new(
                selection.model.clone(),
                selection.effort,
                selection.timeout_seconds,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        };
        if let Some(selection) = &self.user {
            validate(selection)?;
        }
        for selection in self.workspaces.values().chain(self.sessions.values()) {
            validate(selection)?;
        }
        Ok(())
    }
}

impl ModelOverrides {
    pub fn set(&mut self, scope: Scope, selection: ModelSelection) {
        match scope {
            Scope::User => self.user = Some(selection),
            Scope::Workspace(id) => {
                self.workspaces.insert(id, selection);
            }
            Scope::Session(id) => {
                self.sessions.insert(id, selection);
            }
        }
    }

    /// Clear the current session override and let it inherit from Workspace,
    /// then User, then the built-in fallback.
    pub fn inherit_session(&mut self, session: SessionId) -> bool {
        self.sessions.remove(&session).is_some()
    }

    pub fn resolve(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
        built_in: &ModelSelection,
    ) -> ResolvedModel {
        if let Some(selection) = self.sessions.get(&session) {
            return ResolvedModel {
                selection: selection.clone(),
                source: SettingSource::Session,
            };
        }
        if let Some(selection) = self.workspaces.get(&workspace) {
            return ResolvedModel {
                selection: selection.clone(),
                source: SettingSource::Workspace,
            };
        }
        if let Some(selection) = &self.user {
            return ResolvedModel {
                selection: selection.clone(),
                source: SettingSource::User,
            };
        }
        ResolvedModel {
            selection: built_in.clone(),
            source: SettingSource::BuiltIn,
        }
    }
}

/// A concrete configuration choice frozen at the beginning of one user turn.
/// It is immutable by construction: later Settings changes can become
/// effective for the next turn but cannot mutate this value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TurnConfig {
    pub effective_revision: EffectiveRevision,
    pub solver: ModelSelection,
    pub coordinator: ModelSelection,
    pub agent_protocol_version: u32,
    pub tool_schema_version: u32,
}

impl TurnConfig {
    pub fn new(
        effective_revision: EffectiveRevision,
        solver: ModelSelection,
        coordinator: ModelSelection,
        agent_protocol_version: u32,
        tool_schema_version: u32,
    ) -> Result<Self, TurnConfigError> {
        if agent_protocol_version == 0 || tool_schema_version == 0 {
            return Err(TurnConfigError);
        }
        Ok(Self {
            effective_revision,
            solver,
            coordinator,
            agent_protocol_version,
            tool_schema_version,
        })
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("agent and tool schema versions must be greater than zero")]
pub struct TurnConfigError;

/// Human-visible application status after a settings transaction. "Saved" is
/// deliberately not conflated with "Applied": the SettingsService should only
/// construct `Applied` after its runtime consumer acknowledges the revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyState {
    Validating,
    Preparing,
    Applied {
        effective_revision: EffectiveRevision,
    },
    PendingNextTurn {
        desired_revision: EffectiveRevision,
        current_turn_revision: EffectiveRevision,
    },
    FailedUsingLastKnownGood {
        desired_revision: Option<EffectiveRevision>,
        last_known_good_revision: EffectiveRevision,
        reason: String,
    },
}

/// Determine the truthful UX state for a model change after it has been
/// validated, durably saved, and accepted as the next effective revision. A
/// caller still needs to persist / apply it; this function ensures a Working
/// turn cannot be misrepresented as hot-switched.
pub fn model_apply_state(
    desired_revision: EffectiveRevision,
    active_turn: Option<&TurnConfig>,
) -> ApplyState {
    match active_turn {
        Some(turn) => ApplyState::PendingNextTurn {
            desired_revision,
            current_turn_revision: turn.effective_revision.clone(),
        },
        None => ApplyState::Applied {
            effective_revision: desired_revision,
        },
    }
}

/// The model state the frontend can truthfully present after resolving every
/// supported scope. `NeedsModel` is a normal first-run state, not a terminal
/// configuration error: the shell can remain usable for browsing sessions,
/// draft editing, and opening the model picker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelResolution {
    NeedsModel {
        effective_revision: EffectiveRevision,
    },
    Ready {
        resolved: ResolvedModel,
        effective_revision: EffectiveRevision,
    },
}

impl ModelResolution {
    pub fn effective_revision(&self) -> &EffectiveRevision {
        match self {
            Self::NeedsModel { effective_revision }
            | Self::Ready {
                effective_revision, ..
            } => effective_revision,
        }
    }

    /// Materialize a resolved solver choice into the existing Agent startup
    /// API. The choice is session-local task input rather than a hidden
    /// process-wide override.
    pub fn task_config(&self) -> Option<TaskConfig> {
        let Self::Ready { resolved, .. } = self else {
            return None;
        };
        Some(TaskConfig {
            model: Some(resolved.selection.model.clone()),
            effort: resolved.selection.effort,
            timeout_seconds: resolved.selection.timeout_seconds,
        })
    }
}

/// Result of a durable model mutation. The caller combines this revision with
/// the current turn to choose `Applied` versus `PendingNextTurn`; the settings
/// service itself never pretends an existing runtime hot-switched model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChange {
    pub scope: Scope,
    pub resolved: ResolvedModel,
    pub desired_revision: EffectiveRevision,
}

/// Durable application settings backed by the existing typed ConfigManager.
///
/// The manager supplies validation, cross-process CAS, private atomic writes,
/// and an immutable content revision. This service adds BONE-specific
/// initialization, scope resolution, and the first-run `NeedsModel` state.
#[derive(Clone, Debug)]
pub struct SettingsService {
    manager: ConfigManager,
}

impl SettingsService {
    /// Open the normal user configuration path and create the non-secret
    /// BONE settings document on first use. No model is guessed or connected.
    pub fn open_default() -> Result<Self, SettingsError> {
        Self::open_at(bone_config::default_path()?)
    }

    /// Open an explicit absolute configuration path. This injection point is
    /// used by tests, portable installs, and the eventual Settings Center.
    pub fn open_at(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let manager = bone_agent::config_builder()?
            .register::<ModelOverrides>()?
            .register::<TuiDisplaySettings>()?
            .build(path)?;
        let service = Self { manager };
        service.ensure_section::<ModelOverrides>()?;
        service.ensure_section::<TuiDisplaySettings>()?;
        Ok(service)
    }

    /// The low-level manager remains exposed only for connection/startup code;
    /// interactive callers should use the typed methods below instead of
    /// editing JSON sections directly.
    pub fn config_manager(&self) -> &ConfigManager {
        &self.manager
    }

    pub fn display_settings(
        &self,
    ) -> Result<(TuiDisplaySettings, EffectiveRevision), SettingsError> {
        let snapshot = self.manager.snapshot()?;
        let display = snapshot.get::<TuiDisplaySettings>()?.unwrap_or_default();
        Ok((display, revision_of(&snapshot)?))
    }

    pub fn set_show_progress(
        &self,
        show_progress: bool,
    ) -> Result<EffectiveRevision, SettingsError> {
        for _ in 0..CONFIG_RETRY_LIMIT {
            let snapshot = self.manager.snapshot()?;
            let mut display = snapshot.get::<TuiDisplaySettings>()?.unwrap_or_default();
            display.show_progress = show_progress;
            match self.manager.set(&display, snapshot.revision()) {
                Ok(change) => {
                    return EffectiveRevision::new(change.revision.to_string()).map_err(Into::into);
                }
                Err(ConfigError::RevisionConflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(SettingsError::Contention)
    }

    /// Resolve a solver model for exactly one logical session. The fallback is
    /// the system default needed by the Agent's coordinator; there is no
    /// unvalidated hard-coded model ID hidden in BONE.
    pub fn resolve_model(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
    ) -> Result<ModelResolution, SettingsError> {
        let snapshot = self.manager.snapshot()?;
        resolution_from_snapshot(&snapshot, workspace, session)
    }

    /// Persist a model selection at User, Workspace, or Session scope. A
    /// first selection also creates a valid `agent.system` baseline so later
    /// runtime attachment has a coordinator. It does not start a connection
    /// or mutate a running turn.
    pub fn set_solver_model(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
        scope: Scope,
        selection: ModelSelection,
    ) -> Result<ModelChange, SettingsError> {
        // Reconstructing validates callers that obtained a model selection
        // through serde or another frontend rather than `ModelSelection::new`.
        let selection =
            ModelSelection::new(selection.model, selection.effort, selection.timeout_seconds)?;
        validate_scope_target(scope, workspace, session)?;

        for _ in 0..CONFIG_RETRY_LIMIT {
            let snapshot = self.manager.snapshot()?;
            let system = snapshot.get::<SystemConfig>()?;
            let required_system = match (scope, system) {
                (Scope::User, Some(mut system)) => {
                    let solver = agent_model_settings(&selection);
                    if system.default_solver == solver {
                        None
                    } else {
                        system.default_solver = solver;
                        Some(system)
                    }
                }
                (_, Some(_)) => None,
                (_, None) => Some(first_system_config(&selection)),
            };

            if let Some(system) = required_system {
                match self.manager.set(&system, snapshot.revision()) {
                    Ok(_) => continue,
                    Err(ConfigError::RevisionConflict) => continue,
                    Err(error) => return Err(error.into()),
                }
            }

            // The snapshot still has a system section whenever one was needed
            // above: either it existed already or the next retry observes the
            // newly written one.
            let snapshot = self.manager.snapshot()?;
            let mut overrides = snapshot.get::<ModelOverrides>()?.unwrap_or_default();
            overrides.set(scope, selection.clone());
            match self.manager.set(&overrides, snapshot.revision()) {
                Ok(_) => {
                    let resolution = self.resolve_model(workspace, session)?;
                    let ModelResolution::Ready {
                        resolved,
                        effective_revision,
                    } = resolution
                    else {
                        return Err(SettingsError::MissingSystemAfterWrite);
                    };
                    return Ok(ModelChange {
                        scope,
                        resolved,
                        desired_revision: effective_revision,
                    });
                }
                Err(ConfigError::RevisionConflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(SettingsError::Contention)
    }

    /// Delete only one Session model override. Workspace and user defaults
    /// remain intact, so `/model inherit` has an unsurprising scope.
    pub fn inherit_session_model(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
    ) -> Result<ModelResolution, SettingsError> {
        for _ in 0..CONFIG_RETRY_LIMIT {
            let snapshot = self.manager.snapshot()?;
            let mut overrides = snapshot.get::<ModelOverrides>()?.unwrap_or_default();
            if !overrides.inherit_session(session) {
                return resolution_from_snapshot(&snapshot, workspace, session);
            }
            match self.manager.set(&overrides, snapshot.revision()) {
                Ok(_) => return self.resolve_model(workspace, session),
                Err(ConfigError::RevisionConflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(SettingsError::Contention)
    }

    fn ensure_section<T>(&self) -> Result<(), SettingsError>
    where
        T: ConfigSection + Default,
    {
        for _ in 0..CONFIG_RETRY_LIMIT {
            let snapshot = self.manager.snapshot()?;
            if snapshot.get::<T>()?.is_some() {
                return Ok(());
            }
            match self.manager.set(&T::default(), snapshot.revision()) {
                Ok(_) => return Ok(()),
                Err(ConfigError::RevisionConflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(SettingsError::Contention)
    }
}

/// Errors from typed application setting operations. A damaged configuration
/// is deliberately surfaced here; a TUI shell can render repair state without
/// treating it as a reason to lose workspace/session access.
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Model(#[from] ModelSelectionError),
    #[error(transparent)]
    Revision(#[from] RevisionError),
    #[error("configuration changed repeatedly while applying this setting; please retry")]
    Contention,
    #[error("agent system configuration was still missing after a model was saved")]
    MissingSystemAfterWrite,
    #[error("setting scope {scope:?} does not belong to the current workspace/session")]
    ScopeTargetMismatch { scope: Scope },
}

fn revision_of(snapshot: &bone_config::ConfigSnapshot) -> Result<EffectiveRevision, SettingsError> {
    Ok(EffectiveRevision::new(snapshot.revision().to_string())?)
}

fn resolution_from_snapshot(
    snapshot: &bone_config::ConfigSnapshot,
    workspace: WorkspaceId,
    session: SessionId,
) -> Result<ModelResolution, SettingsError> {
    let effective_revision = revision_of(snapshot)?;
    let Some(system) = snapshot.get::<SystemConfig>()? else {
        return Ok(ModelResolution::NeedsModel { effective_revision });
    };
    let fallback = model_selection_from_agent(&system.default_solver)?;
    let overrides = snapshot.get::<ModelOverrides>()?.unwrap_or_default();
    Ok(ModelResolution::Ready {
        resolved: overrides.resolve(workspace, session, &fallback),
        effective_revision,
    })
}

fn model_selection_from_agent(settings: &ModelSettings) -> Result<ModelSelection, SettingsError> {
    Ok(ModelSelection::new(
        settings.model.clone(),
        settings.effort,
        Some(settings.timeout_seconds),
    )?)
}

fn agent_model_settings(selection: &ModelSelection) -> ModelSettings {
    ModelSettings {
        model: selection.model.clone(),
        effort: selection.effort,
        timeout_seconds: selection
            .timeout_seconds
            .unwrap_or(DEFAULT_MODEL_TIMEOUT_SECONDS),
    }
}

fn first_system_config(selection: &ModelSelection) -> SystemConfig {
    let model = agent_model_settings(selection);
    SystemConfig {
        coordinator: model.clone(),
        default_solver: model,
        soft_deadline_seconds: DEFAULT_SOFT_DEADLINE_SECONDS,
        shutdown_grace_seconds: DEFAULT_SHUTDOWN_GRACE_SECONDS,
    }
}

fn validate_scope_target(
    scope: Scope,
    workspace: WorkspaceId,
    session: SessionId,
) -> Result<(), SettingsError> {
    match scope {
        Scope::User => Ok(()),
        Scope::Workspace(scope_workspace) if scope_workspace == workspace => Ok(()),
        Scope::Session(scope_session) if scope_session == session => Ok(()),
        _ => Err(SettingsError::ScopeTargetMismatch { scope }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(value: &str) -> ModelSelection {
        ModelSelection::new(value, Some(Effort::Medium), Some(90)).unwrap()
    }

    fn revision(value: &str) -> EffectiveRevision {
        EffectiveRevision::new(value).unwrap()
    }

    #[test]
    fn public_descriptors_have_real_unique_keys_and_valid_default_scopes() {
        let mut seen = std::collections::BTreeSet::new();
        for descriptor in PUBLIC_SETTINGS {
            assert!(SettingKey::new(descriptor.key).is_ok());
            assert!(seen.insert(descriptor.key));
            assert!(
                descriptor
                    .allowed_scopes
                    .contains(&descriptor.default_scope)
            );
        }
    }

    #[test]
    fn model_resolution_follows_session_workspace_user_builtin_order() {
        let workspace = WorkspaceId::new();
        let session = SessionId::new();
        let mut overrides = ModelOverrides::default();
        let built_in = model("built-in");
        assert_eq!(
            overrides.resolve(workspace, session, &built_in).source,
            SettingSource::BuiltIn
        );

        overrides.set(Scope::User, model("user"));
        assert_eq!(
            overrides.resolve(workspace, session, &built_in),
            ResolvedModel {
                selection: model("user"),
                source: SettingSource::User,
            }
        );

        overrides.set(Scope::Workspace(workspace), model("workspace"));
        assert_eq!(
            overrides.resolve(workspace, session, &built_in).source,
            SettingSource::Workspace
        );

        overrides.set(Scope::Session(session), model("session"));
        assert_eq!(
            overrides.resolve(workspace, session, &built_in).selection,
            model("session")
        );
    }

    #[test]
    fn inherit_removes_only_the_session_override() {
        let workspace = WorkspaceId::new();
        let session = SessionId::new();
        let mut overrides = ModelOverrides::default();
        let built_in = model("built-in");
        overrides.set(Scope::Workspace(workspace), model("workspace"));
        overrides.set(Scope::Session(session), model("session"));

        assert!(overrides.inherit_session(session));
        assert_eq!(
            overrides.resolve(workspace, session, &built_in).selection,
            model("workspace")
        );
        assert!(!overrides.inherit_session(session));
    }

    #[test]
    fn changing_model_in_running_turn_is_pending_next_turn() {
        let current = TurnConfig::new(
            revision("effective-a"),
            model("solver-a"),
            model("coordinator-a"),
            1,
            1,
        )
        .unwrap();

        assert_eq!(
            model_apply_state(revision("effective-b"), Some(&current)),
            ApplyState::PendingNextTurn {
                desired_revision: revision("effective-b"),
                current_turn_revision: revision("effective-a"),
            }
        );
        assert_eq!(
            model_apply_state(revision("effective-b"), None),
            ApplyState::Applied {
                effective_revision: revision("effective-b"),
            }
        );
    }

    #[test]
    fn rejects_invalid_model_and_turn_values() {
        assert_eq!(
            ModelSelection::new(" ", None, None),
            Err(ModelSelectionError::InvalidModel)
        );
        assert_eq!(
            ModelSelection::new("model", None, Some(0)),
            Err(ModelSelectionError::ZeroTimeout)
        );
        assert!(TurnConfig::new(revision("r"), model("a"), model("b"), 0, 1).is_err());
    }

    #[test]
    fn settings_service_creates_a_safe_document_but_never_guesses_a_model() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let service = SettingsService::open_at(&path).unwrap();
        let workspace = WorkspaceId::new();
        let session = SessionId::new();

        assert!(path.exists());
        assert!(matches!(
            service.resolve_model(workspace, session).unwrap(),
            ModelResolution::NeedsModel { .. }
        ));
        let (display, _) = service.display_settings().unwrap();
        assert!(display.show_progress);
        assert!(
            service
                .config_manager()
                .snapshot()
                .unwrap()
                .get::<ModelOverrides>()
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn first_session_model_creates_agent_baseline_and_persists_scope() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let service = SettingsService::open_at(&path).unwrap();
        let workspace = WorkspaceId::new();
        let session = SessionId::new();
        let desired = model("session-solver");

        let change = service
            .set_solver_model(workspace, session, Scope::Session(session), desired.clone())
            .unwrap();
        assert_eq!(change.scope, Scope::Session(session));
        assert_eq!(change.resolved.selection, desired);
        assert_eq!(change.resolved.source, SettingSource::Session);
        assert_eq!(change.desired_revision.as_str().chars().count(), 64);

        let snapshot = service.config_manager().snapshot().unwrap();
        let system = snapshot.get::<SystemConfig>().unwrap().unwrap();
        assert_eq!(system.default_solver.model, "session-solver");
        assert_eq!(system.coordinator.model, "session-solver");
        let reopened = SettingsService::open_at(&path).unwrap();
        let resolved = reopened.resolve_model(workspace, session).unwrap();
        assert_eq!(
            resolved.task_config().unwrap().model.as_deref(),
            Some("session-solver")
        );
    }

    #[test]
    fn persisted_scopes_resolve_in_order_and_inherit_is_session_local() {
        let directory = tempfile::tempdir().unwrap();
        let service = SettingsService::open_at(directory.path().join("config.json")).unwrap();
        let workspace = WorkspaceId::new();
        let session = SessionId::new();

        service
            .set_solver_model(workspace, session, Scope::User, model("user"))
            .unwrap();
        service
            .set_solver_model(
                workspace,
                session,
                Scope::Workspace(workspace),
                model("workspace"),
            )
            .unwrap();
        service
            .set_solver_model(
                workspace,
                session,
                Scope::Session(session),
                model("session"),
            )
            .unwrap();
        assert_eq!(
            service.resolve_model(workspace, session).unwrap(),
            ModelResolution::Ready {
                resolved: ResolvedModel {
                    selection: model("session"),
                    source: SettingSource::Session,
                },
                effective_revision: service
                    .resolve_model(workspace, session)
                    .unwrap()
                    .effective_revision()
                    .clone(),
            }
        );

        let inherited = service.inherit_session_model(workspace, session).unwrap();
        let ModelResolution::Ready { resolved, .. } = inherited else {
            panic!("a configured workspace must resolve a model");
        };
        assert_eq!(resolved.selection, model("workspace"));
        assert_eq!(resolved.source, SettingSource::Workspace);
    }

    #[test]
    fn rejects_scope_ids_that_do_not_belong_to_the_open_session() {
        let directory = tempfile::tempdir().unwrap();
        let service = SettingsService::open_at(directory.path().join("config.json")).unwrap();
        let workspace = WorkspaceId::new();
        let session = SessionId::new();
        let error = service
            .set_solver_model(
                workspace,
                session,
                Scope::Session(SessionId::new()),
                model("not-current"),
            )
            .unwrap_err();
        assert!(matches!(error, SettingsError::ScopeTargetMismatch { .. }));
    }

    #[test]
    fn display_setting_is_saved_with_an_immediate_revision() {
        let directory = tempfile::tempdir().unwrap();
        let service = SettingsService::open_at(directory.path().join("config.json")).unwrap();
        let before = service.display_settings().unwrap().1;
        let after = service.set_show_progress(false).unwrap();
        assert_ne!(after, before);
        assert!(!service.display_settings().unwrap().0.show_progress);
    }
}
