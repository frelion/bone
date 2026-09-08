use std::{fmt, marker::PhantomData, sync::Arc};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};

use super::{
    StoreError,
    document::{decode_payload, encode_payload, unix_millis},
    sqlite::StoreInner,
};

pub(crate) const MAX_JOURNAL_ENTRY_BYTES: usize = 8 * 1024 * 1024;

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
    pub last_sequence: u64,
    pub has_more: bool,
}

impl<E> JournalRead<E> {
    #[cfg(test)]
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

    #[cfg(test)]
    pub fn read(&self) -> Result<JournalRead<E>, StoreError>
    where
        E: DeserializeOwned,
    {
        let connection = self.inner.connection()?;
        read_journal(&connection, &self.key)
    }

    pub fn read_after(&self, after: u64, limit: usize) -> Result<JournalRead<E>, StoreError>
    where
        E: DeserializeOwned,
    {
        let connection = self.inner.connection()?;
        read_journal_after(&connection, &self.key, after, limit)
    }

    #[cfg(test)]
    pub fn read_entry(&self, sequence: u64) -> Result<Option<JournalEntry<E>>, StoreError>
    where
        E: DeserializeOwned,
    {
        let connection = self.inner.connection()?;
        read_journal_entry(&connection, &self.key, sequence)
    }

    pub fn last_sequence(&self) -> Result<u64, StoreError> {
        let connection = self.inner.connection()?;
        read_last_journal_sequence(&connection, &self.key)
    }

    /// Append one event under a short `BEGIN IMMEDIATE` transaction.
    #[cfg(test)]
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

#[cfg(test)]
pub(crate) fn read_journal<E>(
    connection: &Connection,
    key: &JournalKey,
) -> Result<JournalRead<E>, StoreError>
where
    E: DeserializeOwned,
{
    read_journal_after(connection, key, 0, usize::MAX)
}

pub(crate) fn read_journal_after<E>(
    connection: &Connection,
    key: &JournalKey,
    after: u64,
    limit: usize,
) -> Result<JournalRead<E>, StoreError>
where
    E: DeserializeOwned,
{
    let after_sql = i64::try_from(after).map_err(|_| StoreError::RevisionExhausted)?;
    let limit_sql = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "
                SELECT sequence, occurred_at, payload_json
                FROM journal_entries
                WHERE journal_key = ?1 AND sequence > ?2
                ORDER BY sequence ASC
                LIMIT ?3
            ",
        )
        .map_err(|error| StoreError::sqlite("prepare journal read", error))?;
    let rows = statement
        .query_map(params![key.as_str(), after_sql, limit_sql], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| StoreError::sqlite("read journal", error))?;
    let mut entries = Vec::new();
    let mut expected = after.checked_add(1).ok_or(StoreError::RevisionExhausted)?;
    let mut has_more = false;
    for row in rows {
        let (sequence, occurred_at, payload_json) =
            row.map_err(|error| StoreError::sqlite("read journal", error))?;
        if entries.len() == limit {
            has_more = true;
            break;
        }
        let entry = decode_entry(sequence, occurred_at, payload_json)?;
        let sequence = entry.sequence;
        if sequence != expected {
            return Err(StoreError::Corrupt {
                message: "journal sequence is not contiguous",
            });
        }
        entries.push(entry);
        expected = expected
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)?;
    }
    let last_sequence = entries.last().map_or(after, |entry| entry.sequence);
    Ok(JournalRead {
        entries,
        next_sequence: expected,
        last_sequence,
        has_more,
    })
}

#[cfg(test)]
pub(crate) fn read_journal_entry<E>(
    connection: &Connection,
    key: &JournalKey,
    sequence: u64,
) -> Result<Option<JournalEntry<E>>, StoreError>
where
    E: DeserializeOwned,
{
    if sequence == 0 {
        return Ok(None);
    }
    let sequence_sql = i64::try_from(sequence).map_err(|_| StoreError::RevisionExhausted)?;
    let row = connection
        .query_row(
            "
                SELECT sequence, occurred_at, payload_json
                FROM journal_entries
                WHERE journal_key = ?1 AND sequence = ?2
            ",
            params![key.as_str(), sequence_sql],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| StoreError::sqlite("read journal entry", error))?;
    row.map(|(sequence, occurred_at, payload_json)| {
        decode_entry(sequence, occurred_at, payload_json)
    })
    .transpose()
}

pub(crate) fn read_last_journal_sequence(
    connection: &Connection,
    key: &JournalKey,
) -> Result<u64, StoreError> {
    let sequence = connection
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
    match sequence {
        None => Ok(0),
        Some(sequence) => u64::try_from(sequence)
            .ok()
            .filter(|sequence| *sequence > 0)
            .ok_or(StoreError::Corrupt {
                message: "journal sequence is invalid",
            }),
    }
}

fn decode_entry<E>(
    sequence: i64,
    occurred_at: i64,
    payload_json: String,
) -> Result<JournalEntry<E>, StoreError>
where
    E: DeserializeOwned,
{
    let sequence = u64::try_from(sequence)
        .ok()
        .filter(|sequence| *sequence > 0)
        .ok_or(StoreError::Corrupt {
            message: "journal sequence is invalid",
        })?;
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
    Ok(JournalEntry {
        sequence,
        occurred_at,
        event: decode_payload(&payload_json)?,
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

    use super::super::{BoneStore, JournalKey, StoreRoots};

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
    fn journal_pages_in_sql_sequence_order() {
        let (_temporary, store) = store();
        let journal = store.journal::<String>(JournalKey::new("test/pages"));
        for value in ["one", "two", "three", "four", "five"] {
            journal.append(&value.to_owned()).unwrap();
        }

        let first = journal.read_after(0, 2).unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| (entry.sequence, entry.event.as_str()))
                .collect::<Vec<_>>(),
            [(1, "one"), (2, "two")]
        );
        assert_eq!(first.next_sequence, 3);
        assert_eq!(first.last_sequence, 2);
        assert!(first.has_more);

        let second = journal.read_after(2, 2).unwrap();
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| (entry.sequence, entry.event.as_str()))
                .collect::<Vec<_>>(),
            [(3, "three"), (4, "four")]
        );
        assert_eq!(second.next_sequence, 5);
        assert_eq!(second.last_sequence, 4);
        assert!(second.has_more);

        let last = journal.read_after(4, 2).unwrap();
        assert_eq!(last.entries[0].sequence, 5);
        assert_eq!(last.next_sequence, 6);
        assert_eq!(last.last_sequence, 5);
        assert!(!last.has_more);
        let empty = journal.read_after(5, 2).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.last_sequence, 5);
        assert!(!empty.has_more);
        assert_eq!(journal.last_sequence().unwrap(), 5);
    }

    #[test]
    fn journal_reads_one_exact_sequence() {
        let (_temporary, store) = store();
        let journal = store.journal::<String>(JournalKey::new("test/by-sequence"));
        journal.append(&"one".to_owned()).unwrap();
        journal.append(&"two".to_owned()).unwrap();

        let entry = journal.read_entry(2).unwrap().unwrap();
        assert_eq!(entry.sequence, 2);
        assert_eq!(entry.event, "two");
        assert!(journal.read_entry(0).unwrap().is_none());
        assert!(journal.read_entry(3).unwrap().is_none());
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
