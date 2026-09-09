use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use super::{
    StoreError, StoreRoots, schema,
    security::{
        FileDisposition, ensure_private_directory, ensure_private_file, validate_private_directory,
        validate_private_file,
    },
};

const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) struct StoreInner {
    roots: StoreRoots,
    database_path: PathBuf,
    // One connection serializes this App's short write transactions and keeps
    // a quiet store from repeatedly tearing down and recovering its WAL.
    writer: Mutex<Connection>,
}

impl StoreInner {
    pub(crate) fn open(roots: StoreRoots) -> Result<Arc<Self>, StoreError> {
        let data_root = ensure_private_directory(roots.data_root())?;
        let roots = roots.with_data_root(data_root);
        let database_path = roots.database_path();
        let disposition = ensure_private_file(&database_path)?;

        // Existing files are validated before this process asks SQLite to
        // change journal mode or create any application object.
        if disposition == FileDisposition::Existing {
            let validation = open_validation_connection(&roots, &database_path)?;
            schema::validate_existing(&validation)?;
        }

        let mut connection = open_connection(&roots, &database_path)?;
        configure_writer(&connection)?;
        enable_wal(&connection)?;
        if disposition == FileDisposition::Created {
            schema::initialize_new(&mut connection)?;
        }
        validate_database_artifacts(&database_path)?;

        Ok(Arc::new(Self {
            roots,
            database_path,
            writer: Mutex::new(connection),
        }))
    }

    pub(crate) fn roots(&self) -> &StoreRoots {
        &self.roots
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub(crate) fn connection(&self) -> Result<Connection, StoreError> {
        open_connection(&self.roots, &self.database_path)
    }

    pub(crate) fn with_write<R>(
        self: &Arc<Self>,
        operation: impl FnOnce(&Transaction<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let mut connection = self.writer.lock().expect("store writer mutex poisoned");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| StoreError::sqlite("begin write transaction", error))?;
        match operation(&transaction) {
            Ok(value) => {
                transaction
                    .commit()
                    .map_err(|error| StoreError::sqlite("commit write transaction", error))?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
}

fn open_validation_connection(
    roots: &StoreRoots,
    database_path: &Path,
) -> Result<Connection, StoreError> {
    validate_private_directory(roots.data_root())?;
    validate_database_artifacts(database_path)?;
    let connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
    .map_err(|error| StoreError::sqlite("open database for schema validation", error))?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|error| StoreError::sqlite("configure schema validation timeout", error))?;
    Ok(connection)
}

fn open_connection(roots: &StoreRoots, database_path: &Path) -> Result<Connection, StoreError> {
    validate_private_directory(roots.data_root())?;
    validate_database_artifacts(database_path)?;
    let connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
    .map_err(|error| StoreError::sqlite("open database", error))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| StoreError::sqlite("configure busy timeout", error))?;
    Ok(connection)
}

fn configure_writer(connection: &Connection) -> Result<(), StoreError> {
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| StoreError::sqlite("configure synchronous mode", error))
}

fn enable_wal(connection: &Connection) -> Result<(), StoreError> {
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

fn validate_database_artifacts(database_path: &Path) -> Result<(), StoreError> {
    validate_private_file(database_path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = database_path.as_os_str().to_os_string();
        name.push(suffix);
        let artifact = PathBuf::from(name);
        match fs::symlink_metadata(&artifact) {
            Ok(_) => validate_private_file(&artifact)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::io("inspect database sidecar", artifact, error)),
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
        let roots = StoreRoots::new(temporary.path().join("data")).unwrap();
        (temporary, roots)
    }

    #[test]
    fn initializes_a_private_wal_database() {
        let (_temporary, roots) = roots();
        let store = StoreInner::open(roots).unwrap();
        let connection = store.connection().unwrap();
        schema::validate_existing(&connection).unwrap();
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(store.database_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn reopens_an_existing_database() {
        let (_temporary, roots) = roots();
        let store = StoreInner::open(roots.clone()).unwrap();
        let database_path = store.database_path().to_owned();
        drop(store);

        let reopened = StoreInner::open(roots).unwrap();
        assert_eq!(reopened.database_path(), database_path);
        schema::validate_existing(&reopened.connection().unwrap()).unwrap();
    }

    #[test]
    fn refuses_a_damaged_existing_database_without_repairing_it() {
        let (_temporary, roots) = roots();
        let data_root = ensure_private_directory(roots.data_root()).unwrap();
        let database_path = data_root.join("bone.sqlite3");
        let connection = Connection::open(&database_path).unwrap();
        connection
            .execute_batch("CREATE TABLE documents (key TEXT NOT NULL);")
            .unwrap();
        drop(connection);
        #[cfg(unix)]
        fs::set_permissions(&database_path, fs::Permissions::from_mode(0o600)).unwrap();
        let bytes = fs::read(&database_path).unwrap();

        assert!(matches!(
            StoreInner::open(roots),
            Err(StoreError::Corrupt { .. })
        ));
        assert_eq!(fs::read(database_path).unwrap(), bytes);
    }

    #[test]
    fn refuses_a_non_current_layout_before_changing_journal_mode() {
        let (_temporary, roots) = roots();
        let store = StoreInner::open(roots.clone()).unwrap();
        let database_path = store.database_path().to_owned();
        drop(store);

        let connection = Connection::open(&database_path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        connection
            .execute_batch("CREATE TABLE unexpected (value TEXT NOT NULL);")
            .unwrap();
        drop(connection);

        assert!(matches!(
            StoreInner::open(roots),
            Err(StoreError::Corrupt { .. })
        ));
        let connection = Connection::open(database_path).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert!(mode.eq_ignore_ascii_case("delete"));
    }
}
