//! Typed BONE product settings.
//!
//! The user never has to edit a configuration file: this service reads and
//! writes one SQLite-backed `GlobalSettings` document plus Workspace settings.
//! Session records are supplied by their owning session repository, so this
//! service never depends on session storage layout. No dynamic section
//! registry, JSON schema registry, raw config revision, or arbitrary setting
//! map leaks into the TUI or Agent.

use std::{fmt, time::Duration};

use bone_agent::{ResolvedAgentRuntimeConfig, ResolvedAgentRuntimeConfigError};
use bone_llm::{
    ModelOptions, ModelRequestOptionsError,
    protocol::openai_responses::{Reasoning, ReasoningEffort},
};
use bone_store::{BoneStore, StoreError};
use bone_tools::{ToolLimits, ToolLimitsError};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    LlmProfile, LlmProfileId, LlmProfiles, LlmProfilesError, RecordError, SessionId, SessionRecord,
    WorkspaceId, durable::keys,
};

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
    pub const MODEL_COORDINATOR: &'static str = "models.coordinator";
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
        key: SettingKey::MODEL_COORDINATOR,
        label: "Coordinator model",
        allowed_scopes: USER_ONLY,
        default_scope: ScopeKind::User,
        apply_boundary: ApplyBoundary::NextUserTurn,
        model_visible: false,
    },
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

/// A saved, non-secret selection of one model through one named profile.
///
/// This value can live at User, Workspace, or Session scope.  It belongs to
/// the App because it references an App-owned profile ID; the protocol option
/// value nested inside it remains defined by `bone-llm`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelSelection {
    #[serde(default = "LlmProfileId::chatgpt")]
    pub profile: LlmProfileId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ModelOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

impl ModelSelection {
    pub fn new(
        profile: LlmProfileId,
        model: impl Into<String>,
        options: Option<ModelOptions>,
        timeout_seconds: Option<u32>,
    ) -> Result<Self, ModelSelectionError> {
        let selection = Self {
            profile,
            model: model.into(),
            options,
            timeout_seconds,
        };
        selection.validate()?;
        Ok(selection)
    }

    /// Convenience for legacy CLI/TUI forms that intentionally target the
    /// automatically seeded ChatGPT subscription profile.
    pub fn chatgpt(
        model: impl Into<String>,
        timeout_seconds: Option<u32>,
    ) -> Result<Self, ModelSelectionError> {
        Self::new(LlmProfileId::chatgpt(), model, None, timeout_seconds)
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
        self.options
            .as_ref()
            .map(ModelOptions::validate)
            .transpose()?;
        Ok(())
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(
            self.timeout_seconds
                .unwrap_or(DEFAULT_MODEL_TIMEOUT_SECONDS),
        ))
    }

    fn validate_for_profile(&self, profile: &LlmProfile) -> Result<(), ModelSelectionError> {
        self.validate()?;
        if self.profile != profile.id {
            return Err(ModelSelectionError::ProfileMismatch);
        }
        self.options
            .as_ref()
            .map(|options| options.validate_for(&profile.endpoint))
            .transpose()?;
        Ok(())
    }
}

/// Read both the new protocol-scoped option format and existing `effort`
/// records written before provider profiles existed.  New writes emit only
/// `options`, so the migration happens naturally on the next durable update.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSelectionDocument {
    #[serde(default = "LlmProfileId::chatgpt")]
    profile: LlmProfileId,
    model: String,
    #[serde(default)]
    options: Option<ModelOptions>,
    #[serde(default)]
    effort: Option<ReasoningEffort>,
    #[serde(default)]
    timeout_seconds: Option<u32>,
}

impl<'de> Deserialize<'de> for ModelSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let document = ModelSelectionDocument::deserialize(deserializer)?;
        let options = match (document.options, document.effort) {
            (Some(_), Some(_)) => {
                return Err(de::Error::custom(
                    "model selection cannot contain both options and legacy effort",
                ));
            }
            (Some(options), None) => Some(options),
            (None, Some(effort)) => Some(ModelOptions::OpenAiResponses {
                reasoning: Reasoning::new().effort(effort),
            }),
            (None, None) => None,
        };
        let selection = Self {
            profile: document.profile,
            model: document.model,
            options,
            timeout_seconds: document.timeout_seconds,
        };
        selection.validate().map_err(de::Error::custom)?;
        Ok(selection)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModelSelectionError {
    #[error("model identifier must be trimmed, non-empty, and at most {MAX_MODEL_ID_BYTES} bytes")]
    InvalidModel,
    #[error("model timeout must be greater than zero when configured")]
    ZeroTimeout,
    #[error("model options do not match the selected profile")]
    ProfileMismatch,
    #[error(transparent)]
    Options(#[from] ModelRequestOptionsError),
}

/// Effective model value together with the inheritance source which supplied
/// it. UI can truthfully say where the selected model comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModel {
    pub selection: ModelSelection,
    pub profile: LlmProfile,
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

/// The non-secret, pinned product plan for one newly-created Agent runtime.
///
/// The App resolves profile identity and configuration before starting work;
/// it constructs actual `bone_llm::Model` values only when credentials are
/// available.  The Agent receives neither this plan nor any persisted IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRuntime {
    pub coordinator: ResolvedModel,
    pub solver: ResolvedModel,
    pub agent: ResolvedAgentRuntimeConfig,
}

impl ResolvedRuntime {
    /// Stable identity for the non-secret runtime plan that is journaled when
    /// a turn is accepted, before an OAuth flow or network connection starts.
    ///
    /// It intentionally includes endpoint configuration and profile IDs but
    /// never API keys or OAuth material.  A display label is excluded because
    /// changing it cannot affect model behavior.
    pub fn fingerprint(&self) -> String {
        #[derive(Serialize)]
        struct ModelIdentity<'a> {
            profile: &'a LlmProfileId,
            endpoint: &'a bone_llm::EndpointConfig,
            model: &'a str,
            options: &'a Option<ModelOptions>,
            timeout_seconds: Option<u32>,
        }

        #[derive(Serialize)]
        struct RuntimeIdentity<'a> {
            coordinator: ModelIdentity<'a>,
            solver: ModelIdentity<'a>,
            tool_limits: &'a ToolLimits,
            soft_deadline_seconds: u64,
            review_timeout_seconds: u64,
            work_timeout_seconds: u64,
            shutdown_grace_seconds: u64,
        }

        let deadlines = self.agent.deadlines();
        let value = RuntimeIdentity {
            coordinator: ModelIdentity {
                profile: &self.coordinator.profile.id,
                endpoint: &self.coordinator.profile.endpoint,
                model: &self.coordinator.selection.model,
                options: &self.coordinator.selection.options,
                timeout_seconds: self.coordinator.selection.timeout_seconds,
            },
            solver: ModelIdentity {
                profile: &self.solver.profile.id,
                endpoint: &self.solver.profile.endpoint,
                model: &self.solver.selection.model,
                options: &self.solver.selection.options,
                timeout_seconds: self.solver.selection.timeout_seconds,
            },
            tool_limits: self.agent.tool_limits(),
            soft_deadline_seconds: deadlines.soft_deadline().as_secs(),
            review_timeout_seconds: deadlines.review_timeout().as_secs(),
            work_timeout_seconds: deadlines.work_timeout().as_secs(),
            shutdown_grace_seconds: deadlines.shutdown_grace_period().as_secs(),
        };
        let bytes = serde_json::to_vec(&value)
            .expect("resolved runtime identity contains only serializable typed values");
        let mut hasher = Sha256::new();
        hasher.update(b"bone-app.runtime-plan.v1");
        hasher.update(bytes);
        let digest = hasher.finalize();
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Model resolution returned to the frontend. `NeedsModel` is a normal
/// first-run state, not a broken store: Workspace/session browsing and drafts
/// remain fully usable until the user chooses a profile/model pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelResolution {
    NeedsModel,
    Ready(Box<ResolvedRuntime>),
}

impl ModelResolution {
    pub fn runtime(&self) -> Option<&ResolvedRuntime> {
        match self {
            Self::NeedsModel => None,
            Self::Ready(runtime) => Some(runtime.as_ref()),
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
        service.ensure_profiles()?;
        Ok(service)
    }

    /// Return the complete non-secret LLM profile catalog.  Credentials never
    /// travel through this settings API or SQLite document.
    pub fn llm_profiles(&self) -> Result<LlmProfiles, SettingsError> {
        self.read_profiles()
    }

    /// Look up one named connection profile.
    pub fn llm_profile(&self, id: &LlmProfileId) -> Result<LlmProfile, SettingsError> {
        self.read_profiles()?
            .get(id)
            .cloned()
            .ok_or_else(|| SettingsError::UnknownLlmProfile(id.clone()))
    }

    /// Validate a model/profile pair before a caller writes it through a
    /// Session writer.  Session overrides belong to that writer rather than
    /// this service, but they must obey the same profile/option rules as User
    /// and Workspace settings.
    pub fn validate_model_selection(
        &self,
        selection: &ModelSelection,
    ) -> Result<(), SettingsError> {
        selection.validate()?;
        let profile = self.llm_profile(&selection.profile)?;
        selection.validate_for_profile(&profile)?;
        Ok(())
    }

    /// Create one non-secret LLM profile.
    ///
    /// Profile IDs are immutable: an API key is stored in the operating
    /// system credential slot named by this ID, so replacing an existing
    /// endpoint could redirect a previously saved key to a new host. Create a
    /// new ID when endpoint settings change, then enter its key explicitly.
    pub fn add_llm_profile(&self, profile: LlmProfile) -> Result<(), SettingsError> {
        profile.validate().map_err(LlmProfilesError::Profile)?;
        self.update_profiles(|profiles| {
            if profiles.get(&profile.id).is_some() {
                return Err(SettingsError::LlmProfileAlreadyExists(profile.id.clone()));
            }
            profiles.profiles.push(profile.clone());
            Ok(())
        })
    }

    pub fn display_settings(&self) -> Result<TuiDisplaySettings, SettingsError> {
        Ok(self.read_global()?.tui)
    }

    pub fn set_show_progress(&self, show_progress: bool) -> Result<(), SettingsError> {
        self.update_global(|settings| settings.tui.show_progress = show_progress)
    }

    /// Resolve a complete non-secret runtime plan for a supplied Session
    /// record. Solver precedence is Session > Workspace > User; coordinator
    /// and tool/deadline values remain global.
    pub fn resolve_model(&self, session: &SessionRecord) -> Result<ModelResolution, SettingsError> {
        session.validate()?;
        let global = self.read_global()?;
        let profiles = self.read_profiles()?;
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
        let solver = resolve_selected_model(&profiles, selection, source)?;
        let (coordinator_selection, coordinator_source) = global
            .agent
            .coordinator
            .clone()
            .map(|selection| (selection, SettingSource::User))
            // A user-selected solver is a valid first-run coordinator
            // fallback.  This is a resolution rule, not a guessed persisted
            // provider/model choice.
            .unwrap_or_else(|| (solver.selection.clone(), solver.source));
        let coordinator =
            resolve_selected_model(&profiles, coordinator_selection, coordinator_source)?;
        let agent = resolved_runtime(&global, &coordinator.selection, &solver.selection)?;
        Ok(ModelResolution::Ready(Box::new(ResolvedRuntime {
            coordinator,
            solver,
            agent,
        })))
    }

    /// Resolve a one-shot CLI runtime. An explicit CLI model is an ephemeral
    /// input and is never written as a hidden fourth storage scope.
    pub fn resolve_one_shot(
        &self,
        explicit_solver: Option<ModelSelection>,
    ) -> Result<ResolvedRuntime, SettingsError> {
        let global = self.read_global()?;
        let profiles = self.read_profiles()?;
        // A CLI profile/model is one intentional, ephemeral runtime choice.
        // It must cover both Agent roles: otherwise `bone --profile X --model
        // Y ...` could unexpectedly require an unrelated saved coordinator
        // credential before it can start.
        let explicit_selection = explicit_solver;
        let solver = explicit_selection
            .clone()
            .or_else(|| global.agent.default_solver.clone())
            .ok_or(SettingsError::NeedsModel)?;
        let solver = resolve_selected_model(&profiles, solver, SettingSource::User)?;
        let coordinator = if explicit_selection.is_some() {
            solver.clone()
        } else {
            global
                .agent
                .coordinator
                .clone()
                .map(|selection| resolve_selected_model(&profiles, selection, SettingSource::User))
                .transpose()?
                .unwrap_or_else(|| solver.clone())
        };
        let agent = resolved_runtime(&global, &coordinator.selection, &solver.selection)?;
        Ok(ResolvedRuntime {
            coordinator,
            solver,
            agent,
        })
    }

    /// Write a User or Workspace model selection at its natural lifecycle
    /// object. Session overrides intentionally go through `SessionStore` with
    /// that Session's writer; this service only resolves their overlay.
    /// A missing coordinator resolves to the selected solver at runtime. It is
    /// never silently persisted: a separately selected coordinator must stay
    /// an explicit user decision.
    pub fn set_solver_model(
        &self,
        workspace: WorkspaceId,
        scope: Scope,
        selection: ModelSelection,
    ) -> Result<(), SettingsError> {
        self.validate_model_selection(&selection)?;
        match scope {
            Scope::User => {
                self.update_global(|settings| {
                    settings.agent.default_solver = Some(selection.clone());
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

    /// Save the user-wide coordinator selection. Coordinator work is shared
    /// by every workspace, so it intentionally has no workspace or session
    /// override.
    pub fn set_coordinator_model(&self, selection: ModelSelection) -> Result<(), SettingsError> {
        self.validate_model_selection(&selection)?;
        self.update_global(|settings| settings.agent.coordinator = Some(selection.clone()))
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

    fn ensure_profiles(&self) -> Result<(), SettingsError> {
        let document = self.store.document::<LlmProfiles>(keys::llm_profiles());
        for _ in 0..SETTINGS_RETRY_LIMIT {
            let snapshot = document.read()?;
            if let Some(profiles) = snapshot.value {
                profiles.validate()?;
                return Ok(());
            }
            let defaults = LlmProfiles::default();
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

    fn read_profiles(&self) -> Result<LlmProfiles, SettingsError> {
        self.ensure_profiles()?;
        let snapshot = self
            .store
            .document::<LlmProfiles>(keys::llm_profiles())
            .read()?;
        let profiles = snapshot.value.ok_or(SettingsError::MissingLlmProfiles)?;
        profiles.validate()?;
        Ok(profiles)
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

    fn update_profiles(
        &self,
        mutate: impl Fn(&mut LlmProfiles) -> Result<(), SettingsError>,
    ) -> Result<(), SettingsError> {
        let document = self.store.document::<LlmProfiles>(keys::llm_profiles());
        for _ in 0..SETTINGS_RETRY_LIMIT {
            let snapshot = document.read()?;
            let mut profiles = snapshot.value.unwrap_or_default();
            mutate(&mut profiles)?;
            profiles.validate()?;
            match document.replace(&profiles, snapshot.revision) {
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
    Profiles(#[from] LlmProfilesError),
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
    #[error("LLM profiles document is missing after initialization")]
    MissingLlmProfiles,
    #[error("LLM profile `{0}` does not exist")]
    UnknownLlmProfile(LlmProfileId),
    #[error("LLM profile `{0}` already exists; create a new ID to use a different endpoint")]
    LlmProfileAlreadyExists(LlmProfileId),
    #[error("setting workspace does not belong to the current workspace")]
    ScopeMismatch,
    #[error("choose a model with /model <id> before starting work")]
    NeedsModel,
}

fn resolve_selected_model(
    profiles: &LlmProfiles,
    selection: ModelSelection,
    source: SettingSource,
) -> Result<ResolvedModel, SettingsError> {
    let profile = profiles
        .get(&selection.profile)
        .cloned()
        .ok_or_else(|| SettingsError::UnknownLlmProfile(selection.profile.clone()))?;
    selection.validate_for_profile(&profile)?;
    Ok(ResolvedModel {
        selection,
        profile,
        source,
    })
}

fn resolved_runtime(
    global: &GlobalSettings,
    coordinator: &ModelSelection,
    solver: &ModelSelection,
) -> Result<ResolvedAgentRuntimeConfig, SettingsError> {
    global.validate()?;
    ResolvedAgentRuntimeConfig::new(
        global.tool_limits.clone(),
        Duration::from_secs(u64::from(global.agent.soft_deadline_seconds)),
        coordinator.timeout(),
        solver.timeout(),
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
    fn settings_service_refuses_session_scope_without_a_writer() {
        let (_data, _project, sessions, settings) = fixture();
        let writer = sessions.create_writer("New conversation").unwrap();
        let record = writer.record().clone();

        assert!(matches!(
            settings.set_solver_model(
                record.workspace_id,
                Scope::Session(SessionId::new()),
                ModelSelection::chatgpt("must-not-write", None).unwrap(),
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
        let user = ModelSelection::chatgpt("user", None).unwrap();
        let workspace_selection = ModelSelection::chatgpt("workspace", None).unwrap();
        let session_selection = ModelSelection::chatgpt("session", None).unwrap();
        settings
            .set_solver_model(workspace, Scope::User, user)
            .unwrap();
        settings
            .set_solver_model(workspace, Scope::Workspace(workspace), workspace_selection)
            .unwrap();
        writer
            .set_solver_model_override(Some(session_selection))
            .unwrap();
        let ModelResolution::Ready(runtime) = settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("model should resolve");
        };
        assert_eq!(runtime.solver.selection.model, "session");
        assert_eq!(runtime.solver.source, SettingSource::Session);
        writer.set_solver_model_override(None).unwrap();
        let ModelResolution::Ready(runtime) = settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("model should resolve");
        };
        assert_eq!(runtime.solver.selection.model, "workspace");
        assert_eq!(runtime.solver.source, SettingSource::Workspace);
    }

    #[test]
    fn session_model_selection_does_not_mutate_user_defaults() {
        let (_data, _project, sessions, settings) = fixture();
        let mut writer = sessions.create_writer("New conversation").unwrap();

        writer
            .set_solver_model_override(Some(ModelSelection::chatgpt("session-only", None).unwrap()))
            .unwrap();

        let global = settings.read_global().unwrap();
        assert!(global.agent.default_solver.is_none());
        assert!(global.agent.coordinator.is_none());
        let ModelResolution::Ready(runtime) = settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("session model should resolve");
        };
        assert_eq!(runtime.coordinator.selection.model, "session-only");
        assert_eq!(runtime.solver.selection.model, "session-only");
        assert_eq!(runtime.coordinator.profile.id, LlmProfileId::chatgpt());
    }

    #[test]
    fn legacy_model_selection_json_migrates_to_chatgpt_profile_and_options() {
        let selection: ModelSelection = serde_json::from_value(serde_json::json!({
            "model": "gpt-5",
            "effort": "high",
            "timeout_seconds": 45
        }))
        .unwrap();

        assert_eq!(selection.profile, LlmProfileId::chatgpt());
        assert_eq!(selection.model, "gpt-5");
        assert_eq!(selection.timeout_seconds, Some(45));
        assert_eq!(
            selection.options,
            Some(ModelOptions::OpenAiResponses {
                reasoning: Reasoning::new().effort(ReasoningEffort::High),
            })
        );
        assert_eq!(
            serde_json::to_value(selection).unwrap(),
            serde_json::json!({
                "profile": "chatgpt",
                "model": "gpt-5",
                "options": {
                    "type": "openai_responses",
                    "reasoning": { "effort": "high" }
                },
                "timeout_seconds": 45
            })
        );
    }

    #[test]
    fn profile_ids_are_create_only_so_a_saved_key_cannot_be_redirected() {
        let (_data, _project, _sessions, settings) = fixture();
        let id = LlmProfileId::new("openai").unwrap();
        settings
            .add_llm_profile(
                LlmProfile::new(
                    id.clone(),
                    "OpenAI",
                    bone_llm::EndpointConfig::OpenAiResponses { base_url: None },
                )
                .unwrap(),
            )
            .unwrap();

        let duplicate = LlmProfile::new(
            id.clone(),
            "Different host",
            bone_llm::EndpointConfig::OpenAiResponses {
                base_url: Some("https://gateway.example/v1".into()),
            },
        )
        .unwrap();
        assert!(matches!(
            settings.add_llm_profile(duplicate),
            Err(SettingsError::LlmProfileAlreadyExists(existing)) if existing == id
        ));
        assert_eq!(
            settings.llm_profile(&id).unwrap().endpoint,
            bone_llm::EndpointConfig::OpenAiResponses { base_url: None }
        );
    }

    #[test]
    fn coordinator_can_be_selected_independently_of_the_solver() {
        let (_data, _project, sessions, settings) = fixture();
        let writer = sessions.create_writer("New conversation").unwrap();
        let profile = LlmProfileId::new("anthropic").unwrap();
        settings
            .add_llm_profile(
                LlmProfile::new(
                    profile.clone(),
                    "Anthropic",
                    bone_llm::EndpointConfig::AnthropicMessages { base_url: None },
                )
                .unwrap(),
            )
            .unwrap();
        settings
            .set_solver_model(
                writer.record().workspace_id,
                Scope::User,
                ModelSelection::chatgpt("gpt-5", None).unwrap(),
            )
            .unwrap();
        settings
            .set_coordinator_model(
                ModelSelection::new(profile.clone(), "claude", None, None).unwrap(),
            )
            .unwrap();

        let ModelResolution::Ready(runtime) = settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("model should resolve");
        };
        assert_eq!(runtime.solver.profile.id, LlmProfileId::chatgpt());
        assert_eq!(runtime.coordinator.profile.id, profile);
    }

    #[test]
    fn an_implicit_coordinator_tracks_later_user_solver_changes() {
        let (_data, _project, sessions, settings) = fixture();
        let writer = sessions.create_writer("New conversation").unwrap();
        let profile = LlmProfileId::new("anthropic").unwrap();
        settings
            .add_llm_profile(
                LlmProfile::new(
                    profile.clone(),
                    "Anthropic",
                    bone_llm::EndpointConfig::AnthropicMessages { base_url: None },
                )
                .unwrap(),
            )
            .unwrap();

        settings
            .set_solver_model(
                writer.record().workspace_id,
                Scope::User,
                ModelSelection::chatgpt("gpt-5", None).unwrap(),
            )
            .unwrap();
        settings
            .set_solver_model(
                writer.record().workspace_id,
                Scope::User,
                ModelSelection::new(profile.clone(), "claude", None, None).unwrap(),
            )
            .unwrap();

        assert!(settings.read_global().unwrap().agent.coordinator.is_none());
        let ModelResolution::Ready(runtime) = settings.resolve_model(writer.record()).unwrap()
        else {
            panic!("model should resolve");
        };
        assert_eq!(runtime.solver.profile.id, profile);
        assert_eq!(runtime.coordinator.profile.id, profile);
    }

    #[test]
    fn explicit_one_shot_selection_pins_both_roles_to_that_provider() {
        let (_data, _project, _sessions, settings) = fixture();
        let anthropic = LlmProfileId::new("anthropic").unwrap();
        settings
            .add_llm_profile(
                LlmProfile::new(
                    anthropic.clone(),
                    "Anthropic",
                    bone_llm::EndpointConfig::AnthropicMessages { base_url: None },
                )
                .unwrap(),
            )
            .unwrap();
        settings
            .set_coordinator_model(ModelSelection::chatgpt("saved-chatgpt", None).unwrap())
            .unwrap();

        let runtime = settings
            .resolve_one_shot(Some(
                ModelSelection::new(anthropic.clone(), "claude-test", None, None).unwrap(),
            ))
            .unwrap();

        assert_eq!(runtime.solver.profile.id, anthropic);
        assert_eq!(runtime.coordinator.profile.id, runtime.solver.profile.id);
        assert_eq!(runtime.coordinator.selection.model, "claude-test");
    }
}
