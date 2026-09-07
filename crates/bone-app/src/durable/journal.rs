//! Durable, append-only facts for one logical BONE session.
//!
//! The journal is a typed view over `bone-store`'s SQLite table. It has no
//! format version, tail-recovery path, sidecar lock, or JSONL compatibility
//! layer: SQLite commits every row atomically and is the only source of truth.

use std::{fmt, sync::Arc};

use bone_store::Journal;
use serde::{Deserialize, Serialize};

use super::{JournalError, SessionId, SessionWriterLease, UnixMillis, WorkspaceId};

const MAX_FACT_TEXT_BYTES: usize = 512 * 1024;
const MAX_RUNTIME_FINGERPRINT_BYTES: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 512;

/// The strictly increasing, one-based position of a durable journal fact.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JournalSequence(u64);

impl JournalSequence {
    pub const fn first() -> Self {
        Self(1)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    fn from_store(sequence: u64) -> Result<Self, JournalError> {
        if sequence == 0 {
            return Err(invalid("journal sequence must be greater than zero"));
        }
        Ok(Self(sequence))
    }

    fn after(self) -> Result<Self, JournalError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| invalid("journal sequence cannot be incremented further"))
    }
}

impl fmt::Display for JournalSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A terminal-visible result for a logical user turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Stopped,
    WaitingForUser,
    Failed,
}

/// An append-only product fact. It purposefully contains no runtime handle,
/// task future, credential, or executable tool request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalFact {
    /// The durable acceptance boundary for a new user turn. The app commits
    /// this fact and the session summary in one SQLite transaction before it
    /// clears the composer or schedules an Agent effect.
    UserTurnAccepted {
        turn: u64,
        text: String,
        runtime_fingerprint: String,
        solver_model: String,
    },
    /// The runtime has acknowledged receipt of a durable turn.
    TurnStarted {
        turn: u64,
        runtime_fingerprint: String,
        solver_model: String,
    },
    /// A visible assistant reply. This is presentation data, not model
    /// reasoning or an instruction to resume in-flight work.
    AssistantReply { text: String },
    /// A resolved terminal state for a turn.
    TurnFinished { turn: u64, outcome: TurnOutcome },
    /// Runtime work was lost/released at a recovery boundary.
    RuntimeInterrupted { reason: String },
    /// An external side effect may already have occurred and requires an
    /// explicit user decision after recovery.
    UnresolvedExternalEffect { summary: String },
}

impl JournalFact {
    pub(crate) fn validate(&self) -> Result<(), JournalError> {
        match self {
            Self::UserTurnAccepted {
                turn,
                text,
                runtime_fingerprint,
                solver_model,
            } => {
                if *turn == 0 {
                    return Err(invalid("turn number must be greater than zero"));
                }
                validate_text("user message", text, MAX_FACT_TEXT_BYTES)?;
                validate_identifier(
                    "runtime fingerprint",
                    runtime_fingerprint,
                    MAX_RUNTIME_FINGERPRINT_BYTES,
                )?;
                validate_identifier("solver model", solver_model, MAX_MODEL_ID_BYTES)?;
            }
            Self::TurnStarted {
                turn,
                runtime_fingerprint,
                solver_model,
            } => {
                if *turn == 0 {
                    return Err(invalid("turn number must be greater than zero"));
                }
                validate_identifier(
                    "runtime fingerprint",
                    runtime_fingerprint,
                    MAX_RUNTIME_FINGERPRINT_BYTES,
                )?;
                validate_identifier("solver model", solver_model, MAX_MODEL_ID_BYTES)?;
            }
            Self::AssistantReply { text }
            | Self::RuntimeInterrupted { reason: text }
            | Self::UnresolvedExternalEffect { summary: text } => {
                validate_text("journal text", text, MAX_FACT_TEXT_BYTES)?;
            }
            Self::TurnFinished { turn, .. } if *turn == 0 => {
                return Err(invalid("turn number must be greater than zero"));
            }
            Self::TurnFinished { .. } => {}
        }
        Ok(())
    }
}

/// One complete durable journal row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalEntry {
    pub sequence: JournalSequence,
    pub occurred_at: UnixMillis,
    pub fact: JournalFact,
}

impl JournalEntry {
    pub(crate) fn from_store(
        entry: bone_store::JournalEntry<JournalFact>,
    ) -> Result<Self, JournalError> {
        let entry = Self {
            sequence: JournalSequence::from_store(entry.sequence)?,
            occurred_at: UnixMillis::from_millis(entry.occurred_at)
                .map_err(|error| invalid(format!("journal timestamp is invalid: {error}")))?,
            fact: entry.event,
        };
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> Result<(), JournalError> {
        if self.occurred_at.as_millis() < 0 {
            return Err(invalid(
                "journal timestamp must not be before the Unix epoch",
            ));
        }
        self.fact.validate()
    }
}

/// A complete SQLite journal read. Corrupt database rows surface as a storage
/// error; there is no partially recovered JSONL prefix.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JournalRead {
    pub entries: Vec<JournalEntry>,
}

impl JournalRead {
    pub fn next_sequence(&self) -> Result<JournalSequence, JournalError> {
        match self.entries.last() {
            Some(entry) => entry.sequence.after(),
            None => Ok(JournalSequence::first()),
        }
    }
}

/// Typed journal handle for one existing Session record.
#[derive(Clone)]
pub struct SessionJournal {
    session_id: SessionId,
    workspace_id: WorkspaceId,
    lease_issuer: Arc<()>,
    journal: Journal<JournalFact>,
}

impl fmt::Debug for SessionJournal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionJournal")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl SessionJournal {
    pub(crate) fn new(
        session_id: SessionId,
        workspace_id: WorkspaceId,
        lease_issuer: Arc<()>,
        journal: Journal<JournalFact>,
    ) -> Self {
        Self {
            session_id,
            workspace_id,
            lease_issuer,
            journal,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn read(&self) -> Result<JournalRead, JournalError> {
        let read = self.journal.read()?;
        let entries = read
            .entries
            .into_iter()
            .map(JournalEntry::from_store)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(JournalRead { entries })
    }

    /// Append a fact only while this process owns the matching Session writer
    /// lease. Reads intentionally remain lease-free so other BONE processes
    /// can render a held conversation as read-only.
    pub fn append(
        &self,
        lease: &SessionWriterLease,
        fact: JournalFact,
    ) -> Result<JournalEntry, JournalError> {
        lease
            .assert_grants_write(self.workspace_id, self.session_id, &self.lease_issuer)
            .map_err(JournalError::Session)?;
        fact.validate()?;
        JournalEntry::from_store(self.journal.append(&fact)?)
    }
}

fn invalid(message: impl Into<String>) -> JournalError {
    JournalError::InvalidEntry {
        message: message.into(),
    }
}

fn validate_text(field: &str, value: &str, maximum_bytes: usize) -> Result<(), JournalError> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{field} must contain non-whitespace text")));
    }
    if value.len() > maximum_bytes {
        return Err(invalid(format!(
            "{field} exceeds the {maximum_bytes}-byte limit"
        )));
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str, maximum_bytes: usize) -> Result<(), JournalError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(invalid(format!(
            "{field} must be non-empty without surrounding whitespace"
        )));
    }
    if value.len() > maximum_bytes {
        return Err(invalid(format!(
            "{field} exceeds the {maximum_bytes}-byte limit"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_turn_requires_all_pinned_runtime_attribution() {
        assert!(
            JournalFact::UserTurnAccepted {
                turn: 1,
                text: "hello".into(),
                runtime_fingerprint: "abc".into(),
                solver_model: "gpt-test".into(),
            }
            .validate()
            .is_ok()
        );
        assert!(
            JournalFact::UserTurnAccepted {
                turn: 0,
                text: "hello".into(),
                runtime_fingerprint: "abc".into(),
                solver_model: "gpt-test".into(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn user_text_preserves_meaningful_surrounding_whitespace() {
        assert!(
            JournalFact::UserTurnAccepted {
                turn: 1,
                text: "  preserve this code block indentation\n".into(),
                runtime_fingerprint: "abc".into(),
                solver_model: "gpt-test".into(),
            }
            .validate()
            .is_ok()
        );
    }
}
