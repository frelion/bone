//! Typed BONE product settings.
//!
//! The user never has to edit a configuration file: this service reads and
//! writes one SQLite-backed `GlobalSettings` document plus Workspace settings.
//! Session records are supplied by their owning session repository, so this
//! service never depends on session storage layout. No dynamic section
//! registry, JSON schema registry, raw config revision, or arbitrary setting
//! map leaks into the TUI or Agent.

use std::{fmt, time::Duration};

use bone_agent::{
    Effort, ModelSettings, ResolvedAgentRuntimeConfig, ResolvedAgentRuntimeConfigError,
};
use bone_store::{BoneStore, StoreError};
use bone_tools::{ToolLimits, ToolLimitsError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{RecordError, SessionId, SessionRecord, WorkspaceId, durable::keys};

const MAX_MODEL_ID_BYTES: usize = 256;
const SETTINGS_RETRY_LIMIT: usize = 4;
const DEFAULT_MODEL_TIMEOUT_SECONDS: u32 = 120;
const DEFAULT_SOFT_DEADLINE_SECONDS: u32 = 30;
const DEFAULT_SHUTDOWN_GRACE_SECONDS: u32 = 5;

/// A public setting name used by the command/UI descriptor layer. Storage is
/// still typed; these labels are not database paths.
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

/// The scope category a descriptor permits. A running process has exactly one
/// Workspace, so there is no mutable path-like scope in the public API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeKind {
    User,
    Workspace,
    Session,
}

/// A concrete model setting scope. Values resolve Session > Workspace > User.
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

/// The source which supplied an effective value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SettingSource {
    User,
    Workspace,
    Session,
}

/// When a validated desired value may affect execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyBoundary {
    Immediate,
    NextUserTurn,
}

/// Descriptor shared by command help and a future settings UI. It intentionally
/// includes only controls with a real typed persistence/application contract.
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

/// Immediate terminal presentation preferences stored inside `GlobalSettings`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TuiDisplaySettings {
    pub show_progress: bool,
}

impl Default for TuiDisplaySettings {
    fn default() -> Self {
        Self {
            show_progress: true,
        }
    }
}

/// Validated solver parameters which can live at User, Workspace, or Session
/// scope. The Agent receives the fully materialized `ModelSettings` only when
/// a runtime is started.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        let selection = Self {
            model: model.into(),
            effort,
            timeout_seconds,
        };
        selection.validate()?;
        Ok(selection)
    }

    pub fn validate(&self) -> Result<(), ModelSelectionError> {
        if self.model.trim().is_empty()
            || self.model.trim() != self.model
            || self.model.len() > MAX_MODEL_ID_BYTES
        {
            return Err(ModelSelectionError::InvalidModel);
        }
        if self.timeout_seconds == Some(0) {
            return Err(ModelSelectionError::ZeroTimeout);
        }
        Ok(())
    }

    pub fn as_model_settings(&self) -> ModelSettings {
        ModelSettings {
            model: self.model.clone(),
            effort: self.effort,
            timeout_seconds: self
                .timeout_seconds
                .unwrap_or(DEFAULT_MODEL_TIMEOUT_SECONDS),
        }
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
/// it. UI can truthfully say where the selected model comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModel {
    pub selection: ModelSelection,
    pub source: SettingSource,
}

/// Agent defaults persisted in the typed global settings document. The two
/// model values stay optional on first launch: BONE never guesses a model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalAgentSettings {
    pub coordinator: Option<ModelSelection>,
    pub default_solver: Option<ModelSelection>,
    pub soft_deadline_seconds: u32,
    pub shutdown_grace_seconds: u32,
}

impl Default for GlobalAgentSettings {
    fn default() -> Self {
        Self {
            coordinator: None,
            default_solver: None,
            soft_deadline_seconds: DEFAULT_SOFT_DEADLINE_SECONDS,
            shutdown_grace_seconds: DEFAULT_SHUTDOWN_GRACE_SECONDS,
        }
    }
}

impl GlobalAgentSettings {
    fn validate(&self) -> Result<(), SettingsError> {
        self.coordinator
            .as_ref()
            .map(ModelSelection::validate)
            .transpose()?;
        self.default_solver
            .as_ref()
            .map(ModelSelection::validate)
            .transpose()?;
        if self.soft_deadline_seconds == 0 {
            return Err(SettingsError::NonPositive {
                field: "soft_deadline_seconds",
            });
        }
        if self.shutdown_grace_seconds == 0 {
            return Err(SettingsError::NonPositive {
                field: "shutdown_grace_seconds",
            });
        }
        Ok(())
    }
}

/// The one typed BONE global settings document. Secrets and OAuth payloads are
/// intentionally excluded: they never enter SQLite settings or journal JSON.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalSettings {
    pub agent: GlobalAgentSettings,
    pub tool_limits: ToolLimits,
    pub tui: TuiDisplaySettings,
}

impl GlobalSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        self.agent.validate()?;
        self.tool_limits.validate()?;
        Ok(())
    }
}

/// Settings owned by one Workspace. There is no global map keyed by a UUID;
/// each value naturally lives with the Workspace it affects.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceSettings {
    pub default_solver: Option<ModelSelection>,
}

impl WorkspaceSettings {
    fn validate(&self) -> Result<(), ModelSelectionError> {
        self.default_solver
            .as_ref()
            .map(ModelSelection::validate)
            .transpose()
            .map(|_| ())
    }
}

/// Model resolution returned to the frontend. `NeedsModel` is a normal
/// first-run state, not a broken store: Workspace/session browsing and drafts
/// remain fully usable until the user invokes `/model`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelResolution {
    NeedsModel,
    Ready {
        resolved: ResolvedModel,
        runtime: Box<ResolvedAgentRuntimeConfig>,
    },
}

impl ModelResolution {
    pub fn runtime_config(&self) -> Option<&ResolvedAgentRuntimeConfig> {
        match self {
            Self::NeedsModel => None,
            Self::Ready { runtime, .. } => Some(runtime.as_ref()),
        }
    }
}

/// Typed settings service backed by one injected `BoneStore`.
#[derive(Clone, Debug)]
pub struct SettingsService {
    store: BoneStore,
}

impl SettingsService {
    /// Open / initialize the global document inside an already-open BONE
    /// store. App composition opens `BoneStore` once, then injects it here.
    pub fn open(store: BoneStore) -> Result<Self, SettingsError> {
        let service = Self { store };
        service.ensure_global()?;
        Ok(service)
    }

    pub fn display_settings(&self) -> Result<TuiDisplaySettings, SettingsError> {
        Ok(self.read_global()?.tui)
    }

    pub fn set_show_progress(&self, show_progress: bool) -> Result<(), SettingsError> {
        self.update_global(|settings| settings.tui.show_progress = show_progress)
    }

    /// Resolve a complete immutable Agent runtime snapshot for a supplied
    /// Session record. The precedence is Session > Workspace > User;
    /// coordinator and tool/deadline values always come from typed global
    /// settings.
    pub fn resolve_model(&self, session: &SessionRecord) -> Result<ModelResolution, SettingsError> {
        session.validate()?;
        let global = self.read_global()?;
        let workspace = session.workspace_id;
        let workspace_document = self
            .store
            .document::<WorkspaceSettings>(keys::workspace_settings(workspace));
        let workspace_snapshot = workspace_document.read()?;
        let workspace_settings = workspace_snapshot.value.unwrap_or_default();
        workspace_settings.validate()?;

        let selected = if let Some(selection) = session.metadata.solver_model_override.clone() {
            Some((selection, SettingSource::Session))
        } else if let Some(selection) = workspace_settings.default_solver {
            Some((selection, SettingSource::Workspace))
        } else {
            global
                .agent
                .default_solver
                .clone()
                .map(|selection| (selection, SettingSource::User))
        };
        let Some((selection, source)) = selected else {
            return Ok(ModelResolution::NeedsModel);
        };
        let runtime = resolved_runtime(&global, &selection)?;
        Ok(ModelResolution::Ready {
            resolved: ResolvedModel { selection, source },
            runtime: Box::new(runtime),
        })
    }

    /// Resolve a one-shot CLI runtime. An explicit CLI model is an ephemeral
    /// input and is never written as a hidden fourth storage scope.
    pub fn resolve_one_shot(
        &self,
        explicit_solver: Option<ModelSelection>,
    ) -> Result<ResolvedAgentRuntimeConfig, SettingsError> {
        let global = self.read_global()?;
        let solver = explicit_solver
            .or_else(|| global.agent.default_solver.clone())
            .ok_or(SettingsError::NeedsModel)?;
        resolved_runtime(&global, &solver)
    }

    /// Write a User or Workspace model selection at its natural lifecycle
    /// object. Session overrides intentionally go through `SessionStore` with
    /// that Session's writer; this service only resolves their overlay.
    /// A missing coordinator resolves to the selected solver at runtime; only
    /// a user-wide solver choice may initialize the user-wide coordinator.
    pub fn set_solver_model(
        &self,
        workspace: WorkspaceId,
        scope: Scope,
        selection: ModelSelection,
    ) -> Result<(), SettingsError> {
        selection.validate()?;
        match scope {
            Scope::User => {
                self.update_global(|settings| {
                    settings.agent.default_solver = Some(selection.clone());
                    if settings.agent.coordinator.is_none() {
                        settings.agent.coordinator = Some(selection.clone());
                    }
                })?;
            }
            Scope::Workspace(scope_workspace) => {
                if scope_workspace != workspace {
                    return Err(SettingsError::ScopeMismatch);
                }
                self.update_workspace(workspace, |settings| {
                    settings.default_solver = Some(selection.clone());
                })?;
            }
            Scope::Session(_) => return Err(SettingsError::SessionScopeRequiresWriter),
        }
        Ok(())
    }

    fn ensure_global(&self) -> Result<(), SettingsError> {
        let document = self
            .store
            .document::<GlobalSettings>(keys::global_settings());
        for _ in 0..SETTINGS_RETRY_LIMIT {
            let snapshot = document.read()?;
            if let Some(settings) = snapshot.value {
                settings.validate()?;
                return Ok(());
            }
            let defaults = GlobalSettings::default();
            match document.replace(&defaults, snapshot.revision) {
                Ok(_) => return Ok(()),
                Err(StoreError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(SettingsError::Contention)
    }

    fn read_global(&self) -> Result<GlobalSettings, SettingsError> {
        self.ensure_global()?;
        let snapshot = self
            .store
            .document::<GlobalSettings>(keys::global_settings())
            .read()?;
        let settings = snapshot.value.ok_or(SettingsError::MissingGlobalSettings)?;
        settings.validate()?;
        Ok(settings)
    }

    fn update_global(&self, mutate: impl Fn(&mut GlobalSettings)) -> Result<(), SettingsError> {
        let document = self
            .store
            .document::<GlobalSettings>(keys::global_settings());
        for _ in 0..SETTINGS_RETRY_LIMIT {
            let snapshot = document.read()?;
            let mut settings = snapshot.value.unwrap_or_default();
            mutate(&mut settings);
            settings.validate()?;
            match document.replace(&settings, snapshot.revision) {
                Ok(_) => return Ok(()),
                Err(StoreError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(SettingsError::Contention)
    }

    fn update_workspace(
        &self,
        workspace: WorkspaceId,
        mutate: impl Fn(&mut WorkspaceSettings),
    ) -> Result<(), SettingsError> {
        let document = self
            .store
            .document::<WorkspaceSettings>(keys::workspace_settings(workspace));
        for _ in 0..SETTINGS_RETRY_LIMIT {
            let snapshot = document.read()?;
            let mut settings = snapshot.value.unwrap_or_default();
            mutate(&mut settings);
            settings.validate()?;
            match document.replace(&settings, snapshot.revision) {
                Ok(_) => return Ok(()),
                Err(StoreError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(SettingsError::Contention)
    }
}

/// Settings errors intentionally distinguish a normal first-run `NeedsModel`
/// state from a damaged/busy SQLite store.
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Model(#[from] ModelSelectionError),
    #[error(transparent)]
    Runtime(#[from] ResolvedAgentRuntimeConfigError),
    #[error(transparent)]
    ToolLimits(#[from] ToolLimitsError),
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error("session-scoped model settings require that conversation's writer")]
    SessionScopeRequiresWriter,
    #[error("{field} must be greater than zero")]
    NonPositive { field: &'static str },
    #[error("settings changed repeatedly; please retry")]
    Contention,
    #[error("global settings document is missing after initialization")]
    MissingGlobalSettings,
    #[error("setting workspace does not belong to the current workspace")]
    ScopeMismatch,
    #[error("choose a model with /model <id> before starting work")]
    NeedsModel,
}

fn resolved_runtime(
    global: &GlobalSettings,
    solver: &ModelSelection,
) -> Result<ResolvedAgentRuntimeConfig, SettingsError> {
    global.validate()?;
    let solver = solver.as_model_settings();
    let coordinator = global
        .agent
        .coordinator
        .as_ref()
        .map(ModelSelection::as_model_settings)
        // A user-selected solver is a valid first-run coordinator fallback.
        // It is a resolution rule, not a guessed persisted model.
        .unwrap_or_else(|| solver.clone());
    ResolvedAgentRuntimeConfig::new(
        coordinator,
        solver,
        global.tool_limits.clone(),
        Duration::from_secs(u64::from(global.agent.soft_deadline_seconds)),
        Duration::from_secs(u64::from(global.agent.shutdown_grace_seconds)),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use bone_store::StoreRoots;

    use super::*;
    use crate::{CanonicalPath, SessionStore, WorkspaceContext, WorkspaceRegistry};

    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        SessionStore,
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
        let store = BoneStore::open_at(StoreRoots::new(data.path().join("data")).unwrap()).unwrap();
        let registry = WorkspaceRegistry::new(store.clone());
        let canonical = CanonicalPath::new(std::fs::canonicalize(project.path()).unwrap()).unwrap();
        let workspace = WorkspaceContext::from_canonical(
            registry.resolve_or_create_canonical(&canonical).unwrap(),
            canonical,
            project.path(),
        )
        .unwrap();
        let sessions = SessionStore::new(store.clone(), workspace);
        let settings = SettingsService::open(store).unwrap();
        (data, project, sessions, settings)
    }

    #[test]
    fn empty_store_is_usable_but_needs_a_model() {
        let (_data, _project, sessions, settings) = fixture();
        let record = SessionRecord::new(sessions.workspace(), "Unpersisted conversation").unwrap();
        assert!(matches!(
            settings.resolve_model(&record).unwrap(),
            ModelResolution::NeedsModel
        ));
    }

    #[test]
    fn settings_service_refuses_session_scope_without_a_writer_lease() {
        let (_data, _project, sessions, settings) = fixture();
        let writer = sessions.create_writer("New conversation").unwrap();
        let record = writer.record().clone();

        assert!(matches!(
            settings.set_solver_model(
                record.workspace_id,
                Scope::Session(SessionId::new()),
                ModelSelection::new("must-not-write", None, None).unwrap(),
            ),
            Err(SettingsError::SessionScopeRequiresWriter)
        ));
        assert_eq!(sessions.get(record.id).unwrap().unwrap(), record);
    }

    #[test]
    fn malformed_or_invalid_saved_settings_refuse_to_open() {
        let data = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            data.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let roots = StoreRoots::new(data.path().join("data")).unwrap();

        let store = BoneStore::open_at(roots.clone()).unwrap();
        let document = store.document::<serde_json::Value>(keys::global_settings());
        let snapshot = document.read().unwrap();
        document
            .replace(
                &serde_json::json!({"agent": {"soft_deadline_seconds": 0}}),
                snapshot.revision,
            )
            .unwrap();
        assert!(matches!(
            SettingsService::open(store),
            Err(SettingsError::NonPositive {
                field: "soft_deadline_seconds"
            })
        ));

        let store = BoneStore::open_at(roots).unwrap();
        let document = store.document::<serde_json::Value>(keys::global_settings());
        let snapshot = document.read().unwrap();
        document
            .replace(
                &serde_json::json!({"agent": "not an object"}),
                snapshot.revision,
            )
            .unwrap();
        assert!(matches!(
            SettingsService::open(store),
            Err(SettingsError::Store(_))
        ));
    }

    #[test]
    fn model_inheritance_follows_session_workspace_user() {
        let (_data, _project, sessions, settings) = fixture();
        let mut writer = sessions.create_writer("New conversation").unwrap();
        let workspace = sessions.workspace().id();
        let user = ModelSelection::new("user", None, None).unwrap();
        let workspace_selection = ModelSelection::new("workspace", None, None).unwrap();
        let session_selection = ModelSelection::new("session", None, None).unwrap();
        settings
            .set_solver_model(workspace, Scope::User, user)
            .unwrap();
        settings
            .set_solver_model(workspace, Scope::Workspace(workspace), workspace_selection)
            .unwrap();
        writer
            .set_solver_model_override(Some(session_selection))
            .unwrap();
        let ModelResolution::Ready { resolved, .. } =
            settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("model should resolve");
        };
        assert_eq!(resolved.selection.model, "session");
        writer.set_solver_model_override(None).unwrap();
        let ModelResolution::Ready { resolved, .. } =
            settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("model should resolve");
        };
        assert_eq!(resolved.selection.model, "workspace");
    }

    #[test]
    fn session_model_selection_does_not_mutate_user_defaults() {
        let (_data, _project, sessions, settings) = fixture();
        let mut writer = sessions.create_writer("New conversation").unwrap();

        writer
            .set_solver_model_override(Some(
                ModelSelection::new("session-only", None, None).unwrap(),
            ))
            .unwrap();

        let global = settings.read_global().unwrap();
        assert!(global.agent.default_solver.is_none());
        assert!(global.agent.coordinator.is_none());
        let ModelResolution::Ready { runtime, .. } =
            settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("session model should resolve");
        };
        assert_eq!(runtime.coordinator().model, "session-only");
    }
}
