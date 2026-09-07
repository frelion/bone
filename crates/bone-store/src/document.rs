use std::{
    fmt,
    marker::PhantomData,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{StoreError, sqlite::StoreInner};

pub(crate) const MAX_DOCUMENT_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// The optimistic-concurrency version of one stored document.
///
/// Revision zero represents an absent document. Every persisted document has
/// a positive revision assigned by SQLite; callers must use the snapshot's
/// revision for a compare-and-swap replacement.
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

/// Immutable typed result of reading one document.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentSnapshot<T> {
    pub value: Option<T>,
    pub revision: Revision,
}

impl<T> DocumentSnapshot<T> {
    pub fn is_missing(&self) -> bool {
        self.value.is_none()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DocumentAddress {
    pub(crate) namespace: &'static str,
    pub(crate) key: String,
}

/// One fixed, typed BONE document location.
///
/// Applications receive documents from a scoped store capability rather than
/// constructing arbitrary namespaces or keys. This prevents the storage API
/// from degrading into an untyped key/value database.
pub struct Document<T> {
    pub(crate) inner: Arc<StoreInner>,
    pub(crate) address: DocumentAddress,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Document<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            address: self.address.clone(),
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Document<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Document")
            .field("namespace", &self.address.namespace)
            .field("key", &self.address.key)
            .finish_non_exhaustive()
    }
}

impl<T> Document<T>
where
    T: Serialize + DeserializeOwned,
{
    pub fn read(&self) -> Result<DocumentSnapshot<T>, StoreError> {
        let connection = self.inner.connection()?;
        read_document(&connection, &self.address)
    }

    /// Replace this document only if `expected` is still current.
    ///
    /// To create a previously missing document, pass `Revision::default()`.
    /// Replacing an identical encoded payload is idempotent and returns its
    /// existing revision without creating a new version.
    pub fn replace(&self, value: &T, expected: Revision) -> Result<Revision, StoreError> {
        self.inner
            .with_write(|transaction| replace_document(transaction, &self.address, value, expected))
    }

    /// Remove this document only if `expected` is still current.
    ///
    /// Removing a missing document with revision zero is an idempotent no-op.
    pub fn remove(&self, expected: Revision) -> Result<(), StoreError> {
        self.inner
            .with_write(|transaction| remove_document(transaction, &self.address, expected))
    }
}

pub(crate) fn document<T>(
    inner: Arc<StoreInner>,
    namespace: &'static str,
    key: String,
) -> Document<T> {
    Document {
        inner,
        address: DocumentAddress { namespace, key },
        marker: PhantomData,
    }
}

pub(crate) fn same_store<T>(document: &Document<T>, store: &Arc<StoreInner>) -> bool {
    Arc::ptr_eq(&document.inner, store)
}

pub(crate) fn read_document<T>(
    connection: &Connection,
    address: &DocumentAddress,
) -> Result<DocumentSnapshot<T>, StoreError>
where
    T: DeserializeOwned,
{
    let Some(raw) = read_raw_document(connection, address)? else {
        return Ok(DocumentSnapshot {
            value: None,
            revision: Revision::default(),
        });
    };
    let value = serde_json::from_str(&raw.payload_json).map_err(StoreError::Decode)?;
    Ok(DocumentSnapshot {
        value: Some(value),
        revision: raw.revision,
    })
}

pub(crate) fn read_documents_with_prefix<T>(
    connection: &Connection,
    namespace: &'static str,
    key_prefix: &str,
) -> Result<Vec<DocumentSnapshot<T>>, StoreError>
where
    T: DeserializeOwned,
{
    // Workspace/session IDs may legally contain `_`. In SQL `LIKE`, however,
    // `_` is a single-character wildcard, which would let a prefix query
    // return records belonging to a different Workspace. Escape every LIKE
    // metacharacter even though the current domain key grammar only admits
    // `_`; this keeps the storage boundary correct if a future fixed key adds
    // one of the other characters.
    let pattern = format!("{}%", escape_like_literal(key_prefix));
    let mut statement = connection
        .prepare(
            "
            SELECT revision, payload_json
            FROM documents
            WHERE namespace = ?1 AND key LIKE ?2 ESCAPE '\\'
            ORDER BY key ASC
            ",
        )
        .map_err(|error| StoreError::sqlite("prepare document list", error))?;
    let rows = statement
        .query_map(params![namespace, pattern], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| StoreError::sqlite("read document list", error))?;
    rows.map(|row| {
        let (revision, payload_json) =
            row.map_err(|error| StoreError::sqlite("read document list", error))?;
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
        Ok(DocumentSnapshot {
            value: Some(decode_payload(&payload_json)?),
            revision,
        })
    })
    .collect()
}

fn escape_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub(crate) fn replace_document<T>(
    transaction: &Transaction<'_>,
    address: &DocumentAddress,
    value: &T,
    expected: Revision,
) -> Result<Revision, StoreError>
where
    T: Serialize,
{
    let payload_json = encode_payload(value, MAX_DOCUMENT_PAYLOAD_BYTES)?;
    let current = read_raw_document(transaction, address)?;
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
        .0
        .checked_add(1)
        .map(Revision)
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
                        address.namespace,
                        address.key,
                        i64::try_from(revision.0).map_err(|_| StoreError::RevisionExhausted)?,
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
                        address.namespace,
                        address.key,
                        i64::try_from(revision.0).map_err(|_| StoreError::RevisionExhausted)?,
                        payload_json,
                        now,
                    ],
                )
                .map_err(|error| StoreError::sqlite("create document", error))?;
        }
    }
    Ok(revision)
}

pub(crate) fn remove_document(
    transaction: &Transaction<'_>,
    address: &DocumentAddress,
    expected: Revision,
) -> Result<(), StoreError> {
    let current = read_raw_document(transaction, address)?;
    let actual = current
        .as_ref()
        .map_or_else(Revision::default, |document| document.revision);
    if actual != expected {
        return Err(StoreError::RevisionConflict { expected, actual });
    }
    if current.is_some() {
        transaction
            .execute(
                "DELETE FROM documents WHERE namespace = ?1 AND key = ?2",
                params![address.namespace, address.key],
            )
            .map_err(|error| StoreError::sqlite("remove document", error))?;
    }
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
    address: &DocumentAddress,
) -> Result<Option<RawDocument>, StoreError> {
    let row = connection
        .query_row(
            "SELECT revision, payload_json FROM documents WHERE namespace = ?1 AND key = ?2",
            params![address.namespace, address.key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| StoreError::sqlite("read document", error))?;
    row.map(|(revision, payload_json)| {
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
    })
    .transpose()
}
