use bone_app::{EndpointConfig, ModelSelection, Profile, ProfileId, RequestId};

#[derive(Clone, Default, Eq, PartialEq)]
pub struct SecretText(String);

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl From<String> for SecretText {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl SecretText {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A concrete API connection form. ChatGPT account authorization has no form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionKind {
    OpenAiApi,
    AnthropicApi,
    CustomOpenAiResponses,
    CustomOpenAiChatCompletions,
    CustomAnthropicMessages,
}

impl ConnectionKind {
    pub const ADVANCED: [Self; 3] = [
        Self::CustomOpenAiResponses,
        Self::CustomOpenAiChatCompletions,
        Self::CustomAnthropicMessages,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiApi => "OpenAI API",
            Self::AnthropicApi => "Anthropic API",
            Self::CustomOpenAiResponses => "OpenAI-compatible Responses",
            Self::CustomOpenAiChatCompletions => "OpenAI-compatible Chat Completions",
            Self::CustomAnthropicMessages => "Anthropic-compatible Messages",
        }
    }

    pub fn advanced(self) -> bool {
        matches!(
            self,
            Self::CustomOpenAiResponses
                | Self::CustomOpenAiChatCompletions
                | Self::CustomAnthropicMessages
        )
    }

    fn endpoint(self, base_url: Option<String>) -> EndpointConfig {
        match self {
            Self::OpenAiApi | Self::CustomOpenAiResponses => {
                EndpointConfig::OpenAiResponses { base_url }
            }
            Self::CustomOpenAiChatCompletions => EndpointConfig::OpenAiChatCompletions { base_url },
            Self::AnthropicApi | Self::CustomAnthropicMessages => {
                EndpointConfig::AnthropicMessages { base_url }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupField {
    Label,
    BaseUrl,
    Key,
    Model,
}

#[derive(Debug)]
pub struct ConnectionForm {
    pub kind: ConnectionKind,
    pub label: String,
    pub base_url: String,
    pub key: SecretText,
    pub model: String,
    pub field: SetupField,
    pub pending_request: Option<u64>,
    pub(crate) key_was_sent: bool,
    id: ProfileId,
    original_endpoint: Option<EndpointConfig>,
    original_selection: Option<ModelSelection>,
}

impl ConnectionForm {
    pub fn new(kind: ConnectionKind) -> Self {
        let field = if kind.advanced() {
            SetupField::Label
        } else {
            SetupField::Key
        };
        let id = match kind {
            ConnectionKind::OpenAiApi => ProfileId::new("openai").unwrap(),
            ConnectionKind::AnthropicApi => ProfileId::new("anthropic").unwrap(),
            _ => ProfileId::new(format!("connection-{}", RequestId::new()))
                .expect("UUID profile identifier"),
        };
        Self {
            kind,
            label: kind.label().into(),
            base_url: String::new(),
            key: SecretText::default(),
            model: String::new(),
            field,
            pending_request: None,
            key_was_sent: false,
            id,
            original_endpoint: None,
            original_selection: None,
        }
    }

    pub fn edit_selection(profile: &Profile, selection: Option<ModelSelection>) -> Option<Self> {
        let kind = match &profile.endpoint {
            EndpointConfig::ChatGptSubscription => return None,
            EndpointConfig::OpenAiResponses { base_url: None } => ConnectionKind::OpenAiApi,
            EndpointConfig::AnthropicMessages { base_url: None } => ConnectionKind::AnthropicApi,
            EndpointConfig::OpenAiResponses { base_url: Some(_) } => {
                ConnectionKind::CustomOpenAiResponses
            }
            EndpointConfig::OpenAiChatCompletions { .. } => {
                ConnectionKind::CustomOpenAiChatCompletions
            }
            EndpointConfig::AnthropicMessages { base_url: Some(_) } => {
                ConnectionKind::CustomAnthropicMessages
            }
        };
        let selection = selection.filter(|selection| selection.profile == profile.id);
        let mut form = Self::new(kind);
        form.original_endpoint = Some(profile.endpoint.clone());
        form.id = profile.id.clone();
        form.label = profile.label.clone();
        form.base_url = profile.endpoint.base_url().unwrap_or_default().into();
        form.model = selection
            .as_ref()
            .map(|selection| selection.model.clone())
            .unwrap_or_default();
        form.original_selection = selection;
        Some(form)
    }

    pub fn fields(&self) -> &'static [SetupField] {
        if self.kind.advanced() {
            &[
                SetupField::Label,
                SetupField::BaseUrl,
                SetupField::Key,
                SetupField::Model,
            ]
        } else {
            &[SetupField::Key]
        }
    }

    pub fn move_field(&mut self, forward: bool) {
        let fields = self.fields();
        let index = fields
            .iter()
            .position(|field| *field == self.field)
            .unwrap_or(0);
        self.field = fields[(index + if forward { 1 } else { fields.len() - 1 }) % fields.len()];
    }

    pub fn text_mut(&mut self) -> &mut String {
        match self.field {
            SetupField::Label => &mut self.label,
            SetupField::BaseUrl => &mut self.base_url,
            SetupField::Key => &mut self.key.0,
            SetupField::Model => &mut self.model,
        }
    }

    pub fn validated(&self) -> Result<(Profile, Option<ModelSelection>), String> {
        if self.key_was_sent && self.key.is_empty() {
            return Err("Re-enter the API key before retrying".into());
        }
        if self.original_endpoint.is_none() && self.key.is_empty() {
            return Err("Enter an API key".into());
        }
        if self.kind.advanced() && self.base_url.trim().is_empty() {
            return Err("Enter the service URL".into());
        }
        let base_url = self
            .kind
            .advanced()
            .then(|| self.base_url.trim().to_owned());
        let profile = Profile::new(
            self.id.clone(),
            self.label.trim(),
            self.kind.endpoint(base_url),
        )
        .map_err(|error| error.to_string())?;
        if self.key.is_empty()
            && self
                .original_endpoint
                .as_ref()
                .is_some_and(|original| *original != profile.endpoint)
        {
            return Err("Endpoint changed: enter an API key for the new endpoint".into());
        }

        if self.edits_existing_connection() && !self.kind.advanced() {
            return Ok((profile, None));
        }
        if self.edits_existing_connection() && self.model.trim().is_empty() {
            return Ok((profile, None));
        }

        let (model, default_reasoning) = if self.kind.advanced() {
            (self.model.trim(), None)
        } else if let Some(original) = &self.original_selection {
            (original.model.as_str(), None)
        } else {
            let preset = profile
                .model_presets()
                .iter()
                .find(|preset| preset.recommended)
                .or_else(|| profile.model_presets().first())
                .ok_or_else(|| "This provider has no recommended model".to_owned())?;
            (preset.id, preset.default_reasoning)
        };
        let mut selection = if let Some(original) = &self.original_selection
            && original.model == model
        {
            original.clone()
        } else {
            ModelSelection::new(self.id.clone(), model).map_err(|error| error.to_string())?
        };
        if let Some(effort) = default_reasoning {
            selection.options = Some(bone_app::ModelOptions::OpenAiResponses {
                reasoning: bone_app::Reasoning::new().effort(effort),
            });
        }
        let changes_model = self
            .original_selection
            .as_ref()
            .is_none_or(|original| *original != selection);
        Ok((profile, changes_model.then_some(selection)))
    }

    pub(crate) fn edits_existing_connection(&self) -> bool {
        self.original_endpoint.is_some()
    }

    pub(crate) fn changes_model(&self) -> bool {
        self.kind.advanced()
            && self.edits_existing_connection()
            && !self.model.trim().is_empty()
            && self
                .original_selection
                .as_ref()
                .is_none_or(|selection| selection.model != self.model.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_api_only_requires_a_key_and_chooses_the_recommended_model() {
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiApi);
        assert_eq!(form.fields(), &[SetupField::Key]);
        assert_eq!(form.validated().unwrap_err(), "Enter an API key");
        form.key = "secret".to_owned().into();
        let (profile, selection) = form.validated().unwrap();
        let selection = selection.unwrap();
        assert_eq!(profile.label, "OpenAI API");
        assert_eq!(profile.endpoint.base_url(), None);
        assert!(
            profile
                .model_presets()
                .iter()
                .any(|preset| preset.recommended && preset.id == selection.model)
        );
    }

    #[test]
    fn official_connections_are_singletons_and_manage_never_changes_the_model() {
        let first = ConnectionForm::new(ConnectionKind::OpenAiApi);
        let second = ConnectionForm::new(ConnectionKind::OpenAiApi);
        assert_eq!(first.id, ProfileId::new("openai").unwrap());
        assert_eq!(first.id, second.id);

        let profile = Profile::new(
            ProfileId::new("openai").unwrap(),
            "OpenAI API",
            EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        let selection = ModelSelection::new(profile.id.clone(), "gpt-5.5").unwrap();
        let mut edit = ConnectionForm::edit_selection(&profile, Some(selection)).unwrap();
        edit.key = "replacement-key".to_owned().into();

        let (_, selection) = edit.validated().unwrap();
        assert_eq!(selection, None);
    }

    #[test]
    fn advanced_connection_edit_can_explicitly_switch_model() {
        let profile = Profile::new(
            ProfileId::new("existing").unwrap(),
            "Existing",
            EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.invalid/v1".into()),
            },
        )
        .unwrap();
        let current = ModelSelection::new(profile.id.clone(), "old-model").unwrap();
        let mut edit = ConnectionForm::edit_selection(&profile, Some(current.clone())).unwrap();
        assert!(edit.fields().contains(&SetupField::Model));
        assert_eq!(edit.validated().unwrap().1, None);

        edit.model = "new-model".into();
        assert!(edit.changes_model());
        assert_eq!(
            edit.validated().unwrap().1,
            Some(ModelSelection::new(profile.id, "new-model").unwrap())
        );
    }

    #[test]
    fn inactive_advanced_connection_can_be_edited_without_selecting_a_model() {
        let profile = Profile::new(
            ProfileId::new("inactive").unwrap(),
            "Inactive",
            EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.invalid/v1".into()),
            },
        )
        .unwrap();
        let edit = ConnectionForm::edit_selection(&profile, None).unwrap();

        assert!(edit.model.is_empty());
        assert!(!edit.changes_model());
        assert_eq!(edit.validated().unwrap().1, None);
    }

    #[test]
    fn advanced_connection_requires_explicit_url_and_model() {
        let mut form = ConnectionForm::new(ConnectionKind::CustomOpenAiResponses);
        form.key = "secret".to_owned().into();
        assert_eq!(form.validated().unwrap_err(), "Enter the service URL");
        form.base_url = "https://example.invalid/v1".into();
        assert!(form.validated().is_err());
        form.model = "custom-model".into();
        assert!(form.validated().is_ok());
    }

    #[test]
    fn endpoint_change_requires_a_fresh_key() {
        let profile = Profile::new(
            ProfileId::new("existing").unwrap(),
            "Existing",
            EndpointConfig::OpenAiResponses {
                base_url: Some("https://old.example/v1".into()),
            },
        )
        .unwrap();
        let mut edited = ConnectionForm::edit_selection(&profile, None).unwrap();
        assert!(edited.edits_existing_connection());
        edited.base_url = "https://new.example/v1".into();
        assert_eq!(
            edited.validated().unwrap_err(),
            "Endpoint changed: enter an API key for the new endpoint"
        );
    }
}
