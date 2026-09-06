use bone_config::ConfigSection;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Presentation settings owned by the terminal frontend.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TuiConfig {
    /// Show model/tool starts, finishes, and intermediate progress.
    pub show_progress: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            show_progress: true,
        }
    }
}

impl ConfigSection for TuiConfig {
    const KEY: &'static str = "tui.display";

    fn description() -> &'static str {
        "Terminal display preferences. Read when the frontend starts."
    }

    fn schema() -> Value {
        schema_for!(Self).to_value()
    }
}
