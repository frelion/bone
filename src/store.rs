use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;

use crate::state::{Event, JobState, SessionState};

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
                 metadata TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS events_session_order ON events(session_id, sequence);
             CREATE INDEX IF NOT EXISTS events_call ON events(session_id, call_id);
             CREATE INDEX IF NOT EXISTS events_job_order ON events(session_id, job_id, sequence);",
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
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
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
        self.history_slice(session_id, after, limit, false)
    }

    /// Read older records in append order without traversing the whole log.
    pub fn history_before(
        &self,
        session_id: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Event>, bool)> {
        self.history_slice(session_id, before, limit, true)
    }

    fn history_slice(
        &self,
        session_id: &str,
        cursor: Option<&str>,
        limit: usize,
        backwards: bool,
    ) -> Result<(Vec<Event>, bool)> {
        ensure!(
            (1..=1_000).contains(&limit),
            "limit must be between 1 and 1000"
        );
        let sequence = match cursor {
            Some(cursor) => self
                .connection
                .query_row(
                    "SELECT sequence FROM events WHERE session_id = ?1 AND id = ?2",
                    params![session_id, cursor],
                    |row| row.get::<_, i64>(0),
                )
                .with_context(|| format!("unknown cursor {cursor} for session {session_id}"))?,
            None => {
                if backwards {
                    i64::MAX
                } else {
                    0
                }
            }
        };
        let query = if backwards {
            "SELECT payload FROM events WHERE session_id = ?1 AND sequence < ?2 ORDER BY sequence DESC LIMIT ?3"
        } else {
            "SELECT payload FROM events WHERE session_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3"
        };
        let mut statement = self.connection.prepare(query)?;
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
        if backwards {
            events.reverse();
        }
        Ok((events, more))
    }

    pub fn history_search(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::HistoryMatch>> {
        ensure!(
            !query.trim().is_empty() && query.len() <= 4096,
            "query must contain 1–4096 bytes of text"
        );
        ensure!(
            (1..=100).contains(&limit),
            "search limit must be between 1 and 100"
        );
        let query = query.to_lowercase();
        let mut matches = Vec::new();
        let mut cursor = None;
        loop {
            let (events, more) = self.history_page(session_id, cursor.as_deref(), 1)?;
            let Some(event) = events.into_iter().next() else {
                break;
            };
            cursor = Some(event.id.clone());
            let text = match event.kind.as_str() {
                "input" | "tool_result" | "context_note" => readable_text(&event.data["message"]),
                "model_message" | "summary" | "stale" => {
                    readable_text(&event.data["response"]["choice"])
                }
                "question" | "failure" | "input_paused" => readable_text(&event.data),
                _ => String::new(),
            };
            if let Some(offset) = text.to_lowercase().find(&query) {
                // Lowercasing can change byte length (e.g. İ), so map the match
                // back to original character indices before selecting context.
                let mut lower_bytes = 0;
                let start = text
                    .chars()
                    .take_while(|character| {
                        let before = lower_bytes;
                        lower_bytes += character.to_lowercase().map(char::len_utf8).sum::<usize>();
                        before < offset
                    })
                    .count()
                    .saturating_sub(60);
                let snippet = text.chars().skip(start).take(240).collect();
                matches.push(crate::HistoryMatch {
                    event_id: event.id,
                    kind: event.kind,
                    snippet,
                });
                if matches.len() == limit {
                    break;
                }
            }
            if !more {
                break;
            }
        }
        Ok(matches)
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
        // These reads always lead to writes. Acquire the writer reservation
        // before reading so concurrent Store opens/session commits can wait
        // through busy_timeout rather than failing a deferred WAL upgrade.
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
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
            // No runtime future survives reopening this session. Native tool
            // results for unfinished starts are recovered by Engine.
            job.current_call = None;
        }
        if state != original {
            state.revision = state
                .revision
                .checked_add(1)
                .context("session revision overflow")?;
            let mut event = Event::new(id, "stopped", json!({"reason": "recovered"}));
            event.revision = state.revision;
            self.commit(&state, &[event])?;
        }
        Ok(state)
    }
}

/// Project only native readable parts and known tool evidence. Transport metadata,
/// hidden reasoning and encrypted blocks are excluded. Tool paths and commands
/// are searchable without projecting arbitrary argument/transport fields.
fn readable_text(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(readable_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("reasoning" | "encrypted") => String::new(),
            Some("toolcall") => {
                let args = &value["function"]["arguments"];
                ["path", "command"]
                    .iter()
                    .filter_map(|key| args.get(*key))
                    .map(readable_text)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Some("text") => object.get("text").map(readable_text).unwrap_or_default(),
            Some("json") => object.get("value").map(readable_text).unwrap_or_default(),
            Some("toolresult") => value["content"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|part| {
                    // BONE stores structured tool evidence in native text parts.
                    // Decode only tool results; a user's literal JSON stays verbatim.
                    if part["type"] == "text"
                        && let Some(text) = part["text"].as_str()
                        && let Ok(parsed) = serde_json::from_str::<Value>(text)
                    {
                        return readable_text(&parsed);
                    }
                    readable_text(part)
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            Some(_) => object.get("content").map(readable_text).unwrap_or_default(),
            None => [
                "text",
                "stdout",
                "stderr",
                "error",
                "reason",
                "question",
                "instruction",
                "observation",
                "note",
                "path",
                "snippet",
                "preview",
                "command",
                "files",
                "matches",
                "results",
                "message",
                "content",
            ]
            .iter()
            .filter_map(|key| object.get(*key))
            .map(readable_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        },
        _ => String::new(),
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
#[path = "../tests/unit/store.rs"]
mod tests;
