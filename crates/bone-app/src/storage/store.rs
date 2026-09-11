//! Typed operations over BONE's private SQLite store.

use std::{path::Path, sync::Arc};

use serde::{Serialize, de::DeserializeOwned};

use super::{
    Document, DocumentKey, DocumentListEntry, DocumentRecentPage, DocumentSnapshot, Journal,
    JournalAppend, JournalKey, JournalRecentRead, Lease, LeaseKey, Revision, StoreError,
    StoreRoots,
    document::{
        delete_document, document, read_document, read_documents_with_prefix,
        read_recent_documents, replace_document, same_store as same_document_store,
    },
    journal::{
        append_journal, journal, read_journal_after_bounded, read_journal_recent,
        read_last_journal_sequence, same_store as same_journal_store,
    },
    lease::lease_file_name,
    security::{ensure_private_directory, try_acquire_private_lock},
    sqlite::StoreInner,
};

/// Thread-safe handle to one locally opened SQLite store.
///
/// Clones share one writer connection; reads use short-lived WAL connections.
/// SQLite provides the remaining serialization across App processes.
#[derive(Clone)]
pub struct BoneStore {
    inner: Arc<StoreInner>,
}

impl BoneStore {
    pub fn open_at(roots: StoreRoots) -> Result<Self, StoreError> {
        Ok(Self {
            inner: StoreInner::open(roots)?,
        })
    }

    pub fn database_path(&self) -> &Path {
        self.inner.database_path()
    }

    pub fn document<T>(&self, key: DocumentKey) -> Document<T> {
        document(Arc::clone(&self.inner), key)
    }

    pub fn journal<E>(&self, key: JournalKey) -> Journal<E> {
        journal(Arc::clone(&self.inner), key)
    }

    /// List typed documents in one namespace whose keys begin with `prefix`.
    ///
    /// Results use SQLite's binary collation. Each row preserves its key and
    /// reports decode failures independently.
    pub fn list_documents<T>(
        &self,
        namespace: &str,
        prefix: &str,
    ) -> Result<Vec<DocumentListEntry<T>>, StoreError>
    where
        T: DeserializeOwned,
    {
        let connection = self.inner.connection()?;
        read_documents_with_prefix(&connection, namespace, prefix)
    }

    pub fn recent_documents<T>(
        &self,
        namespace: &str,
        prefix: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<DocumentRecentPage<T>, StoreError>
    where
        T: DeserializeOwned,
    {
        let connection = self.inner.connection()?;
        read_recent_documents(&connection, namespace, prefix, before, limit)
    }

    /// Run document and journal mutations in one short `BEGIN IMMEDIATE`
    /// transaction. Returning an error rolls back every mutation.
    pub fn transaction<R>(
        &self,
        operation: impl FnOnce(&mut WriteTransaction<'_, '_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let inner = Arc::clone(&self.inner);
        self.inner.with_write(|transaction| {
            let mut write = WriteTransaction { inner, transaction };
            operation(&mut write)
        })
    }

    /// Read multiple documents from one WAL snapshot without acquiring the writer.
    pub fn read_transaction<R>(
        &self,
        operation: impl FnOnce(&WriteTransaction<'_, '_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let mut connection = self.inner.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|error| StoreError::sqlite("begin snapshot read", error))?;
        operation(&WriteTransaction {
            inner: Arc::clone(&self.inner),
            transaction: &transaction,
        })
    }

    /// Acquire an exclusive process-lifetime lease for an application-defined
    /// logical key. The key is encoded into a safe filename under `leases/`.
    pub fn try_acquire_lease(&self, key: LeaseKey) -> Result<Lease, StoreError> {
        let directory = ensure_private_directory(&self.inner.roots().data_root().join("leases"))?;
        let path = directory.join(lease_file_name(&key));
        let file = try_acquire_private_lock(&path)?;
        Ok(Lease::new(path, file))
    }
}

impl std::fmt::Debug for BoneStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoneStore")
            .field("database_path", &self.database_path())
            .finish_non_exhaustive()
    }
}

/// Typed operations available within [`BoneStore::transaction`].
pub struct WriteTransaction<'transaction, 'connection> {
    inner: Arc<StoreInner>,
    transaction: &'transaction rusqlite::Transaction<'connection>,
}

impl WriteTransaction<'_, '_> {
    pub fn read<T>(&self, document: &Document<T>) -> Result<DocumentSnapshot<T>, StoreError>
    where
        T: DeserializeOwned,
    {
        self.assert_document_store(document)?;
        read_document(self.transaction, document.key())
    }

    pub fn list_documents<T>(
        &self,
        namespace: &str,
        prefix: &str,
    ) -> Result<Vec<DocumentListEntry<T>>, StoreError>
    where
        T: DeserializeOwned,
    {
        read_documents_with_prefix(self.transaction, namespace, prefix)
    }

    pub(crate) fn recent_documents<T>(
        &self,
        namespace: &str,
        prefix: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<DocumentRecentPage<T>, StoreError>
    where
        T: DeserializeOwned,
    {
        read_recent_documents(self.transaction, namespace, prefix, before, limit)
    }

    pub fn journal_last_sequence<E>(&self, journal: &Journal<E>) -> Result<u64, StoreError> {
        self.assert_journal_store(journal)?;
        read_last_journal_sequence(self.transaction, journal.key())
    }

    pub(crate) fn read_recent<E>(
        &self,
        journal: &Journal<E>,
        cursor: Option<(u64, u64)>,
        limit: usize,
    ) -> Result<JournalRecentRead<E>, StoreError>
    where
        E: DeserializeOwned,
    {
        self.assert_journal_store(journal)?;
        read_journal_recent(self.transaction, journal.key(), cursor, limit)
    }

    pub(crate) fn read_after_bounded<E>(
        &self,
        journal: &Journal<E>,
        after: u64,
        maximum_entries: usize,
        maximum_payload_bytes: usize,
    ) -> Result<super::JournalBoundedRead<E>, StoreError>
    where
        E: DeserializeOwned,
    {
        self.assert_journal_store(journal)?;
        read_journal_after_bounded(
            self.transaction,
            journal.key(),
            after,
            maximum_entries,
            maximum_payload_bytes,
        )
    }

    pub fn replace<T>(
        &self,
        document: &Document<T>,
        value: &T,
        expected: Revision,
    ) -> Result<Revision, StoreError>
    where
        T: Serialize,
    {
        self.assert_document_store(document)?;
        replace_document(self.transaction, document.key(), value, expected)
    }

    pub fn delete<T>(&self, document: &Document<T>, expected: Revision) -> Result<(), StoreError> {
        self.assert_document_store(document)?;
        delete_document(self.transaction, document.key(), expected)
    }

    pub fn append<E>(&self, journal: &Journal<E>, event: &E) -> Result<JournalAppend, StoreError>
    where
        E: Serialize,
    {
        self.assert_journal_store(journal)?;
        append_journal(self.transaction, journal.key(), event)
    }

    fn assert_document_store<T>(&self, document: &Document<T>) -> Result<(), StoreError> {
        if same_document_store(document, &self.inner) {
            Ok(())
        } else {
            Err(StoreError::WrongStore)
        }
    }

    fn assert_journal_store<E>(&self, journal: &Journal<E>) -> Result<(), StoreError> {
        if same_journal_store(journal, &self.inner) {
            Ok(())
        } else {
            Err(StoreError::WrongStore)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Barrier},
        thread,
    };

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Settings {
        name: String,
    }

    fn store() -> (tempfile::TempDir, BoneStore) {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            BoneStore::open_at(StoreRoots::new(temporary.path().join("data")).unwrap()).unwrap();
        (temporary, store)
    }

    fn settings_key(key: &str) -> DocumentKey {
        DocumentKey::new("test.settings", key)
    }

    #[test]
    fn document_create_and_replace_use_revision_cas() {
        let (_temporary, store) = store();
        let document = store.document::<Settings>(settings_key("global"));
        let missing = document.read().unwrap();
        assert!(missing.is_missing());
        let first = document
            .replace(
                &Settings {
                    name: "first".to_owned(),
                },
                missing.revision,
            )
            .unwrap();
        assert_eq!(first.value(), 1);
        assert!(matches!(
            document.replace(
                &Settings {
                    name: "stale".to_owned(),
                },
                Revision::default()
            ),
            Err(StoreError::RevisionConflict { .. })
        ));
        assert_eq!(
            document
                .replace(
                    &Settings {
                        name: "first".to_owned(),
                    },
                    first,
                )
                .unwrap(),
            first
        );
    }

    #[test]
    fn transaction_rolls_back_document_and_journal_together() {
        let (_temporary, store) = store();
        let document = store.document::<Settings>(settings_key("session"));
        let journal = store.journal::<String>(JournalKey::new("test/session/events"));
        let failed = store.transaction(|transaction| {
            transaction.replace(
                &document,
                &Settings {
                    name: "uncommitted".to_owned(),
                },
                Revision::default(),
            )?;
            transaction.append(&journal, &"uncommitted".to_owned())?;
            Err::<(), _>(StoreError::Corrupt {
                message: "test rollback",
            })
        });
        assert!(failed.is_err());
        assert!(document.read().unwrap().is_missing());
        assert!(journal.read().unwrap().is_empty());
    }

    #[test]
    fn transaction_deletes_a_document_with_revision_checking() {
        let (_temporary, store) = store();
        let document = store.document::<Settings>(settings_key("temporary"));
        let revision = document
            .replace(
                &Settings {
                    name: "temporary".to_owned(),
                },
                Revision::default(),
            )
            .unwrap();

        store
            .transaction(|transaction| transaction.delete(&document, revision))
            .unwrap();
        assert!(document.read().unwrap().is_missing());
        assert!(matches!(
            store.transaction(|transaction| transaction.delete(&document, revision)),
            Err(StoreError::RevisionConflict { .. })
        ));
    }

    #[test]
    fn prefix_listing_is_case_sensitive_and_keeps_row_errors_local() {
        let (_temporary, store) = store();
        store
            .document::<Settings>(settings_key("workspace/a_/healthy"))
            .replace(
                &Settings {
                    name: "healthy".to_owned(),
                },
                Revision::default(),
            )
            .unwrap();
        store
            .document::<String>(settings_key("workspace/a_/damaged"))
            .replace(&"wrong shape".to_owned(), Revision::default())
            .unwrap();
        store
            .document::<Settings>(settings_key("workspace/A_/other"))
            .replace(
                &Settings {
                    name: "other".to_owned(),
                },
                Revision::default(),
            )
            .unwrap();

        let records = store
            .list_documents::<Settings>("test.settings", "workspace/a_/")
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].key.key(), "workspace/a_/damaged");
        assert!(matches!(records[0].snapshot, Err(StoreError::Decode(_))));
        assert_eq!(records[1].key.key(), "workspace/a_/healthy");
        assert!(records[1].snapshot.is_ok());
    }

    #[test]
    fn prefix_listing_handles_non_ascii_keys() {
        let (_temporary, store) = store();
        store
            .document::<Settings>(settings_key("\u{00ff}/included"))
            .replace(
                &Settings {
                    name: "included".to_owned(),
                },
                Revision::default(),
            )
            .unwrap();
        store
            .document::<Settings>(settings_key("\u{00fe}/excluded"))
            .replace(
                &Settings {
                    name: "excluded".to_owned(),
                },
                Revision::default(),
            )
            .unwrap();

        let records = store
            .list_documents::<Settings>("test.settings", "\u{00ff}")
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key.key(), "\u{00ff}/included");
    }

    #[test]
    fn lease_is_exclusive() {
        let (_temporary, store) = store();
        let lease = store
            .try_acquire_lease(LeaseKey::new("session:one"))
            .unwrap();
        assert!(matches!(
            store.try_acquire_lease(LeaseKey::new("session:one")),
            Err(StoreError::Busy)
        ));
        drop(lease);
        store
            .try_acquire_lease(LeaseKey::new("session:one"))
            .unwrap();
    }

    #[test]
    fn document_writes_have_one_winner_under_contention() {
        let (_temporary, store) = store();
        let document = Arc::new(store.document::<Settings>(settings_key("contended")));
        let expected = document.read().unwrap().revision;
        let barrier = Arc::new(Barrier::new(8));
        let joins = (0..8)
            .map(|number| {
                let document = Arc::clone(&document);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let settings = Settings {
                        name: number.to_string(),
                    };
                    barrier.wait();
                    let result = document.replace(&settings, expected);
                    (settings, result)
                })
            })
            .collect::<Vec<_>>();
        let mut winners = Vec::new();
        for join in joins {
            let (settings, result) = join.join().unwrap();
            match result {
                Ok(revision) => winners.push((settings, revision)),
                Err(StoreError::RevisionConflict {
                    expected: stale,
                    actual,
                }) => {
                    assert_eq!(stale, expected);
                    assert_eq!(actual.value(), expected.value() + 1);
                }
                other => panic!("unexpected concurrent write result: {other:?}"),
            }
        }
        assert_eq!(winners.len(), 1, "one shared revision permits one writer");
        let (settings, revision) = winners.pop().unwrap();
        let saved = document.read().unwrap();
        assert_eq!(revision.value(), expected.value() + 1);
        assert_eq!(saved.revision, revision);
        assert_eq!(saved.value, Some(settings));
    }

    #[test]
    fn reads_remain_available_while_another_connection_writes() {
        let (_temporary, store) = store();
        let writer = rusqlite::Connection::open(store.database_path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let result = store.document::<Settings>(settings_key("global")).read();
        writer.execute_batch("ROLLBACK").unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn store_root_only_contains_data() {
        let temporary = tempfile::tempdir().unwrap();
        let roots = StoreRoots::new(temporary.path().join("data")).unwrap();
        assert_eq!(
            roots.database_path(),
            temporary.path().join("data/bone.sqlite3")
        );
        assert!(!roots.data_root().exists());
        let _store = BoneStore::open_at(roots).unwrap();
        assert!(
            fs::metadata(temporary.path().join("data"))
                .unwrap()
                .is_dir()
        );
    }
}
