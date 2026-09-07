use std::{fmt, marker::PhantomData, sync::Arc};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    StoreError,
    document::{decode_payload, encode_payload, unix_millis},
    sqlite::StoreInner,
};

pub(crate) const MAX_JOURNAL_ENTRY_BYTES: usize = 1024 * 1024;

/// The durable address of one append-only journal.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JournalKey(String);

impl JournalKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Metadata for a newly appended event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalAppend {
    pub sequence: u64,
    pub occurred_at: i64,
}

/// One immutable, ordered journal entry.
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

/// A typed handle for one append-only journal.
pub struct Journal<E> {
    pub(crate) inner: Arc<StoreInner>,
    pub(crate) key: JournalKey,
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

impl<E> Journal<E> {
    pub fn key(&self) -> &JournalKey {
        &self.key
    }

    pub fn read(&self) -> Result<JournalRead<E>, StoreError>
    where
        E: DeserializeOwned,
    {
        let connection = self.inner.connection()?;
        read_journal(&connection, &self.key)
    }

    /// Append one event under a short `BEGIN IMMEDIATE` transaction.
    pub fn append(&self, event: &E) -> Result<JournalAppend, StoreError>
    where
        E: Serialize,
    {
        self.inner
            .with_write(|transaction| append_journal(transaction, &self.key, event))
    }
}

pub(crate) fn journal<E>(inner: Arc<StoreInner>, key: JournalKey) -> Journal<E> {
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
    key: &JournalKey,
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
        .query_map([key.as_str()], |row| {
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
        entries.push(JournalEntry {
            sequence,
            occurred_at,
            event: decode_payload(&payload_json)?,
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
    key: &JournalKey,
    event: &E,
) -> Result<JournalAppend, StoreError>
where
    E: Serialize,
{
    let payload_json = encode_payload(event, MAX_JOURNAL_ENTRY_BYTES)?;
    let next_sequence = next_journal_sequence(transaction, key)?;
    let occurred_at = unix_millis()?;
    transaction
        .execute(
            "
                INSERT INTO journal_entries (journal_key, sequence, occurred_at, payload_json)
                VALUES (?1, ?2, ?3, ?4)
            ",
            params![
                key.as_str(),
                i64::try_from(next_sequence).map_err(|_| StoreError::RevisionExhausted)?,
                occurred_at,
                payload_json,
            ],
        )
        .map_err(|error| StoreError::sqlite("append journal entry", error))?;
    Ok(JournalAppend {
        sequence: next_sequence,
        occurred_at,
    })
}

fn next_journal_sequence(
    transaction: &Transaction<'_>,
    key: &JournalKey,
) -> Result<u64, StoreError> {
    let last_sequence = transaction
        .query_row(
            "
                SELECT sequence
                FROM journal_entries
                WHERE journal_key = ?1
                ORDER BY sequence DESC
                LIMIT 1
            ",
            [key.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| StoreError::sqlite("read journal tail", error))?;
    match last_sequence {
        None => Ok(1),
        Some(sequence) if sequence <= 0 => Err(StoreError::Corrupt {
            message: "journal sequence is invalid",
        }),
        Some(sequence) => sequence
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)
            .and_then(|sequence| {
                u64::try_from(sequence).map_err(|_| StoreError::RevisionExhausted)
            }),
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use crate::{BoneStore, JournalKey, StoreRoots};

    #[derive(Serialize)]
    struct WriteOnlyEvent {
        value: String,
    }

    fn store() -> (tempfile::TempDir, BoneStore) {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            BoneStore::open_at(StoreRoots::new(temporary.path().join("data")).unwrap()).unwrap();
        (temporary, store)
    }

    #[test]
    fn journal_entries_are_strictly_ordered() {
        let (_temporary, store) = store();
        let journal = store.journal::<String>(JournalKey::new("test/events"));
        let first = journal.append(&"one".to_owned()).unwrap();
        let second = journal.append(&"two".to_owned()).unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(journal.read().unwrap().next_sequence, 3);
    }

    #[test]
    fn append_needs_no_deserializer() {
        let (_temporary, store) = store();
        let journal = store.journal::<WriteOnlyEvent>(JournalKey::new("test/write-only"));
        assert_eq!(
            journal
                .append(&WriteOnlyEvent {
                    value: "event".to_owned(),
                })
                .unwrap()
                .sequence,
            1
        );
    }
}
