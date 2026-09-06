//! One synchronous session owner, with models and tools executed as ordinary jobs.
//!
//! [`Kernel::step`] records observations and returns [`Effect`]s. [`Runtime`]
//! executes them without waiting in the inbox loop. The solver owns task
//! decisions; a separate, limited model call can interpret interruptions.

#![forbid(unsafe_code)]

mod kernel;
mod ports;
mod runtime;

use serde::{Deserialize, Serialize};
use std::time::Duration;

pub use kernel::{Kernel, KernelConfig, KernelError};
pub use ports::*;
pub use runtime::{
    AgentHandle, HandleError, Observation, Runtime, RuntimeConfig, RuntimeError, ShutdownReport,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JobId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WakeId(pub u64);

/// Observations, never instructions from a running job to start another job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    UserMessage { id: MessageId, text: String },
    JobFinished { id: JobId, outcome: JobOutcome },
    JobProgress { id: JobId, progress: JobProgress },
    Wake { id: WakeId },
    Stop,
}

/// Every call uses the same execution path. A deadline belongs to the invocation.
#[derive(Clone, Debug)]
pub enum Effect {
    Start {
        id: JobId,
        call: Call,
        timeout: Option<Duration>,
    },
    RequestCancel {
        id: JobId,
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
    /// Reply to a fixed batch, based on a particular record position.
    /// Execution acknowledgements come from JobStarted / JobFinished instead.
    Reply {
        text: String,
        reply_to: Vec<MessageId>,
        as_of: u64,
    },
    JobStarted {
        id: JobId,
        request: JobRequest,
    },
    JobProgress {
        id: JobId,
        progress: JobProgress,
    },
    JobFinished {
        id: JobId,
        outcome: JobOutcome,
    },
    Error {
        message: String,
    },
    Paused,
    /// The task was delivered. Remaining read-only calls are being cancelled;
    /// local cleanup is separate from proof that remote work stopped.
    Finished {
        cleanup: Vec<JobId>,
    },
    Stopped,
}

/// Receipt of a message, independent of completion of the work it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageReceipt {
    pub id: MessageId,
    /// Position of this UserMessage entry in the shared record (one-based).
    pub record_cursor: u64,
}
