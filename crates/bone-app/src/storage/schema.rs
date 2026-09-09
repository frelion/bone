use rusqlite::{Connection, TransactionBehavior};

use super::StoreError;

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
        .execute_batch(CREATE_DOCUMENTS)
        .map_err(|error| StoreError::sqlite("create documents table", error))?;
    transaction
        .execute_batch(CREATE_JOURNAL_ENTRIES)
        .map_err(|error| StoreError::sqlite("create journal table", error))?;
    validate_layout(&transaction)?;
    transaction
        .commit()
        .map_err(|error| StoreError::sqlite("commit schema transaction", error))
}

/// Validate an existing database without changing its schema or journal mode.
pub(crate) fn validate_existing(connection: &Connection) -> Result<(), StoreError> {
    validate_layout(connection)
}

fn validate_layout(connection: &Connection) -> Result<(), StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_schema \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(|error| StoreError::sqlite("inspect schema tables", error))?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| StoreError::sqlite("inspect schema tables", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::sqlite("inspect schema tables", error))?;
    if tables != ["documents", "journal_entries"] {
        return Err(StoreError::Corrupt {
            message: "SQLite schema does not match the current layout",
        });
    }

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

fn require_columns(
    connection: &Connection,
    table: &'static str,
    expected: &[Column],
) -> Result<(), StoreError> {
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
    let matches = columns.len() == expected.len()
        && columns.iter().zip(expected).all(|(actual, required)| {
            actual.name == required.name
                && actual
                    .declared_type
                    .eq_ignore_ascii_case(&required.declared_type)
                && actual.not_null == required.not_null
                && actual.primary_key_order == required.primary_key_order
        });
    if !matches {
        return Err(StoreError::Corrupt {
            message: "SQLite schema does not match the current layout",
        });
    }
    Ok(())
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
