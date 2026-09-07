use std::{fmt, marker::PhantomData, sync::Arc};

use rusqlite::{Connection, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    StoreError,
    document::{decode_payload, encode_payload, unix_millis},
    sqlite::StoreInner,
};

pub(crate) const MAX_JOURNAL_ENTRY_BYTES: usize = 1024 * 1024;

/// One immutable, ordered entry in a durable journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalEntry<E> {
    pub sequence: u64,
    pub occurred_at: i64,
    pub event: E,
}

/// A complete, validated journal read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRead<E> {
    pub entries: Vec<JournalEntry<E>>,
    pub next_sequence: u64,
}

impl<E> JournalRead<E> {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A fixed, append-only journal location.
pub struct Journal<E> {
    pub(crate) inner: Arc<StoreInner>,
    pub(crate) key: String,
    marker: PhantomData<fn() -> E>,
}

impl<E> Clone for Journal<E> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            key: self.key.clone(),
            marker: PhantomData,
        }
    }
}

impl<E> fmt::Debug for Journal<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Journal")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<E> Journal<E>
where
    E: Serialize + DeserializeOwned,
{
    pub fn read(&self) -> Result<JournalRead<E>, StoreError> {
        let connection = self.inner.connection()?;
        read_journal(&connection, &self.key)
    }

    /// Append one event under a short `BEGIN IMMEDIATE` transaction.
    pub fn append(&self, event: &E) -> Result<JournalEntry<E>, StoreError> {
        self.inner
            .with_write(|transaction| append_journal(transaction, &self.key, event))
    }
}

pub(crate) fn journal<E>(inner: Arc<StoreInner>, key: String) -> Journal<E> {
    Journal {
        inner,
        key,
        marker: PhantomData,
    }
}

pub(crate) fn same_store<E>(journal: &Journal<E>, store: &Arc<StoreInner>) -> bool {
    Arc::ptr_eq(&journal.inner, store)
}

pub(crate) fn read_journal<E>(
    connection: &Connection,
    key: &str,
) -> Result<JournalRead<E>, StoreError>
where
    E: DeserializeOwned,
{
    let mut statement = connection
        .prepare(
            "
            SELECT sequence, occurred_at, payload_json
            FROM journal_entries
            WHERE journal_key = ?1
            ORDER BY sequence ASC
            ",
        )
        .map_err(|error| StoreError::sqlite("prepare journal read", error))?;
    let rows = statement
        .query_map([key], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| StoreError::sqlite("read journal", error))?;
    let mut entries = Vec::new();
    let mut expected = 1_u64;
    for row in rows {
        let (sequence, occurred_at, payload_json) =
            row.map_err(|error| StoreError::sqlite("read journal", error))?;
        let sequence = u64::try_from(sequence)
            .ok()
            .filter(|sequence| *sequence > 0)
            .ok_or(StoreError::Corrupt {
                message: "journal sequence is invalid",
            })?;
        if sequence != expected {
            return Err(StoreError::Corrupt {
                message: "journal sequence is not contiguous",
            });
        }
        if occurred_at < 0 {
            return Err(StoreError::Corrupt {
                message: "journal timestamp is invalid",
            });
        }
        if payload_json.len() > MAX_JOURNAL_ENTRY_BYTES {
            return Err(StoreError::Corrupt {
                message: "journal payload exceeds its maximum size",
            });
        }
        let event = decode_payload(&payload_json)?;
        entries.push(JournalEntry {
            sequence,
            occurred_at,
            event,
        });
        expected = expected
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)?;
    }
    Ok(JournalRead {
        entries,
        next_sequence: expected,
    })
}

pub(crate) fn append_journal<E>(
    transaction: &Transaction<'_>,
    key: &str,
    event: &E,
) -> Result<JournalEntry<E>, StoreError>
where
    E: Serialize + DeserializeOwned,
{
    let payload_json = encode_payload(event, MAX_JOURNAL_ENTRY_BYTES)?;
    let stored_event = decode_payload(&payload_json)?;
    // A full typed read before append detects corruption and guarantees that a
    // damaged sequence can never be extended into a seemingly valid history.
    let next_sequence = read_journal::<E>(transaction, key)?.next_sequence;
    let sequence = i64::try_from(next_sequence).map_err(|_| StoreError::RevisionExhausted)?;
    let occurred_at = unix_millis()?;
    transaction
        .execute(
            "
            INSERT INTO journal_entries (journal_key, sequence, occurred_at, payload_json)
            VALUES (?1, ?2, ?3, ?4)
            ",
            params![key, sequence, occurred_at, payload_json],
        )
        .map_err(|error| StoreError::sqlite("append journal entry", error))?;
    Ok(JournalEntry {
        sequence: next_sequence,
        occurred_at,
        // The serialized form was decoded before mutation, so the returned
        // event exactly reflects the durable payload without keeping a clone
        // bound on arbitrary caller event types.
        event: stored_event,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{BoneStore, StoreRoots};

    #[test]
    fn journal_entries_are_strictly_ordered() {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let store = BoneStore::open_at(
            StoreRoots::new(
                temporary.path().join("data"),
                temporary.path().join("config"),
            )
            .unwrap(),
        )
        .unwrap();
        let journal = store
            .workspace_state()
            .session_journal::<String>("workspace", "session")
            .unwrap();
        let first = journal.append(&"one".to_owned()).unwrap();
        let second = journal.append(&"two".to_owned()).unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(journal.read().unwrap().next_sequence, 3);
    }
}
