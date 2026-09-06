//! Terminal presentation and event export for the shared Agent API.

mod config;
mod events;

pub use config::TuiConfig;
pub use events::write_events;
