use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;

use crate::state::{Event, JobState, SessionState, UnknownWrite};

/// SQLite is the sole authority for snapshots and immutable event contents.
pub struct Store {
    connection: Connection,
    lock_directory: PathBuf,
}

/// Held for the whole session runtime, including recovery and shutdown.
pub struct SessionLease {
    file: File,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("opening session store {}", path.display()))?;
        let lock_directory = path.canonicalize()?.with_extension("session-locks");
        fs::create_dir_all(&lock_directory)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS sessions (
                 id TEXT PRIMARY KEY,
                 revision INTEGER NOT NULL CHECK (revision >= 0),
                 snapshot TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS events (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 id TEXT NOT NULL UNIQUE,
                 session_id TEXT NOT NULL REFERENCES sessions(id),
                 call_id TEXT,
                 revision INTEGER NOT NULL CHECK (revision >= 0),
                 payload TEXT NOT NULL,
                 job_id TEXT,
                 metadata TEXT
             );
             CREATE INDEX IF NOT EXISTS events_session_order ON events(session_id, sequence);
             CREATE INDEX IF NOT EXISTS events_call ON events(session_id, call_id);",
        )?;
        let columns = {
            let mut statement = connection.prepare("PRAGMA table_info(events)")?;
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<BTreeSet<_>, _>>()?
        };
        if !columns.contains("job_id") {
            connection.execute("ALTER TABLE events ADD COLUMN job_id TEXT", [])?;
        }
        if !columns.contains("metadata") {
            connection.execute("ALTER TABLE events ADD COLUMN metadata TEXT", [])?;
        }
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             UPDATE events SET job_id = json_extract(payload, '$.job_id'), metadata = json_set(json_remove(payload, '$.data.message', '$.data.response', '$.data.stream_items', '$.data.covered_ids'), '$.data.usage', json_extract(payload, '$.data.response.usage')) WHERE metadata IS NULL;
             CREATE INDEX IF NOT EXISTS events_job_order ON events(session_id, job_id, sequence);
             COMMIT;"
        )?;
        Ok(Self {
            connection,
            lock_directory,
        })
    }

    pub fn acquire_session(&self, id: &str) -> Result<SessionLease> {
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid session ID for ownership lock"
        );
        let path = self.lock_directory.join(format!("{id}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock_exclusive()
            .with_context(|| format!("session {id} is already owned by another process"))?;
        Ok(SessionLease { file })
    }

    pub fn create_session(&self, state: &SessionState) -> Result<()> {
        ensure!(state.revision == 0, "new session revision must be zero");
        let transaction = self.connection.unchecked_transaction()?;
        let snapshot = serde_json::to_string(state)?;
        if let Some(existing) = snapshot_in(&transaction, &state.id)? {
            ensure!(
                existing == *state,
                "session ID {} already has different contents",
                state.id
            );
            return Ok(());
        }
        validate_structure(state)?;
        transaction.execute(
            "INSERT INTO sessions (id, revision, snapshot) VALUES (?1, 0, ?2)",
            params![state.id, snapshot],
        )?;
        validate_references(&transaction, state, None)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn load_session(&self, id: &str) -> Result<SessionState> {
        let (revision, snapshot): (i64, String) = self
            .connection
            .query_row(
                "SELECT revision, snapshot FROM sessions WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .with_context(|| format!("loading session {id}"))?;
        let state: SessionState = serde_json::from_str(&snapshot)?;
        ensure!(
            state.id == id && state.revision == u64::try_from(revision)?,
            "corrupt session snapshot metadata"
        );
        Ok(state)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionState>> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM sessions ORDER BY rowid DESC")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| self.load_session(&id)).collect()
    }

    pub fn events(&self, session_id: &str) -> Result<Vec<Event>> {
        let mut statement = self
            .connection
            .prepare("SELECT payload FROM events WHERE session_id = ?1 ORDER BY sequence")?;
        let payloads = statement
            .query_map([session_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        payloads
            .into_iter()
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .collect()
    }

    /// Projection avoids loading native transcript bodies into the live runtime.
    pub fn event_metadata(&self, session_id: &str) -> Result<Vec<Event>> {
        let mut statement = self
            .connection
            .prepare("SELECT metadata FROM events WHERE session_id = ?1 ORDER BY sequence")?;
        statement
            .query_map([session_id], |row| row.get::<_, String>(0))?
            .map(|row| serde_json::from_str(&row?).map_err(Into::into))
            .collect()
    }

    /// Read a bounded page in append order. The cursor must belong to this session.
    pub fn history_page(
        &self,
        session_id: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Event>, bool)> {
        ensure!(
            (1..=1_000).contains(&limit),
            "limit must be between 1 and 1000"
        );
        let mut sequence = 0_i64;
        if let Some(cursor) = after {
            sequence = self
                .connection
                .query_row(
                    "SELECT sequence FROM events WHERE session_id = ?1 AND id = ?2",
                    params![session_id, cursor],
                    |row| row.get(0),
                )
                .with_context(|| format!("unknown cursor {cursor} for session {session_id}"))?;
        }
        let mut statement = self.connection.prepare(
            "SELECT payload FROM events WHERE session_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3"
        )?;
        let payloads = statement
            .query_map(params![session_id, sequence, (limit + 1) as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = payloads.len() > limit;
        let events = payloads
            .into_iter()
            .take(limit)
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .collect::<Result<Vec<Event>>>()?;
        Ok((events, has_more))
    }

    /// Read older records in append order without traversing the whole log.
    pub fn history_before(
        &self,
        session_id: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Event>, bool)> {
        ensure!(
            (1..=1_000).contains(&limit),
            "limit must be between 1 and 1000"
        );
        let sequence = match before {
            Some(cursor) => self
                .connection
                .query_row(
                    "SELECT sequence FROM events WHERE session_id = ?1 AND id = ?2",
                    params![session_id, cursor],
                    |row| row.get::<_, i64>(0),
                )
                .with_context(|| format!("unknown cursor {cursor} for session {session_id}"))?,
            None => i64::MAX,
        };
        let mut statement = self.connection.prepare(
            "SELECT payload FROM events WHERE session_id = ?1 AND sequence < ?2 ORDER BY sequence DESC LIMIT ?3"
        )?;
        let payloads = statement
            .query_map(params![session_id, sequence, (limit + 1) as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = payloads.len() > limit;
        let mut events = payloads
            .into_iter()
            .take(limit)
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .collect::<Result<Vec<Event>>>()?;
        events.reverse();
        Ok((events, more))
    }

    pub fn read_event(&self, session_id: &str, id: &str) -> Result<Event> {
        let payload: String = self
            .connection
            .query_row(
                "SELECT payload FROM events WHERE session_id = ?1 AND id = ?2",
                params![session_id, id],
                |row| row.get(0),
            )
            .with_context(|| format!("reading event {id}"))?;
        Ok(serde_json::from_str(&payload)?)
    }

    pub fn read_events(&self, session_id: &str, ids: &BTreeSet<String>) -> Result<Vec<Event>> {
        let ids = ids.iter().collect::<Vec<_>>();
        let mut result = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let sql = format!(
                "SELECT payload FROM events WHERE session_id = ? AND id IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let mut statement = self.connection.prepare(&sql)?;
            let values = std::iter::once(session_id).chain(chunk.iter().map(|id| id.as_str()));
            let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                result.push(serde_json::from_str(&row?)?);
            }
        }
        ensure!(
            result.len() == ids.len(),
            "working history refers to missing events"
        );
        Ok(result)
    }

    /// Reverse chronological audit metadata, independently of compacted history.
    pub fn job_records(
        &self,
        session_id: &str,
        job_id: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Event>> {
        let before_sequence: i64 = if let Some(before) = before {
            self.connection
                .query_row(
                    "SELECT sequence FROM events WHERE session_id=?1 AND job_id=?2 AND id=?3",
                    params![session_id, job_id, before],
                    |row| row.get(0),
                )
                .context("before_id is not an audit record of this job")?
        } else {
            i64::MAX
        };
        let mut statement = self.connection.prepare("SELECT metadata FROM events WHERE session_id=?1 AND job_id=?2 AND sequence<?3 ORDER BY sequence DESC LIMIT ?4")?;
        statement
            .query_map(
                params![session_id, job_id, before_sequence, i64::try_from(limit)?],
                |row| row.get::<_, String>(0),
            )?
            .map(|row| serde_json::from_str(&row?).map_err(Into::into))
            .collect()
    }

    /// The snapshot and all new events become visible together, or neither does.
    /// Reusing an event ID is allowed only when every field is identical.
    pub fn commit(&self, state: &SessionState, events: &[Event]) -> Result<()> {
        validate_structure(state)?;
        let transaction = self.connection.unchecked_transaction()?;
        let existing = snapshot_in(&transaction, &state.id)?
            .with_context(|| format!("session {} does not exist", state.id))?;
        ensure!(
            state.revision >= existing.revision,
            "session revision moved backwards"
        );
        let revision = i64::try_from(state.revision)
            .context("session revision exceeds SQLite integer range")?;
        for event in events {
            ensure!(
                event.session_id == state.id,
                "event {} belongs to another session",
                event.id
            );
            ensure!(
                event.revision <= state.revision,
                "event revision exceeds snapshot revision"
            );
            let existing_payload: Option<String> = transaction
                .query_row(
                    "SELECT payload FROM events WHERE id = ?1",
                    [&event.id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(payload) = existing_payload {
                let stored: Event = serde_json::from_str(&payload)?;
                ensure!(
                    stored == *event,
                    "event ID {} has conflicting immutable contents",
                    event.id
                );
                continue;
            }
            if let Some(job) = &event.job_id {
                ensure!(
                    state.jobs.contains_key(job),
                    "event {} refers to missing job {job}",
                    event.id
                );
            }
            transaction.execute(
                "INSERT INTO events (id, session_id, call_id, revision, payload, job_id, metadata) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![event.id, event.session_id, event.call_id, i64::try_from(event.revision)?, serde_json::to_string(event)?, event.job_id, serde_json::to_string(&event.metadata())?],
            )?;
        }
        validate_references(&transaction, state, Some(&existing))?;
        for event in events {
            for id in event.reply_to.iter().chain(event.root_input.iter()) {
                ensure!(
                    event_exists(&transaction, &state.id, id)?,
                    "event {} refers to missing event {id}",
                    event.id
                );
            }
        }
        transaction.execute(
            "UPDATE sessions SET revision = ?2, snapshot = ?3 WHERE id = ?1",
            params![state.id, revision, serde_json::to_string(state)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Must run after acquiring the session lease. Never reruns a tool.
    pub fn recover_session(&self, id: &str) -> Result<SessionState> {
        let mut state = self.load_session(id)?;
        let original = state.clone();
        let events = self.event_metadata(id)?;
        let mut unfinished = BTreeMap::new();
        for event in &events {
            let Some(call_id) = &event.call_id else {
                continue;
            };
            match event.kind.as_str() {
                "tool_started" | "call_started" => {
                    unfinished.insert(call_id.clone(), event);
                }
                "tool_result"
                    if event
                        .data
                        .get("uncertain")
                        .and_then(|value| value.as_bool())
                        == Some(true) => {}
                "tool_result" | "call_finished" | "call_failed" | "tool_reconciled" => {
                    unfinished.remove(call_id);
                }
                _ => {}
            }
        }
        for (call_id, event) in unfinished {
            if event.data.get("effect").and_then(|value| value.as_str()) != Some("write") {
                continue;
            }
            let job_id = event.job_id.clone().context("write call has no job ID")?;
            state
                .unknown_writes
                .entry(call_id.clone())
                .or_insert_with(|| UnknownWrite {
                    call_id,
                    job_id,
                    root_input: event.root_input.clone(),
                    tool_name: event
                        .data
                        .get("tool_name")
                        .or_else(|| event.data.get("name"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown")
                        .to_owned(),
                });
        }
        for job in state.jobs.values_mut() {
            if job.state != JobState::Closed
                && (matches!(
                    job.state,
                    JobState::Ready | JobState::Running | JobState::Waiting
                ) || job.current_call.is_some()
                    || job.active_input.is_some()
                    || !job.inbox.is_empty()
                    || !job.wait_for.is_empty())
            {
                job.state = JobState::Paused;
                state.paused = true;
            }
        }
        if !state.unknown_writes.is_empty() {
            state.paused = true;
        }
        if state != original {
            state.revision = state
                .revision
                .checked_add(1)
                .context("session revision overflow")?;
            let mut event = Event::new(
                id,
                "stopped",
                json!({"reason": "recovered", "unknown_writes": state.unknown_writes.keys().collect::<Vec<_>>() }),
            );
            event.revision = state.revision;
            self.commit(&state, &[event])?;
        }
        Ok(state)
    }
}

fn snapshot_in(transaction: &Transaction<'_>, id: &str) -> Result<Option<SessionState>> {
    let snapshot: Option<String> = transaction
        .query_row("SELECT snapshot FROM sessions WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    snapshot
        .map(|snapshot| serde_json::from_str(&snapshot).map_err(Into::into))
        .transpose()
}

fn validate_structure(state: &SessionState) -> Result<()> {
    ensure!(!state.id.is_empty(), "session ID is empty");
    if let Some(focus) = &state.focus {
        ensure!(
            state.jobs.contains_key(focus),
            "focus refers to missing job {focus}"
        );
    }
    for (key, job) in &state.jobs {
        ensure!(key == &job.id, "job map key does not match job ID");
    }
    for budget in state.budgets.values() {
        ensure!(
            budget.calls_used <= budget.max_calls && budget.jobs_used <= budget.max_jobs,
            "input budget exceeded"
        );
    }
    for (key, write) in &state.unknown_writes {
        ensure!(
            key == &write.call_id && state.jobs.contains_key(&write.job_id),
            "invalid unknown write reference"
        );
    }
    Ok(())
}

fn event_exists(transaction: &Transaction<'_>, session_id: &str, id: &str) -> Result<bool> {
    Ok(transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1 AND session_id = ?2)",
        params![id, session_id],
        |row| row.get(0),
    )?)
}

fn references(state: &SessionState) -> BTreeSet<&str> {
    let mut ids: BTreeSet<_> = state.pending_inputs.iter().map(String::as_str).collect();
    ids.extend(state.budgets.keys().map(String::as_str));
    for job in state.jobs.values() {
        ids.extend(
            job.inbox
                .iter()
                .chain(job.active_input.iter())
                .chain(job.history.iter())
                .chain(job.wait_for.iter())
                .chain(job.summary.iter())
                .map(String::as_str),
        );
    }
    for write in state.unknown_writes.values() {
        ids.extend(write.root_input.iter().map(String::as_str));
    }
    ids
}

fn validate_references(
    transaction: &Transaction<'_>,
    state: &SessionState,
    previous: Option<&SessionState>,
) -> Result<()> {
    // Immutable event IDs already checked in the prior atomic snapshot need no
    // further point queries. Validate only newly introduced references in batches.
    let previous_ids = previous.map(references).unwrap_or_default();
    let references = references(state)
        .difference(&previous_ids)
        .copied()
        .collect::<Vec<_>>();
    for chunk in references.chunks(500) {
        let sql = format!(
            "SELECT COUNT(*) FROM events WHERE session_id = ? AND id IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let values = std::iter::once(state.id.as_str()).chain(chunk.iter().copied());
        let count: i64 =
            transaction.query_row(&sql, rusqlite::params_from_iter(values), |row| row.get(0))?;
        ensure!(
            count == i64::try_from(chunk.len())?,
            "session snapshot refers to missing event"
        );
    }
    for job in state.jobs.values() {
        if let Some(call_id) = &job.current_call {
            if previous
                .and_then(|s| s.jobs.get(&job.id))
                .is_some_and(|old| old.current_call.as_ref() == Some(call_id))
            {
                continue;
            }
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE session_id = ?1 AND call_id = ?2)",
                params![state.id, call_id],
                |row| row.get(0),
            )?;
            ensure!(exists, "job {} refers to missing call {call_id}", job.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Budget, Job};

    fn fixture() -> (tempfile::TempDir, Store, SessionState, Event) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("bone.sqlite")).unwrap();
        let mut state = SessionState::new(directory.path());
        let job = Job::new("main");
        state.focus = Some(job.id.clone());
        state.jobs.insert(job.id.clone(), job.clone());
        store.create_session(&state).unwrap();
        let mut input = Event::new(&state.id, "input", json!({"text": "hello"}));
        input.job_id = Some(job.id.clone());
        input.root_input = Some(input.id.clone());
        input.revision = 1;
        state.revision = 1;
        state
            .jobs
            .get_mut(&job.id)
            .unwrap()
            .inbox
            .push_back(input.id.clone());
        state.pending_inputs.push_back(input.id.clone());
        state.budgets.insert(input.id.clone(), Budget::new(5, 3));
        (directory, store, state, input)
    }

    #[test]
    fn history_page_uses_append_order_and_bounded_cursor_reads() {
        let (_directory, store, mut state, first) = fixture();
        store.commit(&state, std::slice::from_ref(&first)).unwrap();
        let mut second = Event::new(&state.id, "second", json!({"body":"original"}));
        second.revision = 2;
        state.revision = 2;
        store.commit(&state, std::slice::from_ref(&second)).unwrap();
        let (page, more) = store.history_page(&state.id, None, 1).unwrap();
        assert_eq!(page, vec![first.clone()]);
        assert!(more);
        let (last, more) = store.history_page(&state.id, Some(&first.id), 1).unwrap();
        assert_eq!(last, vec![second]);
        assert!(!more);
        assert!(store.history_page(&state.id, Some("unknown"), 1).is_err());
        assert!(store.history_page(&state.id, None, 0).is_err());
    }

    #[test]
    fn history_before_preserves_append_order_and_session_scoped_backward_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("sessions.sqlite3")).unwrap();
        let state = SessionState::new(directory.path());
        let foreign = SessionState::new(directory.path());
        store.create_session(&state).unwrap();
        store.create_session(&foreign).unwrap();
        let mut expected = Vec::new();
        let foreign_event = Event::new(&foreign.id, "notice", json!({"text":"foreign"}));
        for index in 0..5 {
            let mut event = Event::new(&state.id, "notice", json!({"index":index}));
            // Timestamp ties and reverse lexical IDs must not alter append order.
            event.timestamp = "1".into();
            event.id = format!("event-{}", 5 - index);
            store.commit(&state, std::slice::from_ref(&event)).unwrap();
            expected.push(event);
            if index == 1 {
                store
                    .commit(&foreign, std::slice::from_ref(&foreign_event))
                    .unwrap();
            }
        }
        let tail = crate::history_before(directory.path(), &state.id, None, 2).unwrap();
        assert_eq!(tail.events, expected[3..]);
        assert!(tail.has_more);
        assert_eq!(tail.next_cursor.as_deref(), Some(expected[3].id.as_str()));
        let older =
            crate::history_before(directory.path(), &state.id, tail.next_cursor.as_deref(), 2)
                .unwrap();
        assert_eq!(older.events, expected[1..3]);
        assert!(older.has_more);
        let first =
            crate::history_before(directory.path(), &state.id, older.next_cursor.as_deref(), 2)
                .unwrap();
        assert_eq!(first.events, expected[..1]);
        assert!(!first.has_more);
        let combined = first
            .events
            .into_iter()
            .chain(older.events)
            .chain(tail.events)
            .collect::<Vec<_>>();
        assert_eq!(combined, expected);
        let empty =
            crate::history_before(directory.path(), &state.id, first.next_cursor.as_deref(), 2)
                .unwrap();
        assert!(empty.events.is_empty());
        assert!(empty.next_cursor.is_none());
        assert!(!empty.has_more);
        assert!(
            crate::history_before(directory.path(), &state.id, Some(&foreign_event.id), 2).is_err()
        );
        assert!(crate::history_before(directory.path(), &state.id, Some("unknown"), 2).is_err());
        assert!(crate::history_before(directory.path(), &state.id, None, 0).is_err());
        assert!(crate::history_before(directory.path(), &state.id, None, 1001).is_err());
        assert_eq!(
            crate::history_before(directory.path(), &foreign.id, None, 2)
                .unwrap()
                .events,
            vec![foreign_event]
        );
    }

    #[test]
    fn restart_keeps_snapshot_and_one_copy_of_native_message() {
        let (directory, store, mut state, input) = fixture();
        store.commit(&state, &[input]).unwrap();
        let mut message = Event::new(
            &state.id,
            "model_message",
            json!({"role":"assistant", "content":[{"type":"text", "text":"native"}]}),
        );
        message.job_id = state.focus.clone();
        message.revision = 2;
        state.revision = 2;
        state
            .jobs
            .get_mut(state.focus.as_ref().unwrap())
            .unwrap()
            .history
            .push(message.id.clone());
        store
            .commit(&state, std::slice::from_ref(&message))
            .unwrap();
        drop(store);
        let restarted = Store::open(directory.path().join("bone.sqlite")).unwrap();
        assert_eq!(restarted.load_session(&state.id).unwrap(), state);
        assert_eq!(
            restarted.events(&state.id).unwrap().last().unwrap(),
            &message
        );
        assert!(!serde_json::to_string(&state).unwrap().contains("native"));
    }

    #[test]
    fn duplicate_ids_are_idempotent_and_conflicts_rollback() {
        let (_directory, store, mut state, input) = fixture();
        store.commit(&state, std::slice::from_ref(&input)).unwrap();
        store.commit(&state, std::slice::from_ref(&input)).unwrap();
        assert_eq!(store.events(&state.id).unwrap().len(), 1);
        let previous = state.clone();
        state.revision += 1;
        let mut fresh = Event::new(&state.id, "summary", json!({}));
        fresh.revision = state.revision;
        let mut conflict = input;
        conflict.data = json!({"text": "changed"});
        assert!(store.commit(&state, &[fresh, conflict]).is_err());
        assert_eq!(store.load_session(&state.id).unwrap(), previous);
        assert_eq!(store.events(&state.id).unwrap().len(), 1);
    }

    #[test]
    fn job_transitions_preserve_instruction_revision_and_reject_regression() {
        let (_directory, store, mut state, input) = fixture();
        store.commit(&state, &[input]).unwrap();
        state
            .jobs
            .get_mut(state.focus.as_ref().unwrap())
            .unwrap()
            .state = JobState::Ready;
        let mut event = Event::new(&state.id, "context_note", json!({"reason":"ready"}));
        event.revision = state.revision;
        store.commit(&state, &[event]).unwrap();
        assert_eq!(store.load_session(&state.id).unwrap(), state);
        assert_eq!(store.events(&state.id).unwrap().len(), 2);
        state.revision = 0;
        assert!(store.commit(&state, &[]).is_err());
        assert_eq!(store.load_session(&state.id).unwrap().revision, 1);
    }

    #[test]
    fn sqlite_fault_after_event_insert_rolls_back_snapshot_and_events() {
        let (_directory, store, state, input) = fixture();
        let before = store.load_session(&state.id).unwrap();
        store.connection.execute_batch("CREATE TRIGGER inject_fault BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT, 'injected disk write fault'); END;").unwrap();
        assert!(store.commit(&state, std::slice::from_ref(&input)).is_err());
        assert_eq!(store.load_session(&state.id).unwrap(), before);
        assert!(store.events(&state.id).unwrap().is_empty());
        store
            .connection
            .execute_batch("DROP TRIGGER inject_fault;")
            .unwrap();
        store.commit(&state, &[input]).unwrap();
    }

    #[test]
    fn dangling_references_roll_back() {
        let (_directory, store, mut state, input) = fixture();
        state.pending_inputs.push_back("missing".into());
        assert!(store.commit(&state, &[input]).is_err());
        assert_eq!(store.load_session(&state.id).unwrap().revision, 0);
        assert!(store.events(&state.id).unwrap().is_empty());
    }

    #[test]
    fn session_ownership_is_exclusive_and_released() {
        let (directory, store, state, _) = fixture();
        let another = Store::open(directory.path().join("bone.sqlite")).unwrap();
        let lease = store.acquire_session(&state.id).unwrap();
        assert!(another.acquire_session(&state.id).is_err());
        drop(lease);
        assert!(another.acquire_session(&state.id).is_ok());
    }

    #[test]
    fn recovery_pauses_work_and_records_uncertain_writes_without_replay() {
        let (directory, store, mut state, input) = fixture();
        let mut started = Event::new(
            &state.id,
            "tool_started",
            json!({"effect":"write", "tool_name":"write_file"}),
        );
        started.job_id = state.focus.clone();
        started.root_input = Some(input.id.clone());
        started.call_id = Some("tool-call-1".into());
        started.revision = 1;
        let job = state.jobs.get_mut(state.focus.as_ref().unwrap()).unwrap();
        job.active_input = Some(job.inbox.pop_front().unwrap());
        job.current_call = started.call_id.clone();
        job.state = JobState::Running;
        let original_input = job.active_input.clone();
        store.commit(&state, &[input, started]).unwrap();
        drop(store);
        let store = Store::open(directory.path().join("bone.sqlite")).unwrap();
        let _lease = store.acquire_session(&state.id).unwrap();
        let recovered = store.recover_session(&state.id).unwrap();
        assert!(recovered.paused);
        let job = &recovered.jobs[state.focus.as_ref().unwrap()];
        assert_eq!(job.state, JobState::Paused);
        assert_eq!(job.active_input, original_input);
        assert_eq!(job.current_call.as_deref(), Some("tool-call-1"));
        assert_eq!(
            recovered.unknown_writes["tool-call-1"].tool_name,
            "write_file"
        );
        assert!(!directory.path().join("output.txt").exists());
        assert_eq!(store.recover_session(&state.id).unwrap(), recovered);
        assert_eq!(store.events(&state.id).unwrap().len(), 3);
    }

    #[test]
    fn restart_pauses_queued_and_waiting_work_and_preserves_idle_and_closed_jobs() {
        let (_directory, store, mut state, input) = fixture();
        let original_job = state.focus.clone().unwrap();
        state.jobs.get_mut(&original_job).unwrap().state = JobState::Ready;
        let mut waiting = Job::new("waiting");
        waiting.state = JobState::Waiting;
        waiting.wait_for.push(input.id.clone());
        let waiting_id = waiting.id.clone();
        let mut idle_with_work = Job::new("queued");
        idle_with_work.inbox.push_back(input.id.clone());
        let queued_id = idle_with_work.id.clone();
        let idle = Job::new("idle");
        let idle_id = idle.id.clone();
        let mut closed = Job::new("closed");
        closed.state = JobState::Closed;
        let closed_id = closed.id.clone();
        for job in [waiting, idle_with_work, idle, closed] {
            state.jobs.insert(job.id.clone(), job);
        }
        store.commit(&state, std::slice::from_ref(&input)).unwrap();
        let recovered = store.recover_session(&state.id).unwrap();
        assert!(recovered.paused);
        for id in [&original_job, &waiting_id, &queued_id] {
            assert_eq!(recovered.jobs[id].state, JobState::Paused);
        }
        assert_eq!(
            recovered.jobs[&original_job].inbox,
            state.jobs[&original_job].inbox
        );
        assert_eq!(recovered.jobs[&waiting_id].wait_for, vec![input.id]);
        assert_eq!(recovered.jobs[&idle_id].state, JobState::Idle);
        assert_eq!(recovered.jobs[&closed_id].state, JobState::Closed);
        assert_eq!(store.recover_session(&state.id).unwrap(), recovered);
        assert_eq!(store.events(&state.id).unwrap().len(), 2);
    }

    #[test]
    fn uncertain_native_result_stays_unknown_until_explicit_reconciliation() {
        let (_directory, store, mut state, input) = fixture();
        let mut started = Event::new(
            &state.id,
            "tool_started",
            json!({"effect":"write","tool_name":"write_file"}),
        );
        started.job_id = state.focus.clone();
        started.call_id = Some("uncertain-call".into());
        started.root_input = Some(input.id.clone());
        started.revision = 1;
        let mut result = Event::new(&state.id, "tool_result", json!({"uncertain":true}));
        result.job_id = state.focus.clone();
        result.call_id = started.call_id.clone();
        result.revision = 1;
        store.commit(&state, &[input, started, result]).unwrap();
        state = store.recover_session(&state.id).unwrap();
        assert!(state.unknown_writes.contains_key("uncertain-call"));
        state.unknown_writes.remove("uncertain-call");
        let mut reconciled = Event::new(
            &state.id,
            "tool_reconciled",
            json!({"observation":"inspected"}),
        );
        reconciled.job_id = state.focus.clone();
        reconciled.call_id = Some("uncertain-call".into());
        reconciled.revision = state.revision;
        store.commit(&state, &[reconciled]).unwrap();
        assert!(
            store
                .recover_session(&state.id)
                .unwrap()
                .unknown_writes
                .is_empty()
        );
    }

    #[test]
    fn finished_write_is_not_uncertain_after_restart() {
        let (_directory, store, mut state, input) = fixture();
        let mut started = Event::new(&state.id, "tool_started", json!({"effect":"write"}));
        started.job_id = state.focus.clone();
        started.call_id = Some("completed".into());
        started.revision = 1;
        let mut result = Event::new(&state.id, "tool_result", json!({"ok":true}));
        result.job_id = state.focus.clone();
        result.call_id = started.call_id.clone();
        result.revision = 1;
        state
            .jobs
            .get_mut(state.focus.as_ref().unwrap())
            .unwrap()
            .state = JobState::Idle;
        store.commit(&state, &[input, started, result]).unwrap();
        assert!(
            store
                .recover_session(&state.id)
                .unwrap()
                .unknown_writes
                .is_empty()
        );
    }
}
