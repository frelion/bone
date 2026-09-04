use std::{fmt, str::FromStr, sync::Arc};

use bone_agent::{Tool, ToolFailure, ToolFailureKind};
use bone_config::{ConfigError, ConfigManager, ConfigRevision};
use bone_llm::{ToolDefinition, ToolOutput};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const DEFAULT_MAX_OUTPUT_BYTES: usize = 50 * 1024;

/// One closed operation accepted by [`ConfigTool`].
#[derive(Clone, PartialEq)]
pub enum ConfigArgs {
    /// List registered sections and whether each has a stored value.
    List,
    /// Read one complete non-secret section.
    Get { section: String },
    /// Read the JSON Schema and description for one section.
    Schema { section: String },
    /// Validate and atomically replace one complete section.
    Set {
        section: String,
        value: Value,
        expected_revision: String,
    },
    /// Remove one complete stored section.
    Remove {
        section: String,
        expected_revision: String,
    },
}

impl fmt::Debug for ConfigArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List => formatter.write_str("List"),
            Self::Get { section } => formatter
                .debug_struct("Get")
                .field("section", section)
                .finish(),
            Self::Schema { section } => formatter
                .debug_struct("Schema")
                .field("section", section)
                .finish(),
            Self::Set {
                section,
                expected_revision,
                ..
            } => formatter
                .debug_struct("Set")
                .field("section", section)
                .field("value", &"<redacted>")
                .field("expected_revision", expected_revision)
                .finish(),
            Self::Remove {
                section,
                expected_revision,
            } => formatter
                .debug_struct("Remove")
                .field("section", section)
                .field("expected_revision", expected_revision)
                .finish(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputEnvelope {
    request: ConfigRequest,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigRequest {
    List,
    Get {
        section: String,
    },
    Schema {
        section: String,
    },
    Set {
        section: String,
        value: Value,
        expected_revision: String,
    },
    Remove {
        section: String,
        expected_revision: String,
    },
}

impl<'de> Deserialize<'de> for ConfigArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let InputEnvelope { request } = InputEnvelope::deserialize(deserializer)?;
        match request {
            ConfigRequest::List => Ok(Self::List),
            ConfigRequest::Get { section } => {
                validate_section_input::<D::Error>(&section)?;
                Ok(Self::Get { section })
            }
            ConfigRequest::Schema { section } => {
                validate_section_input::<D::Error>(&section)?;
                Ok(Self::Schema { section })
            }
            ConfigRequest::Set {
                section,
                value,
                expected_revision,
            } => {
                validate_section_input::<D::Error>(&section)?;
                Ok(Self::Set {
                    section,
                    value,
                    expected_revision,
                })
            }
            ConfigRequest::Remove {
                section,
                expected_revision,
            } => {
                validate_section_input::<D::Error>(&section)?;
                Ok(Self::Remove {
                    section,
                    expected_revision,
                })
            }
        }
    }
}

fn validate_section_input<E>(section: &str) -> Result<(), E>
where
    E: serde::de::Error,
{
    let valid = !section.is_empty()
        && section.len() <= 128
        && section.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        });
    if valid {
        Ok(())
    } else {
        Err(E::custom("section must be a valid registered section name"))
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConfigListEntry {
    pub section: String,
    pub description: String,
    pub configured: bool,
}

/// Structured result of one configuration operation.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ConfigOutput {
    List {
        revision: String,
        sections: Vec<ConfigListEntry>,
    },
    Get {
        revision: String,
        section: String,
        configured: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
    },
    Schema {
        section: String,
        description: String,
        schema: Value,
    },
    Set {
        previous_revision: String,
        revision: String,
        section: String,
        changed: bool,
    },
    Remove {
        previous_revision: String,
        revision: String,
        section: String,
        removed: bool,
    },
}

#[derive(Debug, Error)]
pub enum ConfigToolError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("configuration tool output exceeds the {maximum_bytes}-byte limit")]
    OutputTooLarge { maximum_bytes: usize },
    #[error("configuration tool output limit must be greater than zero")]
    InvalidOutputLimit,
    #[error("configuration task failed")]
    Task,
}

/// Inspect and atomically update registered non-secret configuration sections.
///
/// The tool cannot read or write credentials. Persistent writes are ordinary
/// tool side effects: the future runtime must apply authorization and approval
/// before dispatch, just as it does for patches and shell commands. Once
/// dispatched to blocking storage, cancellation does not roll back a commit;
/// callers should read the latest revision after an uncertain outcome.
#[derive(Clone)]
pub struct ConfigTool {
    manager: Arc<ConfigManager>,
    max_output_bytes: usize,
}

impl ConfigTool {
    pub fn new(manager: Arc<ConfigManager>) -> Self {
        Self {
            manager,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// Construct a tool with a host-selected model-output ceiling.
    pub fn with_output_limit(
        manager: Arc<ConfigManager>,
        max_output_bytes: usize,
    ) -> Result<Self, ConfigToolError> {
        if max_output_bytes == 0 {
            return Err(ConfigToolError::InvalidOutputLimit);
        }
        Ok(Self {
            manager,
            max_output_bytes,
        })
    }
}

impl fmt::Debug for ConfigTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigTool")
            .field("registered_sections", &self.manager.sections().len())
            .field("max_output_bytes", &self.max_output_bytes)
            .finish()
    }
}

impl Tool for ConfigTool {
    type Args = ConfigArgs;
    type Output = ConfigOutput;
    type Error = ConfigToolError;

    fn definition(&self) -> ToolDefinition {
        let section = || {
            json!({
                "type": "string",
                "minLength": 1,
                "maxLength": 128,
                "pattern": "^[a-z0-9_-]+(?:\\.[a-z0-9_-]+)*$",
                "description": "Exact section name returned by list, such as tools.forge"
            })
        };
        let revision = || {
            json!({
                "type": "string",
                "pattern": "^[0-9a-f]{64}$",
                "description": "Latest opaque revision returned by list or get"
            })
        };
        ToolDefinition::new(
            "config",
            "Inspect schemas and complete values for registered non-secret BONE configuration sections. Use list to discover sections, get to obtain the current value and revision, and schema before set. Set replaces one complete section and remove deletes it; both require the latest revision. Credentials and secret values are never available through this tool.",
            json!({
                "type": "object",
                "properties": {
                    "request": {
                        "description": "Exactly one configuration operation",
                        "oneOf": [
                            {
                                "type": "object",
                                "properties": {
                                    "action": { "const": "list" }
                                },
                                "required": ["action"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "action": { "const": "get" },
                                    "section": section()
                                },
                                "required": ["action", "section"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "action": { "const": "schema" },
                                    "section": section()
                                },
                                "required": ["action", "section"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "action": { "const": "set" },
                                    "section": section(),
                                    "value": {
                                        "description": "Complete replacement value; it must match the registered section schema"
                                    },
                                    "expected_revision": revision()
                                },
                                "required": ["action", "section", "value", "expected_revision"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "action": { "const": "remove" },
                                    "section": section(),
                                    "expected_revision": revision()
                                },
                                "required": ["action", "section", "expected_revision"],
                                "additionalProperties": false
                            }
                        ]
                    }
                },
                "required": ["request"],
                "additionalProperties": false
            }),
        )
    }

    fn map_error(&self, error: Self::Error) -> ToolFailure {
        match error {
            ConfigToolError::Config(ConfigError::InvalidRevision) => {
                let message = "expected_revision is not a valid configuration revision";
                ToolFailure::new(
                    ToolFailureKind::InvalidArguments,
                    message,
                    ToolOutput::text(message),
                )
            }
            ConfigToolError::Config(ConfigError::InvalidSection { .. }) => {
                let message = "configuration section value failed validation";
                ToolFailure::new(
                    ToolFailureKind::InvalidArguments,
                    message,
                    ToolOutput::text(message),
                )
            }
            ConfigToolError::Config(ConfigError::UnknownSection(section)) => {
                let message = format!("configuration section is not registered: {section}");
                ToolFailure::new(
                    ToolFailureKind::NotFound,
                    message.clone(),
                    ToolOutput::text(message),
                )
            }
            ConfigToolError::Config(error @ ConfigError::RevisionConflict) => {
                ToolFailure::new(
                    ToolFailureKind::Other,
                    "configuration revision conflict",
                    ToolOutput::text(
                        "Configuration changed. Call config get or list again, then retry with the returned revision.",
                    ),
                )
                    .with_source(error)
            }
            ConfigToolError::Config(error @ ConfigError::Busy) => {
                ToolFailure::new(
                    ToolFailureKind::Other,
                    "configuration storage is busy",
                    ToolOutput::text("Retry the same operation after a short delay."),
                )
                    .with_source(error)
            }
            ConfigToolError::Config(error @ ConfigError::DocumentTooLarge { .. }) => {
                ToolFailure::new(
                    ToolFailureKind::Other,
                    "configuration exceeds its storage limit",
                    ToolOutput::text(
                        "The configuration is too large. Use a smaller complete section value.",
                    ),
                )
                    .with_source(error)
            }
            ConfigToolError::Config(error @ ConfigError::InvalidDocument) => {
                ToolFailure::new(
                    ToolFailureKind::Other,
                    "configuration storage contains invalid JSON",
                    ToolOutput::text("The configuration file must be repaired by the user."),
                )
                    .with_source(error)
            }
            ConfigToolError::Config(error) => ToolFailure::new(
                ToolFailureKind::Other,
                "configuration storage failed",
                ToolOutput::text("configuration storage failed"),
            )
            .with_source(error),
            ConfigToolError::OutputTooLarge { maximum_bytes } => {
                ToolFailure::new(
                    ToolFailureKind::Other,
                    "configuration tool output is too large",
                    ToolOutput::text(format!(
                        "The requested configuration output exceeds the {maximum_bytes}-byte tool limit. Ask the user to inspect it through a human-facing client."
                    )),
                )
            }
            ConfigToolError::InvalidOutputLimit => {
                let message = "configuration tool output limit must be greater than zero";
                ToolFailure::new(
                    ToolFailureKind::InvalidArguments,
                    message,
                    ToolOutput::text(message),
                )
            }
            ConfigToolError::Task => ToolFailure::new(
                ToolFailureKind::Other,
                "configuration task failed",
                ToolOutput::text("configuration storage task failed"),
            ),
        }
    }

    async fn call(&self, arguments: Self::Args) -> Result<Self::Output, Self::Error> {
        let manager = Arc::clone(&self.manager);
        let max_output_bytes = self.max_output_bytes;
        tokio::task::spawn_blocking(move || execute(&manager, arguments, max_output_bytes))
            .await
            .map_err(|_| ConfigToolError::Task)?
    }
}

fn execute(
    manager: &ConfigManager,
    arguments: ConfigArgs,
    max_output_bytes: usize,
) -> Result<ConfigOutput, ConfigToolError> {
    let output = match arguments {
        ConfigArgs::List => {
            let snapshot = manager.snapshot()?;
            let sections = manager
                .sections()
                .into_iter()
                .map(|info| ConfigListEntry {
                    configured: snapshot.contains(&info.key),
                    section: info.key,
                    description: info.description,
                })
                .collect();
            ConfigOutput::List {
                revision: snapshot.revision().to_string(),
                sections,
            }
        }
        ConfigArgs::Get { section } => {
            if manager.section(&section).is_none() {
                return Err(ConfigError::UnknownSection(section).into());
            }
            let snapshot = manager.snapshot()?;
            ConfigOutput::Get {
                revision: snapshot.revision().to_string(),
                configured: snapshot.contains(&section),
                value: snapshot.value(&section).cloned(),
                section,
            }
        }
        ConfigArgs::Schema { section } => {
            let info = manager
                .section(&section)
                .ok_or_else(|| ConfigError::UnknownSection(section.clone()))?;
            ConfigOutput::Schema {
                section,
                description: info.description,
                schema: info.schema,
            }
        }
        ConfigArgs::Set {
            section,
            value,
            expected_revision,
        } => {
            let expected = ConfigRevision::from_str(&expected_revision)?;
            let change = manager.set_value(&section, value, &expected)?;
            ConfigOutput::Set {
                previous_revision: expected_revision,
                revision: change.revision.to_string(),
                section,
                changed: change.changed,
            }
        }
        ConfigArgs::Remove {
            section,
            expected_revision,
        } => {
            let expected = ConfigRevision::from_str(&expected_revision)?;
            let change = manager.remove_value(&section, &expected)?;
            ConfigOutput::Remove {
                previous_revision: expected_revision,
                revision: change.revision.to_string(),
                section,
                removed: change.changed,
            }
        }
    };
    let output_bytes = serde_json::to_vec(&output)
        .map_err(|_| ConfigToolError::Task)?
        .len();
    if output_bytes > max_output_bytes {
        return Err(ConfigToolError::OutputTooLarge {
            maximum_bytes: max_output_bytes,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use bone_agent::Tool;
    use bone_config::ConfigSection;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct ExampleConfig {
        enabled: bool,
    }

    impl ConfigSection for ExampleConfig {
        const KEY: &'static str = "tools.example";

        fn description() -> &'static str {
            "Example tool settings"
        }

        fn schema() -> Value {
            json!({
                "type": "object",
                "properties": { "enabled": { "type": "boolean" } },
                "required": ["enabled"],
                "additionalProperties": false
            })
        }
    }

    fn tool() -> (tempfile::TempDir, ConfigTool) {
        let directory = tempfile::tempdir().unwrap();
        let manager = ConfigManager::builder()
            .register::<ExampleConfig>()
            .unwrap()
            .build(directory.path().join("config.json"))
            .unwrap();
        (directory, ConfigTool::new(Arc::new(manager)))
    }

    #[tokio::test]
    async fn lists_gets_sets_and_removes_complete_sections() {
        let (_directory, tool) = tool();
        let listed = tool.call(ConfigArgs::List).await.unwrap();
        let ConfigOutput::List { revision, sections } = listed else {
            panic!("expected list output");
        };
        assert_eq!(sections.len(), 1);
        assert!(!sections[0].configured);

        let set = tool
            .call(ConfigArgs::Set {
                section: ExampleConfig::KEY.to_owned(),
                value: json!({"enabled": true}),
                expected_revision: revision,
            })
            .await
            .unwrap();
        let ConfigOutput::Set {
            revision, changed, ..
        } = set
        else {
            panic!("expected set output");
        };
        assert!(changed);

        let get = tool
            .call(ConfigArgs::Get {
                section: ExampleConfig::KEY.to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(
            get,
            ConfigOutput::Get {
                configured: true,
                value: Some(value),
                ..
            } if value == json!({"enabled": true})
        ));

        let removed = tool
            .call(ConfigArgs::Remove {
                section: ExampleConfig::KEY.to_owned(),
                expected_revision: revision,
            })
            .await
            .unwrap();
        assert!(matches!(
            removed,
            ConfigOutput::Remove { removed: true, .. }
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_values_without_writing() {
        let (_directory, tool) = tool();
        let ConfigOutput::List { revision, .. } = tool.call(ConfigArgs::List).await.unwrap() else {
            panic!("expected list output");
        };
        let error = tool
            .call(ConfigArgs::Set {
                section: ExampleConfig::KEY.to_owned(),
                value: json!({"enabled": true, "typo": false}),
                expected_revision: revision.clone(),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ConfigToolError::Config(ConfigError::InvalidSection { .. })
        ));
        let ConfigOutput::List {
            revision: after, ..
        } = tool.call(ConfigArgs::List).await.unwrap()
        else {
            panic!("expected list output");
        };
        assert_eq!(after, revision);
    }

    #[tokio::test]
    async fn validation_failures_do_not_echo_values() {
        let (_directory, tool) = tool();
        let ConfigOutput::List { revision, .. } = tool.call(ConfigArgs::List).await.unwrap() else {
            panic!("expected list output");
        };
        let error = tool
            .call(ConfigArgs::Set {
                section: ExampleConfig::KEY.to_owned(),
                value: json!({"enabled": "never-print-this"}),
                expected_revision: revision,
            })
            .await
            .unwrap_err();
        assert!(!format!("{error:?}").contains("never-print-this"));
        assert!(!format!("{:?}", tool.map_error(error)).contains("never-print-this"));
    }

    #[tokio::test]
    async fn detects_revision_conflicts() {
        let (_directory, tool) = tool();
        let ConfigOutput::List { revision, .. } = tool.call(ConfigArgs::List).await.unwrap() else {
            panic!("expected list output");
        };
        let ConfigOutput::Set {
            revision: current, ..
        } = tool
            .call(ConfigArgs::Set {
                section: ExampleConfig::KEY.to_owned(),
                value: json!({"enabled": true}),
                expected_revision: revision.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("expected set output");
        };
        assert_ne!(current, revision);

        let error = tool
            .call(ConfigArgs::Remove {
                section: ExampleConfig::KEY.to_owned(),
                expected_revision: revision,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ConfigToolError::Config(ConfigError::RevisionConflict)
        ));
        let mapped = tool.map_error(error);
        assert_eq!(mapped.kind(), ToolFailureKind::Other);
        assert_eq!(
            mapped.model_output(),
            &ToolOutput::text(
                "Configuration changed. Call config get or list again, then retry with the returned revision."
            )
        );
        assert!(std::error::Error::source(&mapped).is_some());
    }

    #[test]
    fn arguments_and_schema_are_closed_and_action_specific() {
        for invalid in [
            json!({"action":"list"}),
            json!({"request":{"action":"list"}, "extra": true}),
            json!({"request":{"action":"get", "section":"tools.example", "extra": true}}),
            json!({"request":{"action":"schema", "section":"tools.example", "extra": true}}),
            json!({"request":{"action":"set", "section":"tools.example", "value": {}, "expected_revision":"0".repeat(64), "extra": true}}),
            json!({"request":{"action":"remove", "section":"tools.example", "expected_revision":"0".repeat(64), "extra": true}}),
            json!({"request":{"action":"get", "section":"UPPERCASE"}}),
        ] {
            assert!(serde_json::from_value::<ConfigArgs>(invalid).is_err());
        }
        let revision = "0".repeat(64);
        for (input, expected) in [
            (json!({"request":{"action":"list"}}), ConfigArgs::List),
            (
                json!({"request":{"action":"get", "section":"tools.example"}}),
                ConfigArgs::Get {
                    section: "tools.example".to_owned(),
                },
            ),
            (
                json!({"request":{"action":"schema", "section":"tools.example"}}),
                ConfigArgs::Schema {
                    section: "tools.example".to_owned(),
                },
            ),
            (
                json!({"request":{"action":"set", "section":"tools.example", "value":{"enabled":true}, "expected_revision":revision}}),
                ConfigArgs::Set {
                    section: "tools.example".to_owned(),
                    value: json!({"enabled": true}),
                    expected_revision: revision.clone(),
                },
            ),
            (
                json!({"request":{"action":"remove", "section":"tools.example", "expected_revision":revision}}),
                ConfigArgs::Remove {
                    section: "tools.example".to_owned(),
                    expected_revision: revision,
                },
            ),
        ] {
            assert_eq!(
                serde_json::from_value::<ConfigArgs>(input).unwrap(),
                expected
            );
        }

        let (_directory, tool) = tool();
        let definition = tool.definition();
        let schema = definition.parameters();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        let alternatives = schema["properties"]["request"]["oneOf"].as_array().unwrap();
        assert_eq!(alternatives.len(), 5);
        assert!(
            alternatives
                .iter()
                .all(|alternative| alternative["additionalProperties"] == false)
        );
    }

    #[test]
    fn debug_does_not_include_configuration_values() {
        let (_directory, tool) = tool();
        let debug = format!("{tool:?}");
        assert!(debug.contains("registered_sections"));
        assert!(!debug.contains("config.json"));

        let args = ConfigArgs::Set {
            section: "tools.example".to_owned(),
            value: json!({"token": "never-print-this"}),
            expected_revision: "0".repeat(64),
        };
        let debug = format!("{args:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains("never-print-this"));
    }

    #[tokio::test]
    async fn bounds_model_visible_output() {
        let (directory, tool) = tool();
        let limited = ConfigTool::with_output_limit(Arc::clone(&tool.manager), 16).unwrap();
        let error = limited.call(ConfigArgs::List).await.unwrap_err();
        assert!(matches!(error, ConfigToolError::OutputTooLarge { .. }));
        assert_eq!(limited.map_error(error).kind(), ToolFailureKind::Other);
        drop(directory);
    }
}
