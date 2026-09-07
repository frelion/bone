//! Typed local command recognition for the terminal composer.
//!
//! This module deliberately answers only one question: given a composer
//! submission and its provenance, is it a local BONE command or a regular
//! model-visible message? Executing a command belongs to the application
//! effect layer, not to the composer or renderer.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelCommand {
    /// Open the current-session model picker, optionally pre-filtered.
    Open { query: Option<String> },
    /// Save a current-session solver override.
    SetSession { model: String },
    /// Save the current workspace's default solver.
    SetWorkspaceDefault { model: String },
    /// Save the user's global default solver.
    SetUserDefault { model: String },
    /// Remove the current-session override and resume inheritance.
    Inherit,
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
        description: "Disconnect the current account after confirmation.",
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
    let (scope, model) = split_command(arguments);
    match scope {
        "default" if !model.is_empty() => {
            Submission::Command(LocalCommand::Model(ModelCommand::SetWorkspaceDefault {
                model: model.to_owned(),
            }))
        }
        "global" if !model.is_empty() => {
            Submission::Command(LocalCommand::Model(ModelCommand::SetUserDefault {
                model: model.to_owned(),
            }))
        }
        "default" | "global" => Submission::Invalid {
            raw: raw.to_owned(),
            message: "provide a model identifier after the scope.",
        },
        _ => Submission::Command(LocalCommand::Model(ModelCommand::SetSession {
            model: arguments.to_owned(),
        })),
    }
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
                model: "gpt-5.6".into(),
            }))
        );
        assert_eq!(
            parse_submission("/model default gpt-5.6".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::SetWorkspaceDefault {
                model: "gpt-5.6".into(),
            }))
        );
        assert_eq!(
            parse_submission("/model global gpt-5.6".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::SetUserDefault {
                model: "gpt-5.6".into(),
            }))
        );
        assert_eq!(
            parse_submission("/model inherit".into(), InputProvenance::TypedOnly),
            Submission::Command(LocalCommand::Model(ModelCommand::Inherit))
        );
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
