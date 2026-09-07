use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::StoreError;

pub(crate) const VERSION: i64 = 1;

const CREATE_SCHEMA_META: &str = "
    CREATE TABLE schema_meta (
        schema_version INTEGER NOT NULL
    )
";

const CREATE_DOCUMENTS: &str = "
    CREATE TABLE documents (
        namespace TEXT NOT NULL,
        key TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK (revision >= 1),
        payload_json TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (namespace, key)
    )
";

const CREATE_JOURNAL_ENTRIES: &str = "
    CREATE TABLE journal_entries (
        journal_key TEXT NOT NULL,
        sequence INTEGER NOT NULL CHECK (sequence >= 1),
        occurred_at INTEGER NOT NULL,
        payload_json TEXT NOT NULL,
        PRIMARY KEY (journal_key, sequence)
    )
";

/// Initialize only a database file created by this process.
pub(crate) fn initialize_new(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| StoreError::sqlite("begin schema transaction", error))?;
    transaction
        .execute_batch(CREATE_SCHEMA_META)
        .map_err(|error| StoreError::sqlite("create schema metadata", error))?;
    transaction
        .execute_batch(CREATE_DOCUMENTS)
        .map_err(|error| StoreError::sqlite("create documents table", error))?;
    transaction
        .execute_batch(CREATE_JOURNAL_ENTRIES)
        .map_err(|error| StoreError::sqlite("create journal table", error))?;
    transaction
        .execute(
            "INSERT INTO schema_meta (schema_version) VALUES (?1)",
            [VERSION],
        )
        .map_err(|error| StoreError::sqlite("write schema version", error))?;
    validate_v1_layout(&transaction)?;
    transaction
        .commit()
        .map_err(|error| StoreError::sqlite("commit schema transaction", error))
}

/// Validate an existing database without changing its schema or journal mode.
pub(crate) fn validate_existing(connection: &Connection) -> Result<(), StoreError> {
    require_table(connection, "schema_meta")?;
    let versions = read_schema_versions(connection)?;
    let version = match versions.as_slice() {
        [version] => *version,
        [] => {
            return Err(StoreError::Corrupt {
                message: "schema metadata has no version row",
            });
        }
        _ => {
            return Err(StoreError::Corrupt {
                message: "schema metadata has more than one version row",
            });
        }
    };
    if version != VERSION {
        return Err(StoreError::UnsupportedSchema { found: version });
    }
    validate_v1_layout(connection)
}

fn validate_v1_layout(connection: &Connection) -> Result<(), StoreError> {
    require_columns(
        connection,
        "schema_meta",
        &[Column::new("schema_version", "INTEGER", true, 0)],
    )?;
    require_columns(
        connection,
        "documents",
        &[
            Column::new("namespace", "TEXT", true, 1),
            Column::new("key", "TEXT", true, 2),
            Column::new("revision", "INTEGER", true, 0),
            Column::new("payload_json", "TEXT", true, 0),
            Column::new("created_at", "INTEGER", true, 0),
            Column::new("updated_at", "INTEGER", true, 0),
        ],
    )?;
    require_columns(
        connection,
        "journal_entries",
        &[
            Column::new("journal_key", "TEXT", true, 1),
            Column::new("sequence", "INTEGER", true, 2),
            Column::new("occurred_at", "INTEGER", true, 0),
            Column::new("payload_json", "TEXT", true, 0),
        ],
    )
}

fn require_table(connection: &Connection, table: &'static str) -> Result<(), StoreError> {
    let object_type = connection
        .query_row(
            "SELECT type FROM sqlite_schema WHERE name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| StoreError::sqlite("inspect schema table", error))?;
    if object_type.as_deref() == Some("table") {
        Ok(())
    } else {
        Err(StoreError::Corrupt {
            message: "required schema table is missing or malformed",
        })
    }
}

fn require_columns(
    connection: &Connection,
    table: &'static str,
    expected: &[Column],
) -> Result<(), StoreError> {
    require_table(connection, table)?;
    let quoted_table = quote_identifier(table);
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({quoted_table})"))
        .map_err(|error| StoreError::sqlite("inspect schema columns", error))?;
    let columns = statement
        .query_map([], |row| {
            Ok(Column {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                primary_key_order: row.get(5)?,
            })
        })
        .map_err(|error| StoreError::sqlite("inspect schema columns", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::sqlite("inspect schema columns", error))?;
    for required in expected {
        let found = columns.iter().find(|column| column.name == required.name);
        if !found.is_some_and(|actual| {
            actual
                .declared_type
                .eq_ignore_ascii_case(&required.declared_type)
                && actual.not_null == required.not_null
                && actual.primary_key_order == required.primary_key_order
        }) {
            return Err(StoreError::Corrupt {
                message: "SQLite schema is missing a required column or key",
            });
        }
    }
    Ok(())
}

fn read_schema_versions(connection: &Connection) -> Result<Vec<i64>, StoreError> {
    let result = (|| {
        let mut statement = connection.prepare("SELECT schema_version FROM schema_meta")?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
    })();
    result.map_err(
        |error| match StoreError::sqlite("read schema version", error) {
            error @ (StoreError::Busy | StoreError::Corrupt { .. }) => error,
            _ => StoreError::Corrupt {
                message: "schema metadata table is malformed",
            },
        },
    )
}

fn quote_identifier(identifier: &str) -> String {
    format!("'{}'", identifier.replace('\'', "''"))
}

#[derive(Debug, Eq, PartialEq)]
struct Column {
    name: String,
    declared_type: String,
    not_null: bool,
    primary_key_order: i64,
}

impl Column {
    fn new(
        name: &'static str,
        declared_type: &'static str,
        not_null: bool,
        primary_key_order: i64,
    ) -> Self {
        Self {
            name: name.to_owned(),
            declared_type: declared_type.to_owned(),
            not_null,
            primary_key_order,
        }
    }
}
