//! An event-driven agent OS: semantic jobs, one state owner, asynchronous effects.
//!
//! The Kernel model assigns work; full-capability workers solve it. Both return
//! proposals to [`Kernel::step`]. Only the kernel changes authoritative state;
//! [`Runtime`] executes effects and feeds observations back into the same loop.
#![forbid(unsafe_code)]
mod app;
mod config;
mod context;
mod kernel;
mod model;
mod ports;
mod runtime;
mod tools;

pub use app::{AgentHost, AgentModels, ConfiguredModel, ConfiguredModelError, StartError};
pub use config::{ResolvedAgentRuntimeConfig, ResolvedAgentRuntimeConfigError, RuntimeDeadlines};
pub use kernel::{Kernel, KernelConfig, KernelError};
pub use model::ModelAdapter;
pub use ports::*;
pub use runtime::{
    AgentHandle, HandleError, Observation, Runtime, RuntimeConfig, RuntimeError, ShutdownReport,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
pub use tools::read_only_tools;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InputId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JobId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WakeId(pub u64);

/// All input, including host controls and execution observations, enters here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Input(Input),
    RetryInput {
        id: InputId,
    },
    CallFinished {
        id: CallId,
        outcome: CallOutcome,
    },
    CallProgress {
        id: CallId,
        progress: CallProgress,
    },
    /// Host-verified reconciliation, never a model's claim about a write.
    WriteResolved {
        id: CallId,
        outcome: CallOutcome,
    },
    Wake {
        id: WakeId,
    },
    Stop,
}

/// Permission to start is not proof that an operation has happened.
#[derive(Clone, Debug)]
pub enum Effect {
    Start {
        id: CallId,
        call: Call,
        timeout: Option<Duration>,
    },
    RequestCancel {
        id: CallId,
    },
    WakeAfter {
        id: WakeId,
        delay: Duration,
    },
    CancelWake {
        id: WakeId,
    },
    Publish(Notice),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Notice {
    InputHandled {
        inputs: Vec<InputId>,
        required_jobs: Vec<JobId>,
    },
    InputFinished {
        id: InputId,
        outcome: InputOutcome,
    },
    InputRoutingFailed {
        inputs: Vec<InputId>,
        message: String,
    },
    Clarification {
        inputs: Vec<InputId>,
        job: Option<JobId>,
        question: String,
    },
    Reply {
        job: JobId,
        text: String,
        reply_to: Vec<InputId>,
        as_of: u64,
    },
    JobChanged {
        job: JobSnapshot,
    },
    JobFinished {
        id: JobId,
        state: JobState,
    },
    CallStarted {
        id: CallId,
        job: Option<JobId>,
        request: CallRequest,
    },
    CallProgress {
        id: CallId,
        job: Option<JobId>,
        progress: CallProgress,
    },
    CallFinished {
        id: CallId,
        job: Option<JobId>,
        outcome: CallOutcome,
    },
    Error {
        message: String,
    },
    Stopped,
}

/// Receipt of acceptance into this runtime, not durable storage or completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputReceipt {
    pub id: InputId,
    pub record_cursor: u64,
}
