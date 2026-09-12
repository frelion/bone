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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionKind {
    ChatGptSubscription,
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
}
impl ConnectionKind {
    pub const ALL: [Self; 4] = [
        Self::ChatGptSubscription,
        Self::OpenAiResponses,
        Self::OpenAiChatCompletions,
        Self::AnthropicMessages,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::ChatGptSubscription => "ChatGPT subscription",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::OpenAiChatCompletions => "OpenAI Chat Completions",
            Self::AnthropicMessages => "Anthropic Messages",
        }
    }
    pub fn subscription(self) -> bool {
        self == Self::ChatGptSubscription
    }
    fn endpoint(self, base_url: Option<String>) -> EndpointConfig {
        match self {
            Self::ChatGptSubscription => EndpointConfig::ChatGptSubscription,
            Self::OpenAiResponses => EndpointConfig::OpenAiResponses { base_url },
            Self::OpenAiChatCompletions => EndpointConfig::OpenAiChatCompletions { base_url },
            Self::AnthropicMessages => EndpointConfig::AnthropicMessages { base_url },
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
    pub saving: bool,
    pub existing: Option<ProfileId>,
    pub request: u64,
    pub(crate) key_was_sent: bool,
    id: ProfileId,
    original_endpoint: Option<EndpointConfig>,
    original_selection: Option<ModelSelection>,
}
impl ConnectionForm {
    pub fn new(kind: ConnectionKind) -> Self {
        let id = if kind.subscription() {
            ProfileId::chatgpt()
        } else {
            ProfileId::new(format!("connection-{}", RequestId::new()))
                .expect("UUID profile identifier")
        };
        Self {
            kind,
            label: kind.label().into(),
            base_url: String::new(),
            key: SecretText::default(),
            model: String::new(),
            field: SetupField::Label,
            saving: false,
            existing: None,
            request: 0,
            key_was_sent: false,
            id,
            original_endpoint: None,
            original_selection: None,
        }
    }
    pub fn edit(profile: &Profile, model: String) -> Self {
        let kind = match profile.endpoint {
            EndpointConfig::ChatGptSubscription => ConnectionKind::ChatGptSubscription,
            EndpointConfig::OpenAiResponses { .. } => ConnectionKind::OpenAiResponses,
            EndpointConfig::OpenAiChatCompletions { .. } => ConnectionKind::OpenAiChatCompletions,
            EndpointConfig::AnthropicMessages { .. } => ConnectionKind::AnthropicMessages,
        };
        let mut form = Self::new(kind);
        form.original_endpoint = Some(profile.endpoint.clone());
        form.id = profile.id.clone();
        form.existing = Some(profile.id.clone());
        form.label = profile.label.clone();
        form.base_url = profile.endpoint.base_url().unwrap_or_default().into();
        form.model = model;
        form
    }
    pub fn edit_selection(profile: &Profile, selection: Option<ModelSelection>) -> Self {
        let selection = selection.filter(|selection| selection.profile == profile.id);
        let mut form = Self::edit(
            profile,
            selection
                .as_ref()
                .map(|selection| selection.model.clone())
                .unwrap_or_default(),
        );
        form.original_selection = selection;
        form
    }
    pub fn fields(&self) -> &'static [SetupField] {
        if self.kind.subscription() {
            &[SetupField::Label, SetupField::Model]
        } else {
            &[
                SetupField::Label,
                SetupField::BaseUrl,
                SetupField::Key,
                SetupField::Model,
            ]
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
            return Err("Re-enter the API key before retrying this save".into());
        }
        if !self.kind.subscription() && self.existing.is_none() && self.key.is_empty() {
            return Err("Enter an API key for this new connection".into());
        }
        let base_url = (!self.base_url.trim().is_empty()).then(|| self.base_url.trim().to_owned());
        let profile = Profile::new(
            self.id.clone(),
            self.label.trim(),
            self.kind.endpoint(base_url),
        )
        .map_err(|error| error.to_string())?;
        if !self.kind.subscription()
            && self.key.is_empty()
            && self
                .original_endpoint
                .as_ref()
                .is_some_and(|original| *original != profile.endpoint)
        {
            return Err("Endpoint changed: enter an API key for the new endpoint".into());
        }
        let selection = if self.model.trim().is_empty() {
            None
        } else if let Some(original) = &self.original_selection
            && original.model == self.model.trim()
        {
            Some(original.clone())
        } else {
            Some(
                ModelSelection::new(self.id.clone(), self.model.trim())
                    .map_err(|error| error.to_string())?,
            )
        };
        Ok((profile, selection))
    }
}
