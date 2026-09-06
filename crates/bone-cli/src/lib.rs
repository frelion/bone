//! Adapters that connect the event-driven agent to real models and native tools.

mod config;
mod events;
mod model;
mod review;
mod tools;

pub use config::{Effort, ModelSettings, SystemConfig, TaskConfig};
pub use events::write_events;
pub use model::ModelAdapter;
pub use tools::read_only_tools;
