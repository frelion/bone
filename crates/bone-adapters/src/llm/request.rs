use std::{collections::HashSet, fmt};

use rig_core::{
    completion::CompletionRequest,
    message::{Message, ToolChoice as RigToolChoice},
};

use crate::llm::{
    Error, InputItem, ModelOptions, ModelOptionsError, ToolDefinition, item::InputSource,
    model::RequestOrigin,
};

/// One complete model call.
///
/// `input` is the entire ordered context: prior committed items and the new
/// input use the same representation. Instructions remain a separate,
/// higher-authority field.
///
/// A request carries only what BONE actually sends: text input, instructions,
/// tools, one required tool, and the selected model's protocol-scoped options.
/// A missing option is added together with the code path that needs it, rather
/// than carried as surface nothing sets.
#[derive(Clone)]
pub struct Request {
    input: Vec<InputItem>,
    instructions: Option<String>,
    tools: Vec<ToolDefinition>,
    required_tool: Option<String>,
    options: Option<ModelOptions>,
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("input_count", &self.input.len())
            .field("has_instructions", &self.instructions.is_some())
            .field("tool_count", &self.tools.len())
            .field("required_tool", &self.required_tool)
            .field("options", &self.options)
            .finish()
    }
}

impl Request {
    pub fn new(input: impl IntoIterator<Item = InputItem>) -> Self {
        Self {
            input: input.into_iter().collect(),
            instructions: None,
            tools: Vec::new(),
            required_tool: None,
            options: None,
        }
    }

    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub fn tools(mut self, tools: impl IntoIterator<Item = ToolDefinition>) -> Self {
        self.tools = tools.into_iter().collect();
        self
    }

    /// Require the model to call exactly this tool.
    ///
    /// This is how BONE asks for structure — through one forced submission tool
    /// — so it never asks a provider to enforce a JSON schema. A response
    /// without that call is a protocol error, not a text answer.
    pub fn require_tool(mut self, name: impl Into<String>) -> Self {
        self.required_tool = Some(name.into());
        self
    }

    pub fn options(mut self, options: ModelOptions) -> Self {
        self.options = Some(options);
        self
    }

    pub(crate) fn into_rig(self, origin: &RequestOrigin) -> Result<CompletionRequest, Error> {
        self.validate(origin)?;

        let capacity = self.input.len() + usize::from(self.instructions.is_some());
        let mut messages = Vec::with_capacity(capacity);
        if let Some(instructions) = self.instructions {
            messages.push(Message::system(instructions));
        }
        messages.extend(self.input.into_iter().map(InputItem::into_message));

        let additional_params = self.options.map(ModelOptions::into_additional_params);
        let request = CompletionRequest {
            model: None,
            preamble: None,
            chat_history: messages,
            documents: Vec::new(),
            tools: self
                .tools
                .into_iter()
                .map(ToolDefinition::into_rig)
                .collect(),
            temperature: None,
            max_tokens: None,
            tool_choice: self.required_tool.map(|name| RigToolChoice::Specific {
                function_names: vec![name],
            }),
            additional_params,
            output_schema: None,
            record_telemetry_content: false,
        };
        request
            .validate_message_content()
            .map_err(Error::from_rig)?;
        Ok(request)
    }

    fn validate(&self, origin: &RequestOrigin) -> Result<(), Error> {
        if self.input.is_empty() {
            return Err(Error::invalid("model request input is empty"));
        }
        if self
            .instructions
            .as_deref()
            .is_some_and(|instructions| instructions.trim().is_empty())
        {
            return Err(Error::invalid("model instructions are empty"));
        }
        if let Some(options) = &self.options {
            options
                .validate_for_protocol(origin.protocol)
                .map_err(|error| match error {
                    // An option that belongs to another wire shape is never
                    // dropped silently: it is rejected as unsupported.
                    ModelOptionsError::UnsupportedProtocol { .. } => {
                        Error::unsupported(error.to_string())
                    }
                    ModelOptionsError::EmptyOpenAiResponses => Error::invalid(error.to_string()),
                })?;
        }

        let mut tool_names = HashSet::new();
        for tool in &self.tools {
            if tool.name().trim().is_empty() {
                return Err(Error::invalid("tool name is empty"));
            }
            if tool.description().trim().is_empty() {
                return Err(Error::invalid(format!(
                    "tool `{}` has an empty description",
                    tool.name()
                )));
            }
            if !tool.parameters().is_object() {
                return Err(Error::invalid(format!(
                    "tool `{}` parameters must be a JSON object schema",
                    tool.name()
                )));
            }
            if !tool_names.insert(tool.name()) {
                return Err(Error::invalid(format!(
                    "tool `{}` is defined more than once",
                    tool.name()
                )));
            }
        }

        if let Some(name) = &self.required_tool
            && !tool_names.contains(name.as_str())
        {
            return Err(Error::invalid(format!(
                "required tool `{name}` is not defined"
            )));
        }

        for item in &self.input {
            if item.text.is_empty() {
                return Err(Error::invalid("input text is empty"));
            }
            if let InputSource::Named(name) = &item.source
                && name.trim().is_empty()
            {
                return Err(Error::invalid("named input source is empty"));
            }
        }
        Ok(())
    }
}
