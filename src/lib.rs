//! A single conversational agent whose thinking and tools run inside persistent jobs.
//!
//! Execution goes through [`runtime::Engine`]. Its snapshots are read-only;
//! implementation modules cannot be used to bypass job ownership or persistence.
//!
//! ```compile_fail
//! use bone::tools::execute;
//! ```
//!
//! ```compile_fail
//! use bone::model::prepare;
//! ```
//!
//! ```compile_fail
//! fn overwrite(engine: &mut bone::runtime::Engine) {
//!     engine.state().jobs.clear();
//! }
//! ```
pub mod config;
mod context;
mod model;
pub mod runtime;
pub mod state;
mod store;
mod tools;

pub use model::{login, providers};

use std::path::Path;

/// Inspect saved sessions without taking execution ownership.
pub fn sessions(data_dir: &Path) -> anyhow::Result<Vec<state::SessionState>> {
    store::Store::open(data_dir.join("sessions.sqlite3"))?.list_sessions()
}

/// Read one persisted snapshot. Changing this detached value cannot change a session.
pub fn session(data_dir: &Path, id: &str) -> anyhow::Result<state::SessionState> {
    store::Store::open(data_dir.join("sessions.sqlite3"))?.load_session(id)
}

/// Read original event records, including material summarized for model context.
pub fn history(data_dir: &Path, id: &str) -> anyhow::Result<Vec<state::Event>> {
    let store = store::Store::open(data_dir.join("sessions.sqlite3"))?;
    store.load_session(id)?;
    store.events(id)
}

#[cfg(test)]
#[path = "../tests/internal/provider_contract.rs"]
mod provider_contract;

#[cfg(test)]
#[path = "../tests/internal/runtime_safety.rs"]
mod runtime_safety;
