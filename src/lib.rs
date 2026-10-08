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
use std::path::Path;

pub mod config;
mod context;
#[doc(hidden)]
pub mod filesystem;
mod model;
pub mod runtime;
pub mod state;
mod store;
mod tools;
#[cfg(windows)]
mod windows;

pub use model::{
    has_api_key, has_login, login, login_with, providers, remove_api_key, save_api_key,
};

/// Describe available tools without opening configuration or an execution session.
/// These are the actual native Rig schemas used by the runtime; this metadata
/// function does not expose the private tool executor.
pub fn tool_definitions(
    single_job: bool,
    read_only: bool,
) -> Vec<rig_core::completion::ToolDefinition> {
    tools::definitions(single_job, read_only)
}

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

/// A page of original persisted events in append order.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryPage {
    pub events: Vec<state::Event>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Read at most `limit` events after an event ID. Rust callers supply 1–1000; the CLI default is 100.
pub fn history_page(
    data_dir: &Path,
    id: &str,
    after: Option<&str>,
    limit: usize,
) -> anyhow::Result<HistoryPage> {
    let store = store::Store::open(data_dir.join("sessions.sqlite3"))?;
    store.load_session(id)?;
    let (events, has_more) = store.history_page(id, after, limit)?;
    let next_cursor = events.last().map(|event| event.id.clone());
    Ok(HistoryPage {
        events,
        next_cursor,
        has_more,
    })
}

/// Read the last `limit` records before a cursor, returned in append order.
/// A missing cursor selects the end of the log; `next_cursor` points toward older records.
pub fn history_before(
    data_dir: &Path,
    id: &str,
    before: Option<&str>,
    limit: usize,
) -> anyhow::Result<HistoryPage> {
    let store = store::Store::open(data_dir.join("sessions.sqlite3"))?;
    store.load_session(id)?;
    let (events, has_more) = store.history_before(id, before, limit)?;
    let next_cursor = events.first().map(|event| event.id.clone());
    Ok(HistoryPage {
        events,
        next_cursor,
        has_more,
    })
}

/// A readable match in an original persisted event, independent of screen wrapping.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryMatch {
    pub event_id: String,
    pub kind: String,
    pub snippet: String,
}

/// Search complete persisted readable text using at most one original event at a time.
/// Matching is case-insensitive; `limit` is 1–100. Opaque provider blocks are excluded.
pub fn history_search(
    data_dir: &Path,
    id: &str,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<HistoryMatch>> {
    let store = store::Store::open(data_dir.join("sessions.sqlite3"))?;
    store.load_session(id)?;
    store.history_search(id, query, limit)
}

#[cfg(test)]
#[path = "../tests/internal/provider_contract.rs"]
mod provider_contract;

#[cfg(test)]
#[path = "../tests/internal/runtime_safety.rs"]
mod runtime_safety;
