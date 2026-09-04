use std::{collections::HashSet, fmt};

use rig_core::{
    completion::CompletionRequest,
    message::{AssistantContent, Message, UserContent},
};
use serde_json::Value;

use crate::{
    Error, InputItem, Protocol, ToolChoice, ToolDefinition,
    item::{InputItemKind, InputSource},
    model::{RequestOrigin, RequestSupport},
    tool::ToolCallIdentities,
};

/// Provider-specific, typed request controls.
#[derive(Clone, Debug)]
pub struct Options {
    inner: OptionsKind,
}

#[derive(Clone, Debug)]
enum OptionsKind {
    OpenAiResponses(crate::protocol::openai_responses::Options),
}

impl From<crate::protocol::openai_responses::Options> for Options {
    fn from(value: crate::protocol::openai_responses::Options) -> Self {
        Self {
            inner: OptionsKind::OpenAiResponses(value),
        }
    }
}

impl Options {
    fn protocol(&self) -> Protocol {
        match &self.inner {
            OptionsKind::OpenAiResponses(_) => Protocol::OpenAiResponses,
        }
    }

    fn into_json(self) -> Option<Value> {
        match self.inner {
            OptionsKind::OpenAiResponses(options) => options.into_json(),
        }
    }

    fn is_empty(&self) -> bool {
        match &self.inner {
            OptionsKind::OpenAiResponses(options) => options.is_empty(),
        }
    }
}

/// Desired response representation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum OutputFormat {
    Text,
    JsonSchema(Value),
}

/// One complete model call.
///
/// `input` is the entire ordered context: prior committed items and the new
/// input use the same representation. Instructions remain a separate,
/// higher-authority field.
#[derive(Clone)]
pub struct Request {
    input: Vec<InputItem>,
    instructions: Option<String>,
    tools: Vec<ToolDefinition>,
    tool_choice: Option<ToolChoice>,
    output: OutputFormat,
    max_output_tokens: Option<u64>,
    options: Option<Options>,
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("input_count", &self.input.len())
            .field("has_instructions", &self.instructions.is_some())
            .field("tool_count", &self.tools.len())
            .field("tool_choice", &self.tool_choice)
            .field("output", &self.output)
            .field("max_output_tokens", &self.max_output_tokens)
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
            tool_choice: None,
            output: OutputFormat::Text,
            max_output_tokens: None,
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

    pub fn tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    pub fn output(mut self, output: OutputFormat) -> Self {
        self.output = output;
        self
    }

    pub fn max_output_tokens(mut self, max_output_tokens: u64) -> Self {
        self.max_output_tokens = Some(max_output_tokens);
        self
    }

    pub fn options(mut self, options: impl Into<Options>) -> Self {
        self.options = Some(options.into());
        self
    }

    pub(crate) fn into_rig(
        self,
        origin: &RequestOrigin,
        support: RequestSupport,
    ) -> Result<(CompletionRequest, ToolCallIdentities), Error> {
        let previous_tool_calls = self.validate(origin, support)?;

        let mut messages =
            Vec::with_capacity(self.input.len() + usize::from(self.instructions.is_some()));
        if let Some(instructions) = self.instructions {
            messages.push(Message::system(instructions));
        }

        let mut pending_results = Vec::<UserContent>::new();
        let flush_results = |messages: &mut Vec<Message>, results: &mut Vec<UserContent>| {
            if !results.is_empty() {
                messages.push(Message::User {
                    content: std::mem::take(results),
                });
            }
        };

        for item in self.input {
            match item.kind {
                InputItemKind::ToolResult { call, output } => {
                    pending_results.push(call.result_content(output));
                }
                InputItemKind::External { source, text } => {
                    flush_results(&mut messages, &mut pending_results);
                    messages.push(InputItem::external_message(source, text));
                }
                InputItemKind::AssistantExample(text) => {
                    flush_results(&mut messages, &mut pending_results);
                    messages.push(InputItem::assistant_example_message(text));
                }
                InputItemKind::AssistantReplay { message, .. } => {
                    flush_results(&mut messages, &mut pending_results);
                    messages.push(message);
                }
            }
        }
        flush_results(&mut messages, &mut pending_results);

        let output_schema = match self.output {
            OutputFormat::Text => None,
            OutputFormat::JsonSchema(value) => Some(
                serde_json::from_value::<schemars::Schema>(value).map_err(|error| {
                    Error::invalid(format!("invalid JSON output schema: {error}"))
                })?,
            ),
        };
        let additional_params = self.options.and_then(Options::into_json);
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
            max_tokens: self.max_output_tokens,
            tool_choice: self.tool_choice.map(ToolChoice::into_rig),
            additional_params,
            output_schema,
            record_telemetry_content: false,
        };
        request
            .validate_message_content()
            .map_err(Error::from_rig)?;
        Ok((request, previous_tool_calls))
    }

    fn validate(
        &self,
        origin: &RequestOrigin,
        support: RequestSupport,
    ) -> Result<ToolCallIdentities, Error> {
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
        if self.max_output_tokens == Some(0) {
            return Err(Error::invalid(
                "max_output_tokens must be greater than zero",
            ));
        }
        if self.max_output_tokens.is_some() && !support.max_output_tokens {
            return Err(Error::unsupported(format!(
                "endpoint `{}` does not support max_output_tokens",
                origin.endpoint_id
            )));
        }
        if matches!(self.output, OutputFormat::JsonSchema(_)) && !support.structured_output {
            return Err(Error::unsupported(format!(
                "endpoint `{}` does not support structured output",
                origin.endpoint_id
            )));
        }
        let has_tool_result = self
            .input
            .iter()
            .any(|item| matches!(item.kind, InputItemKind::ToolResult { .. }));
        if origin.protocol == Protocol::OpenAiChatCompletions
            && matches!(self.output, OutputFormat::JsonSchema(_))
            && !self.tools.is_empty()
            && !has_tool_result
        {
            return Err(Error::unsupported(
                "OpenAI Chat Completions cannot enforce structured output on an initial tool turn",
            ));
        }
        if let Some(options) = &self.options
            && options.protocol() != origin.protocol
        {
            return Err(Error::invalid(format!(
                "{} options cannot be used with {}",
                options.protocol(),
                origin.protocol
            )));
        }
        if self.options.as_ref().is_some_and(Options::is_empty) {
            return Err(Error::invalid(
                "protocol options are empty; omit options when no control is needed",
            ));
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

        if self.tool_choice.is_some() && self.tools.is_empty() {
            return Err(Error::invalid(
                "tool_choice requires at least one tool definition",
            ));
        }

        if let Some(ToolChoice::Specific(names)) = &self.tool_choice {
            if names.is_empty() {
                return Err(Error::invalid("specific tool choice is empty"));
            }
            if let Some(name) = names
                .iter()
                .find(|name| !tool_names.contains(name.as_str()))
            {
                return Err(Error::invalid(format!(
                    "tool choice names undefined tool `{name}`"
                )));
            }
            let unique_names = names.iter().collect::<HashSet<_>>();
            if unique_names.len() != names.len() {
                return Err(Error::invalid("specific tool choice repeats a tool name"));
            }
            if names.len() > 1 && origin.protocol != Protocol::OpenAiResponses {
                return Err(Error::unsupported(format!(
                    "{} supports only one specifically selected tool",
                    origin.protocol
                )));
            }
        }

        let mut previous_tool_calls = ToolCallIdentities::default();
        for item in &self.input {
            match &item.kind {
                InputItemKind::External {
                    source: InputSource::Named(name),
                    text,
                } => {
                    if name.trim().is_empty() {
                        return Err(Error::invalid("named input source is empty"));
                    }
                    if text.is_empty() {
                        return Err(Error::invalid("external input text is empty"));
                    }
                }
                InputItemKind::External { text, .. } | InputItemKind::AssistantExample(text) => {
                    if text.is_empty() {
                        return Err(Error::invalid("input text is empty"));
                    }
                }
                InputItemKind::AssistantReplay {
                    origin: item_origin,
                    message,
                } => {
                    origin.ensure_same(item_origin)?;
                    if let Message::Assistant { content, .. } = message {
                        for content in content {
                            if let AssistantContent::ToolCall(call) = content {
                                previous_tool_calls.insert(call)?;
                            }
                        }
                    }
                }
                InputItemKind::ToolResult { call, .. } => {
                    origin.ensure_same(&call.origin)?;
                }
            }
        }
        Ok(previous_tool_calls)
    }
}
