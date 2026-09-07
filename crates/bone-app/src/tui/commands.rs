//! Typed local command recognition for the terminal composer.
//!
//! This module deliberately answers only one question: given a composer
//! submission and its provenance, is it a local BONE command or a regular
//! model-visible message? Executing a command belongs to the application
//! effect layer, not to the composer or renderer.

use bone_llm::protocol::openai_responses::{
    ReasoningContext, ReasoningEffort, ReasoningMode, ReasoningSummary,
};

/// How the current composer value was produced.
///
/// A paste must never gain the authority to execute a local command merely
/// because its text starts with a slash. Future composer integration records
/// `ContainsPaste` for the lifetime of a draft; clearing the composer resets
/// it to `TypedOnly`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputProvenance {
    #[default]
    TypedOnly,
    ContainsPaste,
}

/// A parsed local command. Arguments are deliberately typed at the parser
/// boundary so command execution does not need to re-interpret raw text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalCommand {
    Help,
    Status,
    Config(ConfigCommand),
    Provider(ProviderCommand),
    Model(ModelCommand),
    Login,
    Logout,
    Workspace,
    New,
    Sessions,
    Resume(Option<String>),
    Rename(String),
    Archive,
    Stop,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigCommand {
    Open,
    Doctor,
}

/// Explicit profile-catalog commands.  The parser recognizes only BONE's
/// current public LLM connection surface; there is no arbitrary provider
/// plugin or JSON configuration escape hatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderCommand {
    List,
    Add {
        id: String,
        protocol: ProviderProtocol,
        base_url: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderProtocol {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelCommand {
    /// Open the current-session model picker, optionally pre-filtered.
    Open { query: Option<String> },
    /// Save a current-session solver override.
    SetSession {
        profile: Option<String>,
        model: String,
        tuning: ModelTuning,
    },
    /// Save the current workspace's default solver.
    SetWorkspaceDefault {
        profile: Option<String>,
        model: String,
        tuning: ModelTuning,
    },
    /// Save the user's global default solver.
    SetUserDefault {
        profile: Option<String>,
        model: String,
        tuning: ModelTuning,
    },
    /// Save the user-wide coordinator model.
    SetCoordinator {
        profile: Option<String>,
        model: String,
        tuning: ModelTuning,
    },
    /// Remove the current-session override and resume inheritance.
    Inherit,
}

/// Explicit optional controls for one saved model selection.
///
/// These map one-for-one to persistable `bone_llm` request controls; they are
/// not an untyped `key=value` escape hatch. A protocol that does not support a
/// selected control is rejected before the setting is saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelTuning {
    pub(crate) timeout_seconds: Option<u32>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) reasoning_summary: Option<ReasoningSummary>,
    pub(crate) reasoning_mode: Option<ReasoningMode>,
    pub(crate) reasoning_context: Option<ReasoningContext>,
}

impl ModelTuning {
    pub(crate) fn has_reasoning(&self) -> bool {
        self.reasoning_effort.is_some()
            || self.reasoning_summary.is_some()
            || self.reasoning_mode.is_some()
            || self.reasoning_context.is_some()
    }
}

/// A command descriptor drives discoverability and parsing; future palette and
/// settings surfaces can use the same metadata without copying a command list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub title: &'static str,
    pub description: &'static str,
}

/// A submission result. `Unknown` remains local and must never fall through to
/// the model automatically; the caller may offer an explicit "send as message"
/// follow-up action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submission {
    Empty,
    Command(LocalCommand),
    /// `//text` is the intentional escape for a slash-prefixed user message.
    EscapedMessage(String),
    /// Pasted or multi-line input is always model-visible text.
    Message(String),
    Unknown {
        raw: String,
        name: String,
        suggestions: Vec<&'static CommandDescriptor>,
    },
    Invalid {
        raw: String,
        message: &'static str,
    },
}

const COMMANDS: &[CommandDescriptor] = &[
    CommandDescriptor {
        name: "help",
        aliases: &["?"],
        title: "Help",
        description: "Show available BONE commands.",
    },
    CommandDescriptor {
        name: "status",
        aliases: &[],
        title: "Status",
        description: "Show workspace, session, configuration, and connection status.",
    },
    CommandDescriptor {
        name: "config",
        aliases: &[],
        title: "Settings",
        description: "Open settings, or run /config doctor.",
    },
    CommandDescriptor {
        name: "provider",
        aliases: &[],
        title: "Providers",
        description: "List or add a saved LLM connection profile.",
    },
    CommandDescriptor {
        name: "model",
        aliases: &[],
        title: "Model",
        description: "Choose a model for this session, workspace, or user default.",
    },
    CommandDescriptor {
        name: "login",
        aliases: &[],
        title: "Log in",
        description: "Connect a ChatGPT account.",
    },
    CommandDescriptor {
        name: "logout",
        aliases: &[],
        title: "Log out",
        description: "Remove the local ChatGPT sign-in cache when no runtime owns it.",
    },
    CommandDescriptor {
        name: "workspace",
        aliases: &["ws"],
        title: "Workspace",
        description: "Show the current workspace boundary.",
    },
    CommandDescriptor {
        name: "new",
        aliases: &[],
        title: "New conversation",
        description: "Create a conversation in the current workspace.",
    },
    CommandDescriptor {
        name: "sessions",
        aliases: &[],
        title: "Sessions",
        description: "Browse conversations in the current workspace.",
    },
    CommandDescriptor {
        name: "resume",
        aliases: &[],
        title: "Resume conversation",
        description: "Open a saved conversation in this workspace.",
    },
    CommandDescriptor {
        name: "rename",
        aliases: &[],
        title: "Rename conversation",
        description: "Rename the current conversation.",
    },
    CommandDescriptor {
        name: "archive",
        aliases: &[],
        title: "Archive conversation",
        description: "Archive the current conversation.",
    },
    CommandDescriptor {
        name: "stop",
        aliases: &[],
        title: "Stop work",
        description: "Request that the current conversation stops.",
    },
    CommandDescriptor {
        name: "exit",
        aliases: &["quit"],
        title: "Exit",
        description: "Exit BONE after confirming active work.",
    },
];

pub fn descriptors() -> &'static [CommandDescriptor] {
    COMMANDS
}

/// Parse a submitted composer value according to BONE's local-command safety
/// rules. It does not execute the resulting command.
pub fn parse_submission(text: String, provenance: InputProvenance) -> Submission {
    if text.trim().is_empty() {
        return Submission::Empty;
    }
    if provenance == InputProvenance::ContainsPaste || text.contains(['\n', '\r']) {
        return Submission::Message(text);
    }

    let trimmed = text.trim_start();
    let Some(command_text) = trimmed.strip_prefix('/') else {
        return Submission::Message(text);
    };
    if let Some(message) = command_text.strip_prefix('/') {
        return Submission::EscapedMessage(message.to_owned());
    }

    let (name, arguments) = split_command(command_text);
    if name.is_empty() {
        return Submission::Invalid {
            raw: text,
            message: "enter a command name after '/'.",
        };
    }
    let Some(descriptor) = lookup(name) else {
        let name = name.to_owned();
        return Submission::Unknown {
            raw: text,
            suggestions: suggestions(&name),
            name,
        };
    };
    parse_known(&text, descriptor.name, arguments)
}

fn split_command(value: &str) -> (&str, &str) {
    let mut parts = value.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let arguments = parts.next().unwrap_or_default().trim();
    (name, arguments)
}

fn lookup(name: &str) -> Option<&'static CommandDescriptor> {
    COMMANDS.iter().find(|descriptor| {
        descriptor.name.eq_ignore_ascii_case(name)
            || descriptor
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

fn parse_known(raw: &str, name: &str, arguments: &str) -> Submission {
    let no_arguments = |command| {
        if arguments.is_empty() {
            Submission::Command(command)
        } else {
            Submission::Invalid {
                raw: raw.to_owned(),
                message: "this command does not accept arguments.",
            }
        }
    };
    match name {
        "help" => no_arguments(LocalCommand::Help),
        "status" => no_arguments(LocalCommand::Status),
        "login" => no_arguments(LocalCommand::Login),
        "logout" => no_arguments(LocalCommand::Logout),
        "workspace" => no_arguments(LocalCommand::Workspace),
        "new" => no_arguments(LocalCommand::New),
        "sessions" => no_arguments(LocalCommand::Sessions),
        "archive" => no_arguments(LocalCommand::Archive),
        "stop" => no_arguments(LocalCommand::Stop),
        "exit" => no_arguments(LocalCommand::Exit),
        "config" => match arguments {
            "" => Submission::Command(LocalCommand::Config(ConfigCommand::Open)),
            "doctor" => Submission::Command(LocalCommand::Config(ConfigCommand::Doctor)),
            _ => Submission::Invalid {
                raw: raw.to_owned(),
                message: "use /config or /config doctor.",
            },
        },
        "provider" => parse_provider(raw, arguments),
        "model" => parse_model(raw, arguments),
        "resume" => Submission::Command(LocalCommand::Resume(
            (!arguments.is_empty()).then(|| arguments.to_owned()),
        )),
        "rename" if !arguments.is_empty() => {
            Submission::Command(LocalCommand::Rename(arguments.to_owned()))
        }
        "rename" => Submission::Invalid {
            raw: raw.to_owned(),
            message: "provide a non-empty conversation title after /rename.",
        },
        _ => unreachable!("registry and parser must stay in sync"),
    }
}

fn parse_model(raw: &str, arguments: &str) -> Submission {
    if arguments.is_empty() {
        return Submission::Command(LocalCommand::Model(ModelCommand::Open { query: None }));
    }
    if arguments == "inherit" {
        return Submission::Command(LocalCommand::Model(ModelCommand::Inherit));
    }
    let (scope, target) = split_command(arguments);
    match scope {
        "coordinator" => parse_model_target(raw, target, |profile, model, tuning| {
            ModelCommand::SetCoordinator {
                profile,
                model,
                tuning,
            }
        }),
        "default" => parse_model_target(raw, target, |profile, model, tuning| {
            ModelCommand::SetWorkspaceDefault {
                profile,
                model,
                tuning,
            }
        }),
        "global" => parse_model_target(raw, target, |profile, model, tuning| {
            ModelCommand::SetUserDefault {
                profile,
                model,
                tuning,
            }
        }),
        _ => parse_model_target(raw, arguments, |profile, model, tuning| {
            ModelCommand::SetSession {
                profile,
                model,
                tuning,
            }
        }),
    }
}

fn parse_model_target(
    raw: &str,
    value: &str,
    construct: impl FnOnce(Option<String>, String, ModelTuning) -> ModelCommand,
) -> Submission {
    let mut parts = Vec::new();
    let mut tuning = ModelTuning::default();
    let mut values = value.split_whitespace();
    while let Some(part) = values.next() {
        let Some(flag) = part.strip_prefix("--") else {
            parts.push(part);
            continue;
        };
        let Some(argument) = values.next() else {
            return invalid_model_target(raw);
        };
        let parsed = match flag {
            "timeout" => argument
                .parse::<u32>()
                .ok()
                .filter(|seconds| *seconds > 0)
                .map(|seconds| tuning.timeout_seconds.replace(seconds).is_none()),
            "reasoning-effort" => parse_reasoning_effort(argument)
                .map(|effort| tuning.reasoning_effort.replace(effort).is_none()),
            "reasoning-summary" => parse_reasoning_summary(argument)
                .map(|summary| tuning.reasoning_summary.replace(summary).is_none()),
            "reasoning-mode" => parse_reasoning_mode(argument)
                .map(|mode| tuning.reasoning_mode.replace(mode).is_none()),
            "reasoning-context" => parse_reasoning_context(argument)
                .map(|context| tuning.reasoning_context.replace(context).is_none()),
            _ => None,
        };
        if parsed != Some(true) {
            return invalid_model_target(raw);
        }
    }
    let (profile, model) = match parts.as_slice() {
        [model] => (None, (*model).to_owned()),
        [profile, model] => (Some((*profile).to_owned()), (*model).to_owned()),
        _ => return invalid_model_target(raw),
    };
    Submission::Command(LocalCommand::Model(construct(profile, model, tuning)))
}

fn invalid_model_target(raw: &str) -> Submission {
    Submission::Invalid {
        raw: raw.to_owned(),
        message: "use <model> or <profile> <model>, followed by optional --timeout and --reasoning-* controls.",
    }
}

fn parse_reasoning_effort(value: &str) -> Option<ReasoningEffort> {
    Some(match value {
        "none" => ReasoningEffort::None,
        "minimal" => ReasoningEffort::Minimal,
        "low" => ReasoningEffort::Low,
        "medium" => ReasoningEffort::Medium,
        "high" => ReasoningEffort::High,
        "xhigh" => ReasoningEffort::Xhigh,
        "max" => ReasoningEffort::Max,
        _ => return None,
    })
}

fn parse_reasoning_summary(value: &str) -> Option<ReasoningSummary> {
    Some(match value {
        "auto" => ReasoningSummary::Auto,
        "concise" => ReasoningSummary::Concise,
        "detailed" => ReasoningSummary::Detailed,
        _ => return None,
    })
}

fn parse_reasoning_mode(value: &str) -> Option<ReasoningMode> {
    (value == "pro").then_some(ReasoningMode::Pro)
}

fn parse_reasoning_context(value: &str) -> Option<ReasoningContext> {
    Some(match value {
        "auto" => ReasoningContext::Auto,
        "all_turns" => ReasoningContext::AllTurns,
        "current_turn" => ReasoningContext::CurrentTurn,
        _ => return None,
    })
}

fn parse_provider(raw: &str, arguments: &str) -> Submission {
    if arguments.is_empty() || arguments == "list" {
        return Submission::Command(LocalCommand::Provider(ProviderCommand::List));
    }
    let parts = arguments.split_whitespace().collect::<Vec<_>>();
    let ["add", id, protocol, tail @ ..] = parts.as_slice() else {
        return Submission::Invalid {
            raw: raw.to_owned(),
            message: "use /provider, or /provider add <id> <responses|chat|anthropic> [base-url].",
        };
    };
    let protocol = match *protocol {
        "responses" => ProviderProtocol::OpenAiResponses,
        "chat" => ProviderProtocol::OpenAiChatCompletions,
        "anthropic" => ProviderProtocol::AnthropicMessages,
        _ => {
            return Submission::Invalid {
                raw: raw.to_owned(),
                message: "provider protocol must be responses, chat, or anthropic.",
            };
        }
    };
    let base_url = match tail {
        [] => None,
        [base_url] => Some((*base_url).to_owned()),
        _ => {
            return Submission::Invalid {
                raw: raw.to_owned(),
                message: "an API-key protocol may include one base URL.",
            };
        }
    };
    Submission::Command(LocalCommand::Provider(ProviderCommand::Add {
        id: (*id).to_owned(),
        protocol,
        base_url,
    }))
}

fn suggestions(name: &str) -> Vec<&'static CommandDescriptor> {
    let needle = name.to_ascii_lowercase();
    let mut ranked = COMMANDS
        .iter()
        .map(|descriptor| {
            let canonical = descriptor.name.to_ascii_lowercase();
            let score = if canonical.starts_with(&needle) {
                0
            } else {
                edit_distance(&needle, &canonical)
            };
            (score, descriptor)
        })
        .filter(|(score, descriptor)| *score <= 3 || descriptor.name.starts_with(&needle))
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        left_score
            .cmp(right_score)
            .then_with(|| left.name.cmp(right.name))
    });
    ranked
        .into_iter()
        .map(|(_, descriptor)| descriptor)
        .take(3)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.chars().count()).collect::<Vec<_>>();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(previous.len());
        current.push(left_index + 1);
        for (right_index, right_char) in right.chars().enumerate() {
            let replacement = previous[right_index] + usize::from(left_char != right_char);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current.push(replacement.min(insertion).min(deletion));
        }
        previous = current;
    }
    *previous.last().expect("distance row is never empty")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_unique_names_and_aliases() {
        let mut seen = std::collections::BTreeSet::new();
        for descriptor in descriptors() {
            assert!(seen.insert(descriptor.name));
            for alias in descriptor.aliases {
                assert!(seen.insert(alias));
            }
        }
    }

    #[test]
    fn parses_all_core_commands() {
        let cases = [
            ("/help", LocalCommand::Help),
            ("/status", LocalCommand::Status),
            ("/config", LocalCommand::Config(ConfigCommand::Open)),
            (
                "/config doctor",
                LocalCommand::Config(ConfigCommand::Doctor),
            ),
            ("/login", LocalCommand::Login),
            ("/logout", LocalCommand::Logout),
            ("/workspace", LocalCommand::Workspace),
            ("/new", LocalCommand::New),
            ("/sessions", LocalCommand::Sessions),
            ("/archive", LocalCommand::Archive),
            ("/stop", LocalCommand::Stop),
            ("/exit", LocalCommand::Exit),
        ];
        for (input, expected) in cases {
            assert_eq!(
                parse_submission(input.to_owned(), InputProvenance::TypedOnly),
                Submission::Command(expected)
            );
        }
    }

    #[test]
    fn parses_model_scopes_without_ambiguity() {
        assert_eq!(
            parse_submission("/model".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::Open { query: None }))
        );
        assert_eq!(
            parse_submission("/model gpt-5.6".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::SetSession {
                profile: None,
                model: "gpt-5.6".into(),
                tuning: ModelTuning::default(),
            }))
        );
        assert_eq!(
            parse_submission("/model default gpt-5.6".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::SetWorkspaceDefault {
                profile: None,
                model: "gpt-5.6".into(),
                tuning: ModelTuning::default(),
            }))
        );
        assert_eq!(
            parse_submission("/model global gpt-5.6".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::SetUserDefault {
                profile: None,
                model: "gpt-5.6".into(),
                tuning: ModelTuning::default(),
            }))
        );
        assert_eq!(
            parse_submission(
                "/model coordinator anthropic claude-test".into(),
                InputProvenance::TypedOnly
            ),
            Submission::Command(LocalCommand::Model(ModelCommand::SetCoordinator {
                profile: Some("anthropic".into()),
                model: "claude-test".into(),
                tuning: ModelTuning::default(),
            }))
        );
        assert_eq!(
            parse_submission("/model inherit".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::Inherit))
        );
    }

    #[test]
    fn parses_typed_responses_controls_without_an_untyped_escape_hatch() {
        assert_eq!(
            parse_submission(
                "/model openai gpt-5 --timeout 90 --reasoning-effort high --reasoning-summary concise --reasoning-mode pro --reasoning-context current_turn".into(),
                InputProvenance::TypedOnly,
            ),
            Submission::Command(LocalCommand::Model(ModelCommand::SetSession {
                profile: Some("openai".into()),
                model: "gpt-5".into(),
                tuning: ModelTuning {
                    timeout_seconds: Some(90),
                    reasoning_effort: Some(ReasoningEffort::High),
                    reasoning_summary: Some(ReasoningSummary::Concise),
                    reasoning_mode: Some(ReasoningMode::Pro),
                    reasoning_context: Some(ReasoningContext::CurrentTurn),
                },
            }))
        );
        assert!(matches!(
            parse_submission(
                "/model gpt-5 --reasoning-effort unknown".into(),
                InputProvenance::TypedOnly,
            ),
            Submission::Invalid { .. }
        ));
    }

    #[test]
    fn parses_explicit_provider_profiles_without_treating_urls_as_messages() {
        assert_eq!(
            parse_submission("/provider".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Provider(ProviderCommand::List))
        );
        assert_eq!(
            parse_submission(
                "/provider add work responses https://gateway.example/v1".into(),
                InputProvenance::TypedOnly
            ),
            Submission::Command(LocalCommand::Provider(ProviderCommand::Add {
                id: "work".into(),
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: Some("https://gateway.example/v1".into()),
            }))
        );
        assert!(matches!(
            parse_submission(
                "/provider add work chatgpt https://gateway.example".into(),
                InputProvenance::TypedOnly
            ),
            Submission::Invalid { .. }
        ));
    }

    #[test]
    fn parses_resume_and_rename_arguments() {
        assert_eq!(
            parse_submission("/resume".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Resume(None))
        );
        assert_eq!(
            parse_submission("/resume abc-123".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Resume(Some("abc-123".into())))
        );
        assert_eq!(
            parse_submission("/rename Fix startup".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Rename("Fix startup".into()))
        );
    }

    #[test]
    fn slash_escape_stays_model_visible() {
        assert_eq!(
            parse_submission("  //literal /stop".into(), InputProvenance::TypedOnly),
            Submission::EscapedMessage("literal /stop".into())
        );
    }

    #[test]
    fn pasted_or_multiline_slash_text_never_executes_a_command() {
        assert_eq!(
            parse_submission("/stop".into(), InputProvenance::ContainsPaste),
            Submission::Message("/stop".into())
        );
        assert_eq!(
            parse_submission(
                "/model gpt-5\nplease use it".into(),
                InputProvenance::TypedOnly
            ),
            Submission::Message("/model gpt-5\nplease use it".into())
        );
    }

    #[test]
    fn unknown_commands_remain_local_and_suggest_matches() {
        let result = parse_submission("/modle gpt-5".into(), InputProvenance::TypedOnly);
        assert!(matches!(
            result,
            Submission::Unknown {
                name,
                suggestions,
                ..
            } if name == "modle" && suggestions.first().is_some_and(|item| item.name == "model")
        ));
    }

    #[test]
    fn invalid_arguments_are_never_sent_as_messages() {
        assert!(matches!(
            parse_submission("/config bad".into(), InputProvenance::TypedOnly),
            Submission::Invalid { .. }
        ));
        assert!(matches!(
            parse_submission("/rename".into(), InputProvenance::TypedOnly),
            Submission::Invalid { .. }
        ));
    }

    #[test]
    fn distance_is_symmetric_and_zero_for_identical_values() {
        assert_eq!(edit_distance("model", "model"), 0);
        assert_eq!(
            edit_distance("model", "modle"),
            edit_distance("modle", "model")
        );
    }
}
