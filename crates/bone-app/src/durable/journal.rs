//! Durable, append-only facts for one logical BONE session.
//!
//! This module is intentionally not an Agent snapshot store. It records the
//! durable boundaries the product can safely recover: accepted user input,
//! visible replies, turn boundaries, interruption, and uncertainty about an
//! external effect. A recovered runtime must never infer that a missing job
//! should be replayed from this journal.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    JournalError, SessionId, UnixMillis,
    storage::{StoreLock, append_private_bytes, lock_path, read_private_bytes},
};

// Version 2 introduces `UserTurnAccepted`: one fsynced fact which contains
// both the user-visible message and the immutable turn configuration.  Keep
// accepting version 1 so an upgrade never turns a healthy existing transcript
// into a repair-only session.
const JOURNAL_FORMAT_VERSION: u32 = 2;
const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_FACT_TEXT_BYTES: usize = 512 * 1024;
const MAX_CONFIG_REVISION_BYTES: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 512;

/// The strictly increasing, one-based position of a durable journal fact.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JournalSequence(u64);

impl JournalSequence {
    /// The sequence assigned to the first fact in a journal.
    pub const fn first() -> Self {
        Self(1)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    fn after(self) -> Result<Self, JournalError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| JournalError::InvalidEntry {
                message: "journal sequence cannot be incremented further".to_owned(),
            })
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
    /// The atomic durable acceptance boundary for a new user turn. The TUI may
    /// clear its composer only after this one fact has been synced. Combining
    /// text and turn configuration prevents a crash between two journal
    /// appends from producing a message that looks sent but has no executable
    /// turn identity.
    UserTurnAccepted {
        turn: u64,
        text: String,
        effective_config_revision: String,
        solver_model: String,
    },
    /// Version-1 compatibility fact. New writers use `UserTurnAccepted`, but
    /// readers retain this shape so older local histories remain available.
    UserMessageAccepted {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effective_config_revision: Option<String>,
        /// The actual solver choice pinned for this accepted turn. Keeping it
        /// beside the text means a crash between journal records cannot make
        /// historical model attribution depend on later mutable settings.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        solver_model: Option<String>,
    },
    /// A turn began with an immutable configuration selection.
    TurnStarted {
        turn: u64,
        effective_config_revision: String,
        solver_model: String,
    },
    /// A visible assistant reply. This is a presentation fact, not raw model
    /// chain-of-thought or an instruction to resume an in-flight request.
    AssistantReply { text: String },
    /// A resolved terminal state for a turn.
    TurnFinished { turn: u64, outcome: TurnOutcome },
    /// Runtime work was lost/released at a recovery boundary. The next open
    /// must show this fact rather than pretending the old work still runs.
    RuntimeInterrupted { reason: String },
    /// An external side effect may already have occurred. Recovery must leave
    /// it unresolved until the user explicitly decides how to proceed.
    UnresolvedExternalEffect { summary: String },
}

impl JournalFact {
    fn validate(&self) -> Result<(), JournalError> {
        match self {
            Self::UserTurnAccepted {
                turn,
                text,
                effective_config_revision,
                solver_model,
            } => {
                if *turn == 0 {
                    return Err(invalid("turn number must be greater than zero"));
                }
                validate_required("user message", text, MAX_FACT_TEXT_BYTES)?;
                validate_required(
                    "effective configuration revision",
                    effective_config_revision,
                    MAX_CONFIG_REVISION_BYTES,
                )?;
                validate_required("solver model", solver_model, MAX_MODEL_ID_BYTES)?;
            }
            Self::UserMessageAccepted {
                text,
                effective_config_revision,
                solver_model,
            } => {
                validate_required("user message", text, MAX_FACT_TEXT_BYTES)?;
                validate_optional(
                    "effective configuration revision",
                    effective_config_revision,
                    MAX_CONFIG_REVISION_BYTES,
                )?;
                validate_optional("solver model", solver_model, MAX_MODEL_ID_BYTES)?;
            }
            Self::TurnStarted {
                turn,
                effective_config_revision,
                solver_model,
            } => {
                if *turn == 0 {
                    return Err(invalid("turn number must be greater than zero"));
                }
                validate_required(
                    "effective configuration revision",
                    effective_config_revision,
                    MAX_CONFIG_REVISION_BYTES,
                )?;
                validate_required("solver model", solver_model, MAX_MODEL_ID_BYTES)?;
            }
            Self::AssistantReply { text }
            | Self::RuntimeInterrupted { reason: text }
            | Self::UnresolvedExternalEffect { summary: text } => {
                validate_required("journal text", text, MAX_FACT_TEXT_BYTES)?;
            }
            Self::TurnFinished { turn, .. } if *turn == 0 => {
                return Err(invalid("turn number must be greater than zero"));
            }
            Self::TurnFinished { .. } => {}
        }
        Ok(())
    }
}

/// One complete, self-describing record in the JSON-lines journal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub format_version: u32,
    pub sequence: JournalSequence,
    pub occurred_at: UnixMillis,
    pub fact: JournalFact,
}

impl JournalEntry {
    fn new(sequence: JournalSequence, fact: JournalFact) -> Result<Self, JournalError> {
        let entry = Self {
            format_version: JOURNAL_FORMAT_VERSION,
            sequence,
            occurred_at: UnixMillis::now().map_err(|error| JournalError::InvalidEntry {
                message: error.to_string(),
            })?,
            fact,
        };
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> Result<(), JournalError> {
        if !matches!(self.format_version, 1 | JOURNAL_FORMAT_VERSION) {
            return Err(invalid(format!(
                "unsupported journal format version {}",
                self.format_version
            )));
        }
        if self.sequence.value() == 0 {
            return Err(invalid("journal sequence must be greater than zero"));
        }
        if self.occurred_at.as_millis() < 0 {
            return Err(invalid(
                "journal timestamp must not be before the Unix epoch",
            ));
        }
        self.fact.validate()
    }
}

/// A valid journal prefix plus an explicit recovery problem, if its tail is
/// incomplete or corrupt. Consumers can render the prefix but must not treat
/// the journal as complete or append more facts until it is repaired.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JournalRead {
    pub entries: Vec<JournalEntry>,
    pub recovery_issue: Option<JournalRecoveryIssue>,
}

impl JournalRead {
    pub fn next_sequence(&self) -> Result<JournalSequence, JournalError> {
        match self.entries.last() {
            Some(entry) => entry.sequence.after(),
            None => Ok(JournalSequence::first()),
        }
    }

    pub fn is_recoverable(&self) -> bool {
        self.recovery_issue.is_none()
    }
}

/// A non-fatal parse issue found after a valid journal prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRecoveryIssue {
    pub path: PathBuf,
    /// One-based physical JSON-lines line number.
    pub line: usize,
    pub message: String,
}

/// Handle for one durable session journal. It is cheap to clone; every read
/// and append takes a short cross-process lock at the file boundary.
#[derive(Clone, Debug)]
pub struct SessionJournal {
    session_id: SessionId,
    path: PathBuf,
    lock_path: PathBuf,
}

impl SessionJournal {
    pub(crate) fn open(session_id: SessionId, path: PathBuf) -> Self {
        let lock_path = lock_path(&path);
        Self {
            session_id,
            path,
            lock_path,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read a consistent journal prefix. A malformed or non-newline-terminated
    /// tail is returned as `recovery_issue` instead of being hidden.
    pub fn read(&self) -> Result<JournalRead, JournalError> {
        let _lock = StoreLock::acquire(&self.lock_path)?;
        self.read_unlocked()
    }

    /// Durably append a fact. Sequence allocation is performed under the same
    /// file lock, so independently running BONE processes cannot interleave
    /// JSON lines or allocate the same sequence.
    pub fn append(&self, fact: JournalFact) -> Result<JournalEntry, JournalError> {
        self.append_inner(None, fact)
    }

    /// Append only when the caller's observed next sequence is still current.
    /// This is useful for callers that optimistically assembled a UI intent
    /// from an earlier `read()` and want an explicit stale-write signal.
    pub fn append_if_next(
        &self,
        expected_next: JournalSequence,
        fact: JournalFact,
    ) -> Result<JournalEntry, JournalError> {
        self.append_inner(Some(expected_next), fact)
    }

    fn append_inner(
        &self,
        expected_next: Option<JournalSequence>,
        fact: JournalFact,
    ) -> Result<JournalEntry, JournalError> {
        let _lock = StoreLock::acquire(&self.lock_path)?;
        let read = self.read_unlocked()?;
        if read.recovery_issue.is_some() {
            return Err(JournalError::NeedsRecovery {
                session_id: self.session_id,
                path: self.path.clone(),
            });
        }
        let next = read.next_sequence()?;
        if let Some(expected_next) = expected_next.filter(|expected| *expected != next) {
            return Err(JournalError::SequenceConflict {
                session_id: self.session_id,
                expected_next,
                actual_next: next,
            });
        }
        let entry = JournalEntry::new(next, fact)?;
        let mut bytes = serde_json::to_vec(&entry).map_err(|error| JournalError::InvalidEntry {
            message: error.to_string(),
        })?;
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err(invalid(format!(
                "serialized journal entry exceeds the {MAX_ENTRY_BYTES}-byte limit"
            )));
        }
        bytes.push(b'\n');
        append_private_bytes(&self.path, &bytes)?;
        Ok(entry)
    }

    fn read_unlocked(&self) -> Result<JournalRead, JournalError> {
        let Some(bytes) = read_private_bytes(&self.path, MAX_JOURNAL_BYTES)? else {
            return Ok(JournalRead::default());
        };
        let mut read = JournalRead::default();
        let mut offset = 0;
        let mut line = 1;
        let mut expected_sequence = JournalSequence::first();
        while offset < bytes.len() {
            let Some(newline_offset) = bytes[offset..].iter().position(|byte| *byte == b'\n')
            else {
                read.recovery_issue = Some(JournalRecoveryIssue {
                    path: self.path.clone(),
                    line,
                    message: "journal tail is not terminated by a newline".to_owned(),
                });
                return Ok(read);
            };
            let end = offset + newline_offset;
            let raw = &bytes[offset..end];
            if raw.is_empty() {
                read.recovery_issue = Some(JournalRecoveryIssue {
                    path: self.path.clone(),
                    line,
                    message: "journal contains an empty record".to_owned(),
                });
                return Ok(read);
            }
            if raw.len() > MAX_ENTRY_BYTES {
                read.recovery_issue = Some(JournalRecoveryIssue {
                    path: self.path.clone(),
                    line,
                    message: format!("journal entry exceeds the {MAX_ENTRY_BYTES}-byte limit"),
                });
                return Ok(read);
            }
            let entry: JournalEntry = match serde_json::from_slice(raw) {
                Ok(entry) => entry,
                Err(error) => {
                    read.recovery_issue = Some(JournalRecoveryIssue {
                        path: self.path.clone(),
                        line,
                        message: format!("journal entry is not valid JSON: {error}"),
                    });
                    return Ok(read);
                }
            };
            if let Err(error) = entry.validate() {
                read.recovery_issue = Some(JournalRecoveryIssue {
                    path: self.path.clone(),
                    line,
                    message: error.to_string(),
                });
                return Ok(read);
            }
            if entry.sequence != expected_sequence {
                read.recovery_issue = Some(JournalRecoveryIssue {
                    path: self.path.clone(),
                    line,
                    message: format!(
                        "journal sequence is {}, expected {}",
                        entry.sequence, expected_sequence
                    ),
                });
                return Ok(read);
            }
            expected_sequence = expected_sequence.after()?;
            read.entries.push(entry);
            offset = end + 1;
            line += 1;
        }
        Ok(read)
    }
}

fn invalid(message: impl Into<String>) -> JournalError {
    JournalError::InvalidEntry {
        message: message.into(),
    }
}

fn validate_required(field: &str, value: &str, maximum_bytes: usize) -> Result<(), JournalError> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    if value.len() > maximum_bytes {
        return Err(invalid(format!(
            "{field} exceeds the {maximum_bytes}-byte limit"
        )));
    }
    Ok(())
}

fn validate_optional(
    field: &str,
    value: &Option<String>,
    maximum_bytes: usize,
) -> Result<(), JournalError> {
    if let Some(value) = value {
        validate_required(field, value, maximum_bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread, time::Duration};

    use super::*;
    use crate::{SessionStore, StorageError, WorkspaceContext, WorkspaceRegistry};

    fn private_data() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        directory
    }

    fn journal() -> (tempfile::TempDir, tempfile::TempDir, SessionJournal) {
        let data = private_data();
        let root = tempfile::tempdir().unwrap();
        let registry = WorkspaceRegistry::open_in(data.path()).unwrap();
        let workspace = WorkspaceContext::discover(root.path(), &registry).unwrap();
        let store = SessionStore::open_in(data.path(), workspace).unwrap();
        let session = store.create("Recover a failure").unwrap();
        (data, root, store.journal(session.id).unwrap())
    }

    #[test]
    fn synced_entries_round_trip_in_order() {
        let (_data, _root, journal) = journal();
        let first = journal
            .append(JournalFact::UserTurnAccepted {
                turn: 1,
                text: "inspect the failing test".into(),
                effective_config_revision: "cfg-1".into(),
                solver_model: "solver-a".into(),
            })
            .unwrap();
        let second = journal
            .append(JournalFact::AssistantReply {
                text: "I found the failing assertion.".into(),
            })
            .unwrap();

        assert_eq!(first.sequence, JournalSequence::first());
        assert_eq!(first.format_version, JOURNAL_FORMAT_VERSION);
        assert_eq!(second.sequence.value(), 2);
        let reopened = SessionJournal::open(journal.session_id(), journal.path().to_path_buf());
        let read = reopened.read().unwrap();
        assert!(read.is_recoverable());
        assert_eq!(read.entries, vec![first, second]);
    }

    #[test]
    fn version_one_history_remains_readable_after_atomic_turn_upgrade() {
        let (_data, _root, journal) = journal();
        let legacy = JournalEntry {
            format_version: 1,
            sequence: JournalSequence::first(),
            occurred_at: UnixMillis::from_millis(1).unwrap(),
            fact: JournalFact::UserMessageAccepted {
                text: "legacy saved message".into(),
                effective_config_revision: Some("cfg-1".into()),
                solver_model: Some("solver-a".into()),
            },
        };
        std::fs::write(
            journal.path(),
            format!("{}\n", serde_json::to_string(&legacy).unwrap()),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            journal.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();

        let read = journal.read().unwrap();
        assert!(read.is_recoverable());
        assert_eq!(read.entries, vec![legacy]);
    }

    #[test]
    fn atomic_turn_rejects_an_invalid_turn_before_any_file_append() {
        let (_data, _root, journal) = journal();
        assert!(matches!(
            journal.append(JournalFact::UserTurnAccepted {
                turn: 0,
                text: "inspect the failure".into(),
                effective_config_revision: "cfg-1".into(),
                solver_model: "solver-a".into(),
            }),
            Err(JournalError::InvalidEntry { .. })
        ));
        assert!(!journal.path().exists());
    }

    #[test]
    fn stale_expected_sequence_never_overwrites_or_reorders_facts() {
        let (_data, _root, journal) = journal();
        journal
            .append(JournalFact::RuntimeInterrupted {
                reason: "process exited".into(),
            })
            .unwrap();
        let error = journal
            .append_if_next(
                JournalSequence::first(),
                JournalFact::AssistantReply {
                    text: "late".into(),
                },
            )
            .unwrap_err();
        assert!(matches!(error, JournalError::SequenceConflict { .. }));
        assert_eq!(journal.read().unwrap().entries.len(), 1);
    }

    #[test]
    fn partial_tail_is_visible_and_blocks_a_further_append() {
        let (_data, _root, journal) = journal();
        journal
            .append(JournalFact::RuntimeInterrupted {
                reason: "connection lost".into(),
            })
            .unwrap();
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.path())
            .unwrap();
        file.write_all(b"{\"format_version\":1").unwrap();
        file.sync_data().unwrap();

        let read = journal.read().unwrap();
        assert_eq!(read.entries.len(), 1);
        assert!(read.recovery_issue.is_some());
        assert!(matches!(
            journal.append(JournalFact::AssistantReply {
                text: "unsafe".into()
            }),
            Err(JournalError::NeedsRecovery { .. })
        ));
    }

    #[test]
    fn concurrent_appenders_receive_a_linear_sequence() {
        let (_data, _root, journal) = journal();
        let journal = Arc::new(journal);
        let joins = (0..8)
            .map(|index| {
                let journal = Arc::clone(&journal);
                thread::spawn(move || {
                    for _ in 0..100 {
                        match journal.append(JournalFact::UserMessageAccepted {
                            text: format!("message {index}"),
                            effective_config_revision: None,
                            solver_model: None,
                        }) {
                            Ok(entry) => return entry,
                            Err(JournalError::Storage(StorageError::Busy { .. })) => {
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(error) => panic!("unexpected journal failure: {error}"),
                        }
                    }
                    panic!("journal remained busy after bounded retry")
                })
            })
            .collect::<Vec<_>>();
        let mut sequences = joins
            .into_iter()
            .map(|join| join.join().unwrap().sequence.value())
            .collect::<Vec<_>>();
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=8).collect::<Vec<_>>());
        assert_eq!(journal.read().unwrap().entries.len(), 8);
    }

    #[test]
    fn invalid_facts_are_rejected_before_any_file_append() {
        let (_data, _root, journal) = journal();
        assert!(matches!(
            journal.append(JournalFact::UserMessageAccepted {
                text: " ".into(),
                effective_config_revision: None,
                solver_model: None,
            }),
            Err(JournalError::InvalidEntry { .. })
        ));
        assert!(!journal.path().exists());
    }
}
