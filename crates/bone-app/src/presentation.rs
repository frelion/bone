//! Human-facing projections; never persisted or sent back as model output.
use serde_json::Value;

use crate::{ToolDetails, ToolOutcome, ToolSummary};

pub fn tool_summary(name: &str, arguments: &Value, outcome: Option<&ToolOutcome>) -> ToolSummary {
    if name == "session_history" && !outcome.is_some_and(|outcome| outcome.result.is_err()) {
        return crate::tools::history_summary(arguments, outcome);
    }
    bone_adapters::tools::presentation::summary(name, arguments, outcome)
}

pub fn tool_details(name: &str, arguments: &Value, outcome: &ToolOutcome) -> ToolDetails {
    if name == "session_history"
        && let Some(details) = crate::tools::history_details(outcome)
    {
        return details;
    }
    bone_adapters::tools::presentation::details(name, arguments, outcome)
}
