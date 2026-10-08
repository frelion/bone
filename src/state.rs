use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Ready,
    Running,
    Waiting,
    Idle,
    Paused,
    Closed,
}

/// References in inbox, active_input, history, and wait_for are event IDs.
/// Native model messages live exclusively in Event.data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub state: JobState,
    pub inbox: VecDeque<String>,
    pub active_input: Option<String>,
    pub history: Vec<String>,
    /// Event ID of the latest native summary completion, never copied text.
    pub summary: Option<String>,
    pub wait_for: Vec<String>,
    pub current_call: Option<String>,
    /// Last public instruction incorporated into this job's history or summary.
    pub public_revision: u64,
}

impl Job {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            title: title.into(),
            state: JobState::Idle,
            inbox: VecDeque::new(),
            active_input: None,
            history: Vec::new(),
            summary: None,
            wait_for: Vec::new(),
            current_call: None,
            public_revision: 0,
        }
    }
}

/// One allowance shared by all work caused by a root user input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub calls_used: u32,
    pub max_calls: u32,
    pub jobs_used: u32,
    pub max_jobs: u32,
}

impl Budget {
    pub fn new(max_calls: u32, max_jobs: u32) -> Self {
        Self {
            calls_used: 0,
            max_calls,
            jobs_used: 0,
            max_jobs,
        }
    }

    pub fn reserve_call(&mut self) -> Result<()> {
        ensure!(
            self.calls_used < self.max_calls,
            "input call budget exhausted"
        );
        self.calls_used += 1;
        Ok(())
    }

    pub fn reserve_job(&mut self) -> Result<()> {
        ensure!(self.jobs_used < self.max_jobs, "input job budget exhausted");
        self.jobs_used += 1;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionState {
    pub id: String,
    pub workspace: PathBuf,
    pub focus: Option<String>,
    pub revision: u64,
    pub pending_inputs: VecDeque<String>,
    pub jobs: BTreeMap<String, Job>,
    pub budgets: BTreeMap<String, Budget>,
    pub paused: bool,
}

impl SessionState {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            id: new_id(),
            workspace: workspace.into(),
            focus: None,
            revision: 0,
            pending_inputs: VecDeque::new(),
            jobs: BTreeMap::new(),
            budgets: BTreeMap::new(),
            paused: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub session_id: String,
    pub job_id: Option<String>,
    pub call_id: Option<String>,
    pub reply_to: Option<String>,
    pub root_input: Option<String>,
    pub kind: String,
    pub revision: u64,
    pub data: Value,
    /// Unix milliseconds, represented as a string for lossless JSON storage.
    pub timestamp: String,
}

impl Event {
    pub fn new(session_id: impl Into<String>, kind: impl Into<String>, data: Value) -> Self {
        Self {
            id: new_id(),
            session_id: session_id.into(),
            job_id: None,
            call_id: None,
            reply_to: None,
            root_input: None,
            kind: kind.into(),
            revision: 0,
            data,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .to_string(),
        }
    }

    /// Only scheduling metadata remains resident after a transcript is archived.
    pub(crate) fn metadata(&self) -> Self {
        let mut event = self.clone();
        if let Some(data) = event.data.as_object_mut() {
            if let Some(usage) = data.get("response").and_then(|r| r.get("usage")).cloned() {
                data.insert("usage".into(), usage);
            }
            for field in [
                "message",
                "response",
                "stream_items",
                "covered_ids",
                "observations",
            ] {
                data.remove(field);
            }
        }
        event
    }
}

#[cfg(test)]
#[path = "../tests/unit/state.rs"]
mod tests;
