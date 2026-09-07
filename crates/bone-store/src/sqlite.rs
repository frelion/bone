use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use crate::{
    StoreError, StoreRoots,
    security::{
        OpenDisposition, create_private_file, ensure_private_directory, secure_open,
        validate_private_directory, validate_private_file,
    },
};

pub(crate) const SCHEMA_VERSION: i64 = 1;

#[derive(Debug)]
pub(crate) struct StoreInner {
    roots: StoreRoots,
    database_path: PathBuf,
    // Keep one configured SQLite handle open for the entire BoneStore
    // lifetime. If every short-lived connection closes at once, SQLite may
    // perform WAL recovery just as a new reader opens and report
    // `SQLITE_BUSY_RECOVERY`. The anchor never executes application work; it
    // only keeps the established WAL state alive. `Connection` is not Sync,
    // so a Mutex supplies the thread-safe ownership boundary for StoreInner.
    _anchor: Mutex<Connection>,
}

impl StoreInner {
    pub(crate) fn open(roots: StoreRoots) -> Result<Arc<Self>, StoreError> {
        let data_root = ensure_private_directory(roots.data_root())?;
        let roots = roots.with_data_root(data_root);
        let database_path = roots.database_path();
        prepare_database_file(&database_path)?;
        let mut connection = open_read_connection(&roots, &database_path)?;
        configure_write_connection(&connection)?;
        enable_write_ahead_logging(&connection)?;
        validate_database_artifacts(&database_path)?;
        initialize_schema(&mut connection)?;
        validate_database_artifacts(&database_path)?;
        Ok(Arc::new(Self {
            roots,
            database_path,
            _anchor: Mutex::new(connection),
        }))
    }

    pub(crate) fn roots(&self) -> &StoreRoots {
        &self.roots
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub(crate) fn connection(&self) -> Result<Connection, StoreError> {
        self.open_read_connection()
    }

    pub(crate) fn with_write<R>(
        self: &Arc<Self>,
        operation: impl FnOnce(&Transaction<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let mut connection = self.open_write_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| StoreError::sqlite("begin write transaction", error))?;
        let result = operation(&transaction);
        match result {
            Ok(value) => {
                // SQLite creates WAL/SHM artifacts lazily while the write is
                // in progress. Validate them *before* committing so a safety
                // failure still rolls back the domain mutation. Returning an
                // error after a successful commit would make a caller retry a
                // turn that was already durably accepted.
                validate_database_artifacts(&self.database_path)?;
                transaction
                    .commit()
                    .map_err(|error| StoreError::sqlite("commit write transaction", error))?;
                Ok(value)
            }
            Err(error) => {
                drop(transaction);
                Err(error)
            }
        }
    }

    /// Open a connection that will only read from the store.
    ///
    /// `synchronous` affects write durability only. SQLite can report
    /// `SQLITE_BUSY` while applying that connection-local pragma beside an
    /// active writer, so readers deliberately avoid it and retain WAL's
    /// concurrent-read guarantee.
    fn open_read_connection(&self) -> Result<Connection, StoreError> {
        open_read_connection(&self.roots, &self.database_path)
    }

    /// Open a connection that may begin `BEGIN IMMEDIATE` and commit durable
    /// BONE state. Only writer connections need `synchronous = FULL`; applying
    /// it before the transaction begins gives every BONE write the required
    /// durability policy without making concurrent WAL reads spuriously busy.
    fn open_write_connection(&self) -> Result<Connection, StoreError> {
        let connection = self.open_read_connection()?;
        configure_write_connection(&connection)?;
        Ok(connection)
    }
}

fn open_read_connection(
    roots: &StoreRoots,
    database_path: &Path,
) -> Result<Connection, StoreError> {
    validate_private_directory(roots.data_root())?;
    prepare_database_file(database_path)?;
    // Validate existing SQLite sidecars before opening the database. In
    // particular, SQLite may otherwise inspect a pre-existing WAL during
    // connection setup before the post-open validation below gets a chance to
    // reject it.
    validate_database_artifacts(database_path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_FULL_MUTEX;
    let connection = Connection::open_with_flags(database_path, flags)
        .map_err(|error| StoreError::sqlite("open database", error))?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|error| StoreError::sqlite("configure busy timeout", error))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| StoreError::sqlite("enable foreign keys", error))?;
    validate_database_artifacts(database_path)?;
    Ok(connection)
}

fn configure_write_connection(connection: &Connection) -> Result<(), StoreError> {
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| StoreError::sqlite("configure synchronous mode", error))
}

/// Set the database-wide journal policy while the store is being opened.
///
/// `journal_mode` persists in the database. Re-running the mutating pragma on
/// every short-lived reader can contend with another process's write
/// transaction, so the open path establishes it once and ordinary connections
/// only apply their connection-local policies.
fn enable_write_ahead_logging(connection: &Connection) -> Result<(), StoreError> {
    let mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(|error| StoreError::sqlite("enable write-ahead logging", error))?;
    if mode.eq_ignore_ascii_case("wal") {
        Ok(())
    } else {
        Err(StoreError::Corrupt {
            message: "SQLite database did not accept WAL mode",
        })
    }
}

fn prepare_database_file(path: &Path) -> Result<(), StoreError> {
    match crate::security::existing_private_file(path)? {
        Some(file) => {
            drop(file);
            Ok(())
        }
        None => match create_private_file(path)? {
            Some(file) => {
                file.sync_all()
                    .map_err(|error| StoreError::io("sync database file", path, error))?;
                Ok(())
            }
            None => {
                let file = secure_open(path, OpenDisposition::Existing)?;
                drop(file);
                Ok(())
            }
        },
    }
}

fn initialize_schema(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| StoreError::sqlite("begin schema transaction", error))?;
    transaction
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_meta (
                schema_version INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS documents (
                namespace TEXT NOT NULL,
                key TEXT NOT NULL,
                revision INTEGER NOT NULL CHECK (revision >= 1),
                payload_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (namespace, key)
            );

            CREATE TABLE IF NOT EXISTS journal_entries (
                journal_key TEXT NOT NULL,
                sequence INTEGER NOT NULL CHECK (sequence >= 1),
                occurred_at INTEGER NOT NULL,
                payload_json TEXT NOT NULL,
                PRIMARY KEY (journal_key, sequence)
            );
            ",
        )
        .map_err(|error| StoreError::sqlite("create store schema", error))?;

    let versions = {
        let mut statement = transaction
            .prepare("SELECT schema_version FROM schema_meta")
            .map_err(|error| StoreError::sqlite("read schema version", error))?;
        let rows = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|error| StoreError::sqlite("read schema version", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::sqlite("read schema version", error))?
    };
    match versions.as_slice() {
        [] => {
            transaction
                .execute(
                    "INSERT INTO schema_meta (schema_version) VALUES (?1)",
                    [SCHEMA_VERSION],
                )
                .map_err(|error| StoreError::sqlite("initialize schema version", error))?;
        }
        [version] if *version == SCHEMA_VERSION => {}
        [version] => return Err(StoreError::UnsupportedSchema { found: *version }),
        _ => {
            return Err(StoreError::Corrupt {
                message: "schema metadata has more than one version row",
            });
        }
    }
    transaction
        .commit()
        .map_err(|error| StoreError::sqlite("commit schema transaction", error))
}

fn validate_database_artifacts(database_path: &Path) -> Result<(), StoreError> {
    validate_private_file(database_path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = database_path.as_os_str().to_os_string();
        name.push(suffix);
        let path = PathBuf::from(name);
        // `Path::exists` follows symlinks and therefore treats a broken
        // sidecar symlink as absent. Inspect the directory entry itself so a
        // malicious or stale WAL/SHM/journal symlink is always rejected
        // before SQLite can use it.
        match fs::symlink_metadata(&path) {
            Ok(_) => validate_private_file(&path)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::io("inspect database sidecar", &path, error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn roots() -> (tempfile::TempDir, StoreRoots) {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let roots = StoreRoots::new(
            temporary.path().join("data"),
            temporary.path().join("config"),
        )
        .unwrap();
        (temporary, roots)
    }

    #[test]
    fn initializes_a_private_wal_database() {
        let (_temporary, roots) = roots();
        let store = StoreInner::open(roots).unwrap();
        assert!(store.database_path().is_file());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(store.database_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let connection = store.connection().unwrap();
        let version: i64 = connection
            .query_row("SELECT schema_version FROM schema_meta", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        connection
            .execute(
                "INSERT INTO documents (namespace, key, revision, payload_json, created_at, updated_at) VALUES ('test', 'wal', 1, '{}', 0, 0)",
                [],
            )
            .unwrap();
        #[cfg(unix)]
        for suffix in ["-wal", "-shm"] {
            let mut artifact = store.database_path().as_os_str().to_os_string();
            artifact.push(suffix);
            let artifact = PathBuf::from(artifact);
            assert_eq!(
                fs::metadata(&artifact).unwrap().permissions().mode() & 0o777,
                0o600,
                "{} must be private",
                artifact.display(),
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_broken_database_sidecar_symlink_before_opening_sqlite() {
        use std::os::unix::fs::symlink;

        let (_temporary, roots) = roots();
        let store = StoreInner::open(roots.clone()).unwrap();
        let mut wal = store.database_path().as_os_str().to_os_string();
        wal.push("-wal");
        let wal = PathBuf::from(wal);
        drop(store);
        match fs::remove_file(&wal) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => panic!("remove WAL before symlink test: {error}"),
        }
        symlink("does-not-exist", &wal).unwrap();

        assert!(matches!(
            StoreInner::open(roots),
            Err(StoreError::UnsafeStorage { .. })
        ));
    }

    #[test]
    fn opening_a_store_reestablishes_wal_mode() {
        let (_temporary, roots) = roots();
        let store = StoreInner::open(roots.clone()).unwrap();
        let database_path = store.database_path().to_path_buf();
        drop(store);
        let connection = Connection::open(&database_path).unwrap();
        let mode: String = connection
            .pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))
            .unwrap();
        assert!(mode.eq_ignore_ascii_case("delete"));
        drop(connection);

        let reopened = StoreInner::open(roots).unwrap();
        let connection = Connection::open(reopened.database_path()).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert!(mode.eq_ignore_ascii_case("wal"));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_wal_artifacts_abort_a_write_before_it_commits() {
        use std::os::unix::fs::PermissionsExt;

        let (_temporary, roots) = roots();
        let store = StoreInner::open(roots).unwrap();
        let database_path = store.database_path().to_path_buf();
        let keeper = store.connection().unwrap();
        keeper
            .execute(
                "INSERT INTO documents (namespace, key, revision, payload_json, created_at, updated_at) VALUES ('test', 'keep-wal-open', 1, '{}', 0, 0)",
                [],
            )
            .unwrap();
        let wal = sidecar_path(&database_path, "-wal");
        assert!(wal.is_file());
        let result = store.with_write(|transaction| {
            transaction
                .execute(
                    "INSERT INTO documents (namespace, key, revision, payload_json, created_at, updated_at) VALUES ('test', 'unsafe-wal', 1, '{}', 0, 0)",
                    [],
                )
                .map_err(|error| StoreError::sqlite("insert test document", error))?;
            fs::set_permissions(&wal, fs::Permissions::from_mode(0o644))
                .map_err(|error| StoreError::io("make WAL unsafe", &wal, error))?;
            Ok(())
        });
        assert!(matches!(result, Err(StoreError::UnsafeStorage { .. })));

        fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).unwrap();
        let connection = store.connection().unwrap();
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM documents WHERE namespace = 'test' AND key = 'unsafe-wal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    fn sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
        let mut name = database_path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }
}
