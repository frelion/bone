//! Validated, immutable-at-use limits for BONE's local tools.
//!
//! This module deliberately contains no storage, section registration, or
//! schema policy. A product settings layer may serialize this value however it
//! chooses, then must validate it before constructing a [`ToolEnvironment`].

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Hard limits shared by the built-in tools.
///
/// Persisted values use integer seconds for Bash deadlines. The direct Rust
/// API intentionally continues to support sub-second `Duration` values; such
/// values are valid for an in-process environment but cannot be serialized by
/// this representation without an explicit product-level policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolLimits {
    /// Maximum retained UTF-8 bytes in each tool-defined textual output budget.
    pub max_output_bytes: usize,
    /// Maximum lines returned by one `read` call.
    pub max_read_lines: usize,
    /// Maximum size of one file read or scanned by `read`.
    pub max_read_file_bytes: u64,
    /// Maximum paths returned by one `glob` call.
    pub max_glob_results: usize,
    /// Maximum matches returned by one `grep` call.
    pub max_grep_matches: usize,
    /// Maximum UTF-8 bytes accepted in one grep pattern.
    pub max_grep_pattern_bytes: usize,
    /// Maximum characters retained from one grep line.
    pub max_grep_line_chars: usize,
    /// Maximum size of one file inspected by filesystem search tools.
    pub max_search_file_bytes: u64,
    /// Maximum combined file bytes inspected by one grep call.
    pub max_search_total_bytes: u64,
    /// Maximum size of one workspace-local ignore file loaded during search.
    pub max_ignore_file_bytes: u64,
    /// Maximum combined ignore-file bytes loaded during one search call.
    pub max_ignore_total_bytes: u64,
    /// Maximum filesystem entries inspected by one search call.
    pub max_walk_entries: usize,
    /// Maximum UTF-8 bytes accepted in one patch document.
    pub max_patch_bytes: usize,
    /// Maximum file operations accepted in one patch document.
    pub max_patch_files: usize,
    /// Maximum size of one existing file read while planning a patch.
    pub max_patch_file_bytes: u64,
    /// Maximum combined bytes retained from existing files while planning a patch.
    pub max_patch_total_bytes: u64,
    /// Maximum UTF-8 bytes accepted in one Bash command.
    pub max_bash_command_bytes: usize,
    /// Default shell deadline.
    #[serde(rename = "default_bash_timeout_seconds", with = "duration_seconds")]
    pub default_bash_timeout: Duration,
    /// Largest shell deadline.
    #[serde(rename = "max_bash_timeout_seconds", with = "duration_seconds")]
    pub max_bash_timeout: Duration,
}

/// A semantic validation failure in [`ToolLimits`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ToolLimitsError {
    #[error("{field} must be greater than zero")]
    NonPositive { field: &'static str },
    #[error("max_bash_timeout must be at least default_bash_timeout")]
    BashTimeoutOrder,
    #[error(
        "max_bash_timeout must be at least one second because Bash arguments use whole seconds"
    )]
    BashTimeoutTooShort,
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
    /// Validate limits before binding them into a [`crate::ToolEnvironment`].
    pub fn validate(&self) -> Result<(), ToolLimitsError> {
        for (field, value) in [
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
        ] {
            if value == 0 {
                return Err(ToolLimitsError::NonPositive { field });
            }
        }
        for (field, value) in [
            ("max_read_file_bytes", self.max_read_file_bytes),
            ("max_search_file_bytes", self.max_search_file_bytes),
            ("max_search_total_bytes", self.max_search_total_bytes),
            ("max_ignore_file_bytes", self.max_ignore_file_bytes),
            ("max_ignore_total_bytes", self.max_ignore_total_bytes),
            ("max_patch_file_bytes", self.max_patch_file_bytes),
            ("max_patch_total_bytes", self.max_patch_total_bytes),
        ] {
            if value == 0 {
                return Err(ToolLimitsError::NonPositive { field });
            }
        }
        if self.default_bash_timeout.is_zero() {
            return Err(ToolLimitsError::NonPositive {
                field: "default_bash_timeout",
            });
        }
        if self.max_bash_timeout < self.default_bash_timeout {
            return Err(ToolLimitsError::BashTimeoutOrder);
        }
        if self.max_bash_timeout < Duration::from_secs(1) {
            return Err(ToolLimitsError::BashTimeoutTooShort);
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
        assert_eq!(
            limits.validate(),
            Err(ToolLimitsError::NonPositive {
                field: "max_ignore_file_bytes"
            })
        );

        limits.max_ignore_file_bytes = 1;
        limits.max_ignore_total_bytes = 0;
        assert_eq!(
            limits.validate(),
            Err(ToolLimitsError::NonPositive {
                field: "max_ignore_total_bytes"
            })
        );
    }

    #[test]
    fn direct_runtime_limits_can_use_subsecond_deadlines() {
        let limits = ToolLimits {
            default_bash_timeout: Duration::from_millis(500),
            max_bash_timeout: Duration::from_secs(1),
            ..ToolLimits::default()
        };
        assert!(limits.validate().is_ok());
        assert!(serde_json::to_value(limits).is_err());
    }
}
