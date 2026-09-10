use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{PortFuture, Record, Seq};

/// Versioned Core state. Hosts persist this value without interpreting its payload.
/// Original records are stored separately and must be supplied on restoration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DurableSnapshot {
    pub(crate) version: u32,
    pub(crate) through: Seq,
    pub(crate) epoch: u64,
    pub(crate) payload: serde_json::Value,
}

impl DurableSnapshot {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn through(&self) -> Seq {
        self.through
    }
}

/// State loaded by a host before constructing a durable runtime.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DurableRestore {
    pub revision: u64,
    pub snapshot: DurableSnapshot,
    pub records: Vec<Arc<Record>>,
}

/// Atomically append new records and replace the state at an expected revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DurableCommit {
    pub commit_id: String,
    pub expected_revision: u64,
    pub snapshot: DurableSnapshot,
    pub records: Vec<Arc<Record>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableReceipt {
    pub commit_id: String,
    pub revision: u64,
    pub through: Seq,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DurableError {
    #[error("durable revision conflict")]
    Conflict,
    #[error("unsupported durable snapshot version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid durable state: {0}")]
    Invalid(String),
    #[error("durable storage failed: {0}")]
    Storage(String),
}

/// Injected persistence. Success means both state and records are durable.
/// Implementations must reject conflicts without partially applying a commit.
/// Repeating an identical commit ID must return its original receipt, including
/// after an uncertain acknowledgement. Reusing an ID with different content must
/// fail. Implementations must resolve an uncertain acknowledgement internally;
/// a returned error means the commit definitely did not apply. Revision checks
/// and record append must occur in the same transaction.
///
/// The runtime may drop the returned future when its shutdown grace expires.
/// Dropping the future does not prove that the commit failed. Hosts must keep
/// any transaction/session exclusion alive until an in-progress write resolves,
/// and reload durable state before another runtime continues the session.
pub trait DurablePort: Send + Sync + 'static {
    fn commit(&self, commit: DurableCommit) -> PortFuture<Result<DurableReceipt, DurableError>>;
}
