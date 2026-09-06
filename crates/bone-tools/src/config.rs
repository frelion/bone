use std::time::Duration;

use bone_config::ConfigSection;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ToolError;

/// Hard limits shared by the built-in tools.
///
/// The `tools.local` configuration section uses these defaults for omitted
/// fields. Bash deadlines are persisted as integer `*_seconds` fields; the
/// direct Rust API continues to accept subsecond durations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolLimits {
    /// Maximum retained UTF-8 bytes in each tool-defined textual output budget.
    #[schemars(range(min = 1))]
    pub max_output_bytes: usize,
    /// Maximum lines returned by one `read` call.
    #[schemars(range(min = 1))]
    pub max_read_lines: usize,
    /// Maximum size of one file read or scanned by `read`.
    #[schemars(range(min = 1))]
    pub max_read_file_bytes: u64,
    /// Maximum paths returned by one `glob` call.
    #[schemars(range(min = 1))]
    pub max_glob_results: usize,
    /// Maximum matches returned by one `grep` call.
    #[schemars(range(min = 1))]
    pub max_grep_matches: usize,
    /// Maximum UTF-8 bytes accepted in one grep pattern.
    #[schemars(range(min = 1))]
    pub max_grep_pattern_bytes: usize,
    /// Maximum characters retained from one grep line.
    #[schemars(range(min = 1))]
    pub max_grep_line_chars: usize,
    /// Maximum size of one file inspected by filesystem search tools.
    #[schemars(range(min = 1))]
    pub max_search_file_bytes: u64,
    /// Maximum combined file bytes inspected by one grep call.
    #[schemars(range(min = 1))]
    pub max_search_total_bytes: u64,
    /// Maximum size of one workspace-local ignore file loaded during search.
    #[schemars(range(min = 1))]
    pub max_ignore_file_bytes: u64,
    /// Maximum combined ignore-file bytes loaded during one search call.
    #[schemars(range(min = 1))]
    pub max_ignore_total_bytes: u64,
    /// Maximum filesystem entries inspected by one search call.
    #[schemars(range(min = 1))]
    pub max_walk_entries: usize,
    /// Maximum UTF-8 bytes accepted in one patch document.
    #[schemars(range(min = 1))]
    pub max_patch_bytes: usize,
    /// Maximum file operations accepted in one patch document.
    #[schemars(range(min = 1))]
    pub max_patch_files: usize,
    /// Maximum size of one existing file read while planning a patch.
    #[schemars(range(min = 1))]
    pub max_patch_file_bytes: u64,
    /// Maximum combined bytes retained from existing files while planning a patch.
    #[schemars(range(min = 1))]
    pub max_patch_total_bytes: u64,
    /// Maximum UTF-8 bytes accepted in one Bash command.
    #[schemars(range(min = 1))]
    pub max_bash_command_bytes: usize,
    /// Default shell deadline; configuration stores a positive whole-second count.
    #[serde(rename = "default_bash_timeout_seconds", with = "duration_seconds")]
    #[schemars(with = "u64", range(min = 1))]
    pub default_bash_timeout: Duration,
    /// Largest shell deadline; must be at least the default deadline.
    #[serde(rename = "max_bash_timeout_seconds", with = "duration_seconds")]
    #[schemars(with = "u64", range(min = 1))]
    pub max_bash_timeout: Duration,
}

impl ConfigSection for ToolLimits {
    const KEY: &'static str = "tools.local";

    fn description() -> &'static str {
        "Local tool limits. Loaded when a session starts."
    }

    fn schema() -> Value {
        schema_for!(Self).to_value()
    }

    fn validate(&self) -> Result<(), String> {
        ToolLimits::validate(self).map_err(|error| error.to_string())?;
        if self.default_bash_timeout.subsec_nanos() != 0
            || self.max_bash_timeout.subsec_nanos() != 0
        {
            return Err("configured Bash timeouts must use whole seconds".to_owned());
        }
        Ok(())
    }
}

mod duration_seconds {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer, ser::Error};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if duration.subsec_nanos() != 0 {
            return Err(S::Error::custom(
                "configured Bash timeouts must use whole seconds",
            ));
        }
        serializer.serialize_u64(duration.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        u64::deserialize(deserializer).map(Duration::from_secs)
    }
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: 50 * 1024,
            max_read_lines: 2_000,
            max_read_file_bytes: 10 * 1024 * 1024,
            max_glob_results: 1_000,
            max_grep_matches: 100,
            max_grep_pattern_bytes: 4 * 1024,
            max_grep_line_chars: 500,
            max_search_file_bytes: 10 * 1024 * 1024,
            max_search_total_bytes: 128 * 1024 * 1024,
            max_ignore_file_bytes: 1024 * 1024,
            max_ignore_total_bytes: 4 * 1024 * 1024,
            max_walk_entries: 200_000,
            max_patch_bytes: 1024 * 1024,
            max_patch_files: 100,
            max_patch_file_bytes: 10 * 1024 * 1024,
            max_patch_total_bytes: 64 * 1024 * 1024,
            max_bash_command_bytes: 64 * 1024,
            default_bash_timeout: Duration::from_secs(120),
            max_bash_timeout: Duration::from_secs(600),
        }
    }
}

impl ToolLimits {
    pub(crate) fn validate(&self) -> Result<(), ToolError> {
        let positive = [
            ("max_output_bytes", self.max_output_bytes),
            ("max_read_lines", self.max_read_lines),
            ("max_glob_results", self.max_glob_results),
            ("max_grep_matches", self.max_grep_matches),
            ("max_grep_pattern_bytes", self.max_grep_pattern_bytes),
            ("max_grep_line_chars", self.max_grep_line_chars),
            ("max_walk_entries", self.max_walk_entries),
            ("max_patch_bytes", self.max_patch_bytes),
            ("max_patch_files", self.max_patch_files),
            ("max_bash_command_bytes", self.max_bash_command_bytes),
        ];
        if let Some((name, _)) = positive.into_iter().find(|(_, value)| *value == 0) {
            return Err(ToolError::InvalidArgs(format!(
                "{name} must be greater than zero"
            )));
        }
        if self.max_search_file_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "max_search_file_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_search_total_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "max_search_total_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_ignore_file_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "max_ignore_file_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_ignore_total_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "max_ignore_total_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_read_file_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "max_read_file_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_patch_file_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "max_patch_file_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_patch_total_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "max_patch_total_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.default_bash_timeout.is_zero() {
            return Err(ToolError::InvalidArgs(
                "default_bash_timeout must be greater than zero".to_owned(),
            ));
        }
        if self.max_bash_timeout < self.default_bash_timeout {
            return Err(ToolError::InvalidArgs(
                "max_bash_timeout must be at least default_bash_timeout".to_owned(),
            ));
        }
        if self.max_bash_timeout < Duration::from_secs(1) {
            return Err(ToolError::InvalidArgs(
                "max_bash_timeout must be at least one second because Bash arguments use whole seconds"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_byte_limits_must_be_positive() {
        let mut limits = ToolLimits {
            max_ignore_file_bytes: 0,
            ..ToolLimits::default()
        };
        assert!(matches!(
            limits.validate(),
            Err(ToolError::InvalidArgs(message))
                if message.contains("max_ignore_file_bytes")
        ));

        limits.max_ignore_file_bytes = 1;
        limits.max_ignore_total_bytes = 0;
        assert!(matches!(
            limits.validate(),
            Err(ToolError::InvalidArgs(message))
                if message.contains("max_ignore_total_bytes")
        ));
    }
}
