//! A real-time agent kernel built around owned jobs and scoped context.
//!
//! [`Agent`] is the host API. A single runtime executes model and tool
//! calls while a plain Rust kernel owns every state transition.
#![forbid(unsafe_code)]

mod app;
mod config;
mod context;
mod job;
mod kernel;
mod model;
mod ports;
mod runtime;
mod tools;

#[cfg(test)]
mod tests;

pub use app::{ConfiguredModel, ConfiguredModelError, StartError};
pub use config::{AgentLimits, AgentLimitsError};
pub use context::{
    Checkpoint, CheckpointDraft, CompactInput, CoordinateInput, DeliveryKind, DeliveryTarget,
    InquiryResult, JobCard, Origin, Record, RecordBody, RecordRange, RecordView, WorkInput,
};
pub use job::*;
pub(crate) use model::ModelAdapter;
pub use ports::*;
pub use runtime::{Agent, AgentError, Observation, RuntimeError, ShutdownReport, UnresolvedWrite};
pub(crate) use tools::read_only_tools;

use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};

macro_rules! id {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
        )]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

id!(InputId);
id!(JobId);
id!(CallId);
id!(Seq);

impl Seq {
    pub const ZERO: Self = Self(0);
}

/// Monotonic time elapsed since one runtime started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonoTime(pub Duration);

impl MonoTime {
    pub fn after(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(duration))
    }
}
