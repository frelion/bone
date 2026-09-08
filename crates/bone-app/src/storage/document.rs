use std::{
    fmt,
    marker::PhantomData,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{StoreError, sqlite::StoreInner};

pub(crate) const MAX_DOCUMENT_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// The durable address of one document.
///
/// Namespaces and keys are application-defined strings. They are always bound
/// as SQLite parameters, never interpolated into SQL.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentKey {
    namespace: String,
    key: String,
}

impl DocumentKey {
    pub fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

/// The optimistic-concurrency version of one stored document.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Immutable result of reading one document.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentSnapshot<T> {
    pub value: Option<T>,
    pub revision: Revision,
}

impl<T> DocumentSnapshot<T> {
    #[cfg(test)]
    pub fn is_missing(&self) -> bool {
        self.value.is_none()
    }
}

/// One typed result from a namespace/prefix listing.
///
/// A bad payload stays attached to its durable key so callers can keep using
/// healthy records from the same namespace.
#[derive(Debug)]
pub struct DocumentListEntry<T> {
    #[cfg(test)]
    pub key: DocumentKey,
    pub snapshot: Result<DocumentSnapshot<T>, StoreError>,
}

/// A typed handle for one document key.
pub struct Document<T> {
    pub(crate) inner: Arc<StoreInner>,
    pub(crate) key: DocumentKey,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Document<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            key: self.key.clone(),
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Document<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Document")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<T> Document<T> {
    pub fn key(&self) -> &DocumentKey {
        &self.key
    }

    pub fn read(&self) -> Result<DocumentSnapshot<T>, StoreError>
    where
        T: DeserializeOwned,
    {
        let connection = self.inner.connection()?;
        read_document(&connection, &self.key)
    }

    /// Replace this document only when its revision still equals `expected`.
    /// Use revision zero to create a missing document.
    pub fn replace(&self, value: &T, expected: Revision) -> Result<Revision, StoreError>
    where
        T: Serialize,
    {
        self.inner
            .with_write(|transaction| replace_document(transaction, &self.key, value, expected))
    }
}

pub(crate) fn document<T>(inner: Arc<StoreInner>, key: DocumentKey) -> Document<T> {
    Document {
        inner,
        key,
        marker: PhantomData,
    }
}

pub(crate) fn same_store<T>(document: &Document<T>, store: &Arc<StoreInner>) -> bool {
    Arc::ptr_eq(&document.inner, store)
}

pub(crate) fn read_document<T>(
    connection: &Connection,
    key: &DocumentKey,
) -> Result<DocumentSnapshot<T>, StoreError>
where
    T: DeserializeOwned,
{
    let Some(raw) = read_raw_document(connection, key)? else {
        return Ok(DocumentSnapshot {
            value: None,
            revision: Revision::default(),
        });
    };
    Ok(DocumentSnapshot {
        value: Some(decode_payload(&raw.payload_json)?),
        revision: raw.revision,
    })
}

pub(crate) fn read_documents_with_prefix<T>(
    connection: &Connection,
    namespace: &str,
    key_prefix: &str,
) -> Result<Vec<DocumentListEntry<T>>, StoreError>
where
    T: DeserializeOwned,
{
    let (sql, upper_bound) = match binary_prefix_upper_bound(key_prefix) {
        Some(upper_bound) => (
            "
                SELECT key, revision, payload_json
                FROM documents
                WHERE namespace COLLATE BINARY = ?1
                  AND key COLLATE BINARY >= ?2
                  AND key COLLATE BINARY < CAST(?3 AS TEXT)
                ORDER BY key COLLATE BINARY ASC
            ",
            Some(upper_bound),
        ),
        None => (
            "
                SELECT key, revision, payload_json
                FROM documents
                WHERE namespace COLLATE BINARY = ?1
                ORDER BY key COLLATE BINARY ASC
            ",
            None,
        ),
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| StoreError::sqlite("prepare document list", error))?;
    let mut rows = match upper_bound {
        Some(upper_bound) => statement.query(params![namespace, key_prefix, upper_bound]),
        None => statement.query([namespace]),
    }
    .map_err(|error| StoreError::sqlite("read document list", error))?;

    let mut documents = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| StoreError::sqlite("read document list", error))?
    {
        let _key = row
            .get::<_, String>(0)
            .map_err(|error| StoreError::sqlite("read listed document key", error))?;
        let snapshot = (|| {
            let revision = row
                .get::<_, i64>(1)
                .map_err(|error| StoreError::sqlite("read listed document revision", error))?;
            let payload = row
                .get::<_, String>(2)
                .map_err(|error| StoreError::sqlite("read listed document payload", error))?;
            let raw = validate_raw_document(revision, payload)?;
            Ok(DocumentSnapshot {
                value: Some(decode_payload(&raw.payload_json)?),
                revision: raw.revision,
            })
        })();
        documents.push(DocumentListEntry {
            #[cfg(test)]
            key: DocumentKey::new(namespace, _key),
            snapshot,
        });
    }
    Ok(documents)
}

// SQLite's BINARY collation compares TEXT as UTF-8 bytes. Binding the upper
// bound as a BLOB and casting it to TEXT retains a valid byte-range even when
// incrementing the final byte would not itself be valid UTF-8.
fn binary_prefix_upper_bound(prefix: &str) -> Option<Vec<u8>> {
    if prefix.is_empty() {
        return None;
    }
    let mut bytes = prefix.as_bytes().to_vec();
    let position = bytes.iter().rposition(|byte| *byte != u8::MAX)?;
    bytes[position] += 1;
    bytes.truncate(position + 1);
    Some(bytes)
}

pub(crate) fn replace_document<T>(
    transaction: &Transaction<'_>,
    key: &DocumentKey,
    value: &T,
    expected: Revision,
) -> Result<Revision, StoreError>
where
    T: Serialize,
{
    let payload_json = encode_payload(value, MAX_DOCUMENT_PAYLOAD_BYTES)?;
    let current = read_raw_document(transaction, key)?;
    let actual = current
        .as_ref()
        .map_or_else(Revision::default, |document| document.revision);
    if actual != expected {
        return Err(StoreError::RevisionConflict { expected, actual });
    }
    if current
        .as_ref()
        .is_some_and(|document| document.payload_json == payload_json)
    {
        return Ok(actual);
    }
    let revision = actual
        .value()
        .checked_add(1)
        .map(Revision)
        .filter(|revision| i64::try_from(revision.value()).is_ok())
        .ok_or(StoreError::RevisionExhausted)?;
    let now = unix_millis()?;
    match current {
        Some(_) => {
            transaction
                .execute(
                    "
                        UPDATE documents
                        SET revision = ?3, payload_json = ?4, updated_at = ?5
                        WHERE namespace = ?1 AND key = ?2
                    ",
                    params![
                        key.namespace(),
                        key.key(),
                        i64::try_from(revision.value())
                            .map_err(|_| StoreError::RevisionExhausted)?,
                        payload_json,
                        now,
                    ],
                )
                .map_err(|error| StoreError::sqlite("replace document", error))?;
        }
        None => {
            transaction
                .execute(
                    "
                        INSERT INTO documents
                            (namespace, key, revision, payload_json, created_at, updated_at)
                        VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                    ",
                    params![
                        key.namespace(),
                        key.key(),
                        i64::try_from(revision.value())
                            .map_err(|_| StoreError::RevisionExhausted)?,
                        payload_json,
                        now,
                    ],
                )
                .map_err(|error| StoreError::sqlite("create document", error))?;
        }
    }
    Ok(revision)
}

pub(crate) fn delete_document(
    transaction: &Transaction<'_>,
    key: &DocumentKey,
    expected: Revision,
) -> Result<(), StoreError> {
    let actual = read_raw_document(transaction, key)?
        .map_or_else(Revision::default, |document| document.revision);
    if actual != expected {
        return Err(StoreError::RevisionConflict { expected, actual });
    }
    if actual == Revision::default() {
        return Ok(());
    }
    transaction
        .execute(
            "DELETE FROM documents WHERE namespace = ?1 AND key = ?2",
            params![key.namespace(), key.key()],
        )
        .map_err(|error| StoreError::sqlite("delete document", error))?;
    Ok(())
}

pub(crate) fn encode_payload<T: Serialize>(
    value: &T,
    maximum_bytes: usize,
) -> Result<String, StoreError> {
    let payload_json = serde_json::to_string(value).map_err(StoreError::Encode)?;
    if payload_json.len() > maximum_bytes {
        return Err(StoreError::PayloadTooLarge { maximum_bytes });
    }
    Ok(payload_json)
}

pub(crate) fn decode_payload<T: DeserializeOwned>(payload: &str) -> Result<T, StoreError> {
    serde_json::from_str(payload).map_err(StoreError::Decode)
}

pub(crate) fn unix_millis() -> Result<i64, StoreError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Clock)?;
    i64::try_from(duration.as_millis()).map_err(|_| StoreError::Clock)
}

struct RawDocument {
    revision: Revision,
    payload_json: String,
}

fn read_raw_document(
    connection: &Connection,
    key: &DocumentKey,
) -> Result<Option<RawDocument>, StoreError> {
    let row = connection
        .query_row(
            "SELECT revision, payload_json FROM documents WHERE namespace = ?1 AND key = ?2",
            params![key.namespace(), key.key()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| StoreError::sqlite("read document", error))?;
    row.map(|(revision, payload_json)| validate_raw_document(revision, payload_json))
        .transpose()
}

fn validate_raw_document(revision: i64, payload_json: String) -> Result<RawDocument, StoreError> {
    let revision = u64::try_from(revision)
        .ok()
        .filter(|revision| *revision > 0)
        .map(Revision)
        .ok_or(StoreError::Corrupt {
            message: "document revision is invalid",
        })?;
    if payload_json.len() > MAX_DOCUMENT_PAYLOAD_BYTES {
        return Err(StoreError::Corrupt {
            message: "document payload exceeds its maximum size",
        });
    }
    Ok(RawDocument {
        revision,
        payload_json,
    })
}
