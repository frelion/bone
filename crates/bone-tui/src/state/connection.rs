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
    models: Vec<String>,
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
            models: Vec::new(),
        }
    }

    pub fn edit_selection(profile: &Profile) -> Option<Self> {
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
        let mut form = Self::new(kind);
        form.original_endpoint = Some(profile.endpoint.clone());
        form.id = profile.id.clone();
        form.label = profile.label.clone();
        form.base_url = profile.endpoint.base_url().unwrap_or_default().into();
        form.models = profile.models.clone();
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
            &[SetupField::Key, SetupField::Model]
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
        let mut profile = profile;
        profile.models = self.models.clone();
        let selection = if self.model.is_empty() {
            None
        } else {
            profile
                .add_model(&self.model)
                .map_err(|error| error.to_string())?;
            Some(
                ModelSelection::new(profile.id.clone(), self.model.clone())
                    .map_err(|error| error.to_string())?,
            )
        };
        Ok((profile, selection))
    }

    pub(crate) fn edits_existing_connection(&self) -> bool {
        self.original_endpoint.is_some()
    }
}

/// A one-field form for adding a model to an existing connection.
#[derive(Debug)]
pub struct ModelForm {
    pub profile: Profile,
    pub model: String,
    pub apply: bool,
    pub remove: bool,
    pub pending_request: Option<u64>,
}

impl ModelForm {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            model: String::new(),
            apply: true,
            remove: false,
            pending_request: None,
        }
    }

    pub fn remove(profile: Profile, model: impl Into<String>) -> Self {
        Self {
            profile,
            model: model.into(),
            apply: false,
            remove: true,
            pending_request: None,
        }
    }

    pub fn text_mut(&mut self) -> &mut String {
        &mut self.model
    }

    pub fn validated(&self) -> Result<(Profile, Option<ModelSelection>), String> {
        let model = self.model.as_str();
        let mut profile = self.profile.clone();
        if self.remove {
            if !profile.remove_model(model) {
                return Err("This model is not saved on the connection".into());
            }
            return Ok((profile, None));
        }
        profile
            .add_model(model)
            .map_err(|error| error.to_string())?;
        let selection = self
            .apply
            .then(|| ModelSelection::new(profile.id.clone(), model))
            .transpose()
            .map_err(|error| error.to_string())?;
        Ok((profile, selection))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_api_accepts_an_empty_key_and_optional_model() {
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiApi);
        assert_eq!(form.fields(), &[SetupField::Key, SetupField::Model]);
        let (profile, selection) = form.validated().unwrap();
        assert_eq!(profile.label, "OpenAI API");
        assert_eq!(profile.endpoint.base_url(), None);
        assert_eq!(selection, None);

        form.model = "gpt-test".into();
        let (profile, selection) = form.validated().unwrap();
        assert_eq!(profile.models, vec!["gpt-test"]);
        assert_eq!(selection.unwrap().model, "gpt-test");
    }

    #[test]
    fn official_connections_are_singletons_and_keep_saved_models() {
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
        let mut edit = ConnectionForm::edit_selection(&profile).unwrap();
        edit.key = "replacement-key".to_owned().into();

        let (edited, selection) = edit.validated().unwrap();
        assert_eq!(selection, None);
        assert_eq!(edited.models, profile.models);
    }

    #[test]
    fn advanced_connection_edit_does_not_touch_models() {
        let profile = Profile::new(
            ProfileId::new("existing").unwrap(),
            "Existing",
            EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.invalid/v1".into()),
            },
        )
        .unwrap();
        let edit = ConnectionForm::edit_selection(&profile).unwrap();
        assert_eq!(
            edit.fields(),
            &[
                SetupField::Label,
                SetupField::BaseUrl,
                SetupField::Key,
                SetupField::Model,
            ]
        );
        assert_eq!(edit.validated().unwrap().1, None);
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
        let edit = ConnectionForm::edit_selection(&profile).unwrap();

        assert!(edit.models.is_empty());
        assert_eq!(edit.validated().unwrap().1, None);
    }

    #[test]
    fn advanced_connection_requires_explicit_url_but_not_a_model() {
        let mut form = ConnectionForm::new(ConnectionKind::CustomOpenAiResponses);
        assert_eq!(form.validated().unwrap_err(), "Enter the service URL");
        form.base_url = "https://example.invalid/v1".into();
        assert!(form.validated().is_ok());
    }

    #[test]
    fn model_form_adds_and_validates_one_model() {
        let profile = Profile::new(
            ProfileId::new("custom").unwrap(),
            "Custom",
            EndpointConfig::OpenAiResponses {
                base_url: Some("http://127.0.0.1:8080/v1".into()),
            },
        )
        .unwrap();
        let mut form = ModelForm::new(profile.clone());
        assert!(form.validated().is_err());
        form.model = "model-a".into();
        let (saved, selection) = form.validated().unwrap();
        assert_eq!(saved.models, vec!["model-a"]);
        assert_eq!(selection.unwrap().model, "model-a");
        let mut duplicate = profile;
        duplicate.add_model("model-a").unwrap();
        let mut form = ModelForm::new(duplicate);
        form.model = "model-a".into();
        assert!(form.validated().is_err());

        let mut invalid = ModelForm::new(saved.clone());
        invalid.model = " model-b ".into();
        assert!(invalid.validated().is_err());

        let mut catalog_only = ModelForm::new(saved);
        catalog_only.model = "model-b".into();
        catalog_only.apply = false;
        let (_, selection) = catalog_only.validated().unwrap();
        assert_eq!(selection, None);
    }

    #[test]
    fn model_form_removes_a_saved_model_without_changing_runtime_selection() {
        let mut profile = Profile::new(
            ProfileId::new("custom").unwrap(),
            "Custom",
            EndpointConfig::OpenAiResponses {
                base_url: Some("http://127.0.0.1:8080/v1".into()),
            },
        )
        .unwrap();
        profile.add_model("model-a").unwrap();
        let form = ModelForm::remove(profile, "model-a");
        let (saved, selection) = form.validated().unwrap();
        assert!(saved.models.is_empty());
        assert_eq!(selection, None);
    }

    #[test]
    fn endpoint_change_keeps_an_optional_key_optional() {
        let profile = Profile::new(
            ProfileId::new("existing").unwrap(),
            "Existing",
            EndpointConfig::OpenAiResponses {
                base_url: Some("https://old.example/v1".into()),
            },
        )
        .unwrap();
        let mut edited = ConnectionForm::edit_selection(&profile).unwrap();
        assert!(edited.edits_existing_connection());
        edited.base_url = "https://new.example/v1".into();
        assert!(edited.validated().is_ok());
    }
}
