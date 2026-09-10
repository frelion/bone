use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bone_core::{
    DurableCommit, DurableError, DurablePort, DurableReceipt, DurableRestore, DurableSnapshot,
    ExternalEffect, PortFuture, Record, ToolOutcome,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CallRef, ConfigChange, ConfigScope, HistoryEntry, HistoryPage, InputId, InputState, InputView,
    JobRef, Profile, QuestionId, RequestId, RuntimeConfig, RuntimeId, RuntimeOverrides,
    RuntimeSettings, SessionEvent, SessionId, SessionInfo, SessionSeq, SubmissionReceipt,
    SubmitInput, UnresolvedWriteStatus, UnresolvedWriteView, WorkspaceId, WorkspaceInfo,
    WriteResolution,
    storage::{
        BoneStore, DocumentKey, JournalKey, Lease, LeaseKey, Revision, StoreError, StoreRoots,
        WriteTransaction,
    },
};

const NAMESPACE: &str = "app";

#[derive(Clone, Serialize, Deserialize)]
struct StoredCoreState {
    revision: u64,
    snapshot: DurableSnapshot,
    records: Vec<Arc<Record>>,
    receipts: BTreeMap<String, StoredCoreReceipt>,
    call_origins: BTreeMap<u64, RuntimeId>,
    record_origins: BTreeMap<u64, RuntimeId>,
}

// The snapshot itself, as well as history and receipts, can exceed a document's
// size limit. Read and replace all chunks under one SQLite transaction.
const CORE_CHUNK_BYTES: usize = 512 * 1024;

#[derive(Serialize, Deserialize)]
struct CoreDocument {
    version: u32,
    chunks: usize,
    byte_len: usize,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredCoreReceipt {
    digest: String,
    receipt: DurableReceipt,
}

struct SessionDurablePort {
    store: DataStore,
    session: SessionId,
    runtime: RuntimeId,
    lease: Arc<Lease>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl DurablePort for SessionDurablePort {
    fn commit(&self, commit: DurableCommit) -> PortFuture<Result<DurableReceipt, DurableError>> {
        let store = self.store.clone();
        let session = self.session;
        let runtime = self.runtime;
        let lease = Arc::clone(&self.lease);
        let guard = Arc::clone(&self.gate).try_lock_owned();
        Box::pin(async move {
            let guard = guard.map_err(|_| {
                DurableError::Storage("previous Core commit is still pending".into())
            })?;
            // Keep the transaction and acknowledgement proof together: cancelling
            // this future must not cancel a SQLite commit halfway through.
            match tokio::task::spawn_blocking(move || {
                let _guard = guard;
                let _lease = lease;
                store.commit_core_in_runtime(session, runtime, commit)
            })
            .await
            {
                Ok(result) => result,
                Err(error) => panic!("durable transaction worker failed: {error}"),
            }
        })
    }
}

#[derive(Clone)]
pub(crate) struct DataStore {
    store: BoneStore,
    #[cfg(test)]
    faults: Arc<TestFaults>,
}

#[cfg(test)]
const NO_AGENT_RECORD_FAILURE: usize = usize::MAX;

#[cfg(test)]
struct TestFaults {
    agent_record_saves_before_failure: AtomicUsize,
    replayed_agent_record_saves: AtomicUsize,
    runtime_reconfigure_failure: AtomicBool,
    pause_core_commits: AtomicBool,
    core_commit_waiting: AtomicBool,
    fail_core_chunk: AtomicBool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct WorkspaceCatalog {
    items: Vec<WorkspaceInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedSession {
    pub info: SessionInfo,
    pub next_input: u64,
    pub runtime: Option<SavedRuntime>,
    pub agent_through: u64,
    pub core_through: u64,
    pub draft: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedRuntime {
    pub id: RuntimeId,
    pub config: RuntimeConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredEvent {
    App(SessionEvent),
    Agent { runtime: RuntimeId, record: Record },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RequestRecord {
    input: InputId,
    text: String,
    reply_to: Option<QuestionId>,
    saved_at: SessionSeq,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WriteAttempt {
    session: SessionId,
    call: CallRef,
    job: Option<JobRef>,
    tool: String,
    arguments: serde_json::Value,
    state: WriteState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum WriteState {
    Pending,
    Finished(ToolOutcome),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredWriteResolution {
    session: SessionId,
    external_effect: ExternalEffect,
}

pub(crate) enum ResolveWriteResult {
    Applied,
    AlreadyResolved(ExternalEffect),
    Unchanged,
    Conflicts(ExternalEffect),
}

impl DataStore {
    fn read_core_in(
        &self,
        transaction: &WriteTransaction<'_, '_>,
        session: SessionId,
    ) -> Result<(Option<StoredCoreState>, Revision, usize), StoreError> {
        let document = self
            .store
            .document::<CoreDocument>(DocumentKey::new(NAMESPACE, format!("core/{session}")));
        let saved = transaction.read(&document)?;
        match saved.value {
            None => Ok((None, saved.revision, 0)),
            Some(CoreDocument {
                version,
                chunks,
                byte_len,
            }) => {
                if version != 1 || chunks == 0 || chunks > byte_len {
                    return Err(StoreError::Corrupt {
                        message: "invalid Core chunk manifest",
                    });
                }
                let mut bytes = Vec::new();
                for index in 0..chunks {
                    let chunk = self.store.document::<String>(DocumentKey::new(
                        NAMESPACE,
                        format!("core-chunks/{session}/{index}"),
                    ));
                    let chunk_bytes =
                        transaction.read(&chunk)?.value.ok_or(StoreError::Corrupt {
                            message: "missing Core state chunk",
                        })?;
                    if chunk_bytes.is_empty() || chunk_bytes.len() > CORE_CHUNK_BYTES {
                        return Err(StoreError::Corrupt {
                            message: "invalid Core chunk length",
                        });
                    }
                    bytes.extend_from_slice(chunk_bytes.as_bytes());
                }
                if bytes.len() != byte_len {
                    return Err(StoreError::Corrupt {
                        message: "invalid Core state length",
                    });
                }
                let state = serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt {
                    message: "invalid Core state chunks",
                })?;
                Ok((Some(state), saved.revision, chunks))
            }
        }
    }

    fn read_core(&self, session: SessionId) -> Result<Option<StoredCoreState>, StoreError> {
        self.store.read_transaction(|transaction| {
            self.read_core_in(transaction, session)
                .map(|(state, _, _)| state)
        })
    }

    pub fn core_projection_through(&self, session: SessionId) -> Result<u64, StoreError> {
        let saved = self.session(session)?.ok_or(StoreError::Corrupt {
            message: "session does not exist",
        })?;
        Ok(saved.core_through)
    }

    pub fn core_record_origin(
        &self,
        session: SessionId,
        seq: u64,
    ) -> Result<Option<RuntimeId>, StoreError> {
        Ok(self
            .store
            .document::<RuntimeId>(DocumentKey::new(
                NAMESPACE,
                format!("core-record/{session}/{seq}"),
            ))
            .read()?
            .value)
    }

    pub fn core_durable_port(
        &self,
        session: SessionId,
        runtime: RuntimeId,
        lease: Arc<Lease>,
        gate: Arc<tokio::sync::Mutex<()>>,
    ) -> Arc<dyn DurablePort> {
        Arc::new(SessionDurablePort {
            store: self.clone(),
            session,
            runtime,
            lease,
            gate,
        })
    }

    pub fn load_core_restore(
        &self,
        session: SessionId,
    ) -> Result<Option<DurableRestore>, StoreError> {
        Ok(self.read_core(session)?.map(|saved| DurableRestore {
            revision: saved.revision,
            snapshot: saved.snapshot,
            records: saved.records,
        }))
    }

    #[cfg(test)]
    pub fn commit_core(
        &self,
        session: SessionId,
        commit: DurableCommit,
    ) -> Result<DurableReceipt, DurableError> {
        self.commit_core_in_runtime(session, RuntimeId::from_uuid(uuid::Uuid::nil()), commit)
    }

    fn commit_core_in_runtime(
        &self,
        session: SessionId,
        runtime: RuntimeId,
        commit: DurableCommit,
    ) -> Result<DurableReceipt, DurableError> {
        #[cfg(test)]
        if self.faults.pause_core_commits.load(Ordering::Acquire) {
            self.faults
                .core_commit_waiting
                .store(true, Ordering::Release);
            while self.faults.pause_core_commits.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.faults
                .core_commit_waiting
                .store(false, Ordering::Release);
        }
        if commit.commit_id.is_empty() {
            return Err(DurableError::Invalid("empty commit identity".into()));
        }
        let encoded = serde_json::to_vec(&(runtime, &commit))
            .map_err(|error| DurableError::Invalid(error.to_string()))?;
        let digest = format!("{:x}", Sha256::digest(encoded));
        let document = self
            .store
            .document::<CoreDocument>(DocumentKey::new(NAMESPACE, format!("core/{session}")));
        let mut mutation_started = false;
        let result = self.store.transaction(|transaction| {
            let (current, document_revision, old_chunks) = self.read_core_in(transaction, session)?;
            if let Some(prior) = current
                .as_ref()
                .and_then(|state| state.receipts.get(&commit.commit_id))
            {
                return Ok(if prior.digest == digest {
                    Ok(prior.receipt.clone())
                } else {
                    Err(DurableError::Invalid(
                        "commit identity reused with different content".into(),
                    ))
                });
            }
            let revision = current.as_ref().map_or(0, |state| state.revision);
            if revision != commit.expected_revision {
                return Ok(Err(DurableError::Conflict));
            }
            let Some(next_revision) = revision.checked_add(1) else {
                return Ok(Err(DurableError::Invalid("revision exhausted".into())));
            };
            let mut records = current
                .as_ref()
                .map_or_else(Vec::new, |state| state.records.clone());
            let through = records.last().map_or(0, |record| record.seq.0);
            if records.len() as u64 != through
                || commit.records.iter().enumerate().any(|(index, record)| {
                    Some(record.seq.0) != through.checked_add(index as u64 + 1)
                })
                || through.checked_add(commit.records.len() as u64)
                    != Some(commit.snapshot.through().0)
            {
                return Ok(Err(DurableError::Invalid(
                    "non-contiguous core records".into(),
                )));
            }
            let mut call_origins = current
                .as_ref()
                .map_or_else(BTreeMap::new, |state| state.call_origins.clone());
            for record in &commit.records {
                if let bone_core::RecordBody::CallStarted { call, .. } = &record.body
                {
                    if call_origins.contains_key(&call.0)
                        || records.iter().any(|record| {
                            matches!(&record.body, bone_core::RecordBody::CallStarted { call: prior, .. } if prior == call)
                        })
                    {
                        return Ok(Err(DurableError::Invalid(
                            "Core call identity was started twice".into(),
                        )));
                    }
                    call_origins.insert(call.0, runtime);
                }
            }
            let mut record_origins = current.as_ref().map_or_else(BTreeMap::new, |state| state.record_origins.clone());
            for record in &commit.records {
                record_origins.insert(record.seq.0, runtime);
            }
            records.extend(commit.records.iter().cloned());
            let receipt = DurableReceipt {
                commit_id: commit.commit_id.clone(),
                revision: next_revision,
                through: commit.snapshot.through(),
            };
            let mut receipts = current
                .map_or_else(BTreeMap::new, |state| state.receipts);
            receipts.insert(
                commit.commit_id.clone(),
                StoredCoreReceipt {
                    digest: digest.clone(),
                    receipt: receipt.clone(),
                },
            );
            let next = StoredCoreState {
                revision: next_revision,
                snapshot: commit.snapshot.clone(),
                records,
                receipts,
                call_origins,
                record_origins,
            };
            let bytes = serde_json::to_string(&next).map_err(|_| StoreError::Corrupt { message: "cannot encode Core state" })?;
            mutation_started = true;
            for record in &commit.records {
                if let bone_core::RecordBody::CallStarted { call, .. } = &record.body {
                    let origin = self.store.document::<RuntimeId>(DocumentKey::new(NAMESPACE, format!("core-call/{session}/{}", call.0)));
                    let revision = transaction.read(&origin)?.revision;
                    transaction.replace(&origin, &runtime, revision)?;
                }
                let origin = self.store.document::<RuntimeId>(DocumentKey::new(NAMESPACE, format!("core-record/{session}/{}", record.seq.0)));
                let revision = transaction.read(&origin)?.revision;
                transaction.replace(&origin, &runtime, revision)?;
            }
            let mut offset = 0;
            let mut chunks = 0;
            while offset < bytes.len() {
                let mut end = (offset + CORE_CHUNK_BYTES).min(bytes.len());
                while !bytes.is_char_boundary(end) { end -= 1; }
                let chunk = self.store.document::<String>(DocumentKey::new(NAMESPACE, format!("core-chunks/{session}/{chunks}")));
                let revision = transaction.read(&chunk)?.revision;
                transaction.replace(&chunk, &bytes[offset..end].to_owned(), revision)?;
                #[cfg(test)]
                if self.faults.fail_core_chunk.load(Ordering::Acquire) {
                    return Err(StoreError::Corrupt { message: "injected failure after Core chunk write" });
                }
                offset = end;
                chunks += 1;
            }
            for index in chunks..old_chunks {
                let chunk = self.store.document::<String>(DocumentKey::new(NAMESPACE, format!("core-chunks/{session}/{index}")));
                let revision = transaction.read(&chunk)?.revision;
                transaction.delete(&chunk, revision)?;
            }
            transaction.replace(&document, &CoreDocument { version: 1, chunks, byte_len: bytes.len() }, document_revision)?;
            Ok(Ok(receipt))
        });
        match result {
            Ok(result) => result,
            Err(error) if !mutation_started => Err(DurableError::Storage(error.to_string())),
            Err(error) => {
                // A commit I/O error can have an uncertain acknowledgement.
                // Do not report rejection until an authoritative read resolves it.
                loop {
                    match self.read_core(session) {
                        Ok(saved) => {
                            return match saved
                                .and_then(|state| state.receipts.get(&commit.commit_id).cloned())
                            {
                                Some(prior) if prior.digest == digest => Ok(prior.receipt),
                                Some(_) => Err(DurableError::Invalid(
                                    "commit identity reused with different content".into(),
                                )),
                                None => Err(DurableError::Storage(error.to_string())),
                            };
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
                    }
                }
            }
        }
    }

    /// Call numbers are session-scoped; only their committed origin can address
    /// the workspace write ledger.
    pub fn core_call_origin(
        &self,
        session: SessionId,
        call: bone_core::CallId,
    ) -> Result<Option<CallRef>, StoreError> {
        let runtime = self
            .store
            .document::<RuntimeId>(DocumentKey::new(
                NAMESPACE,
                format!("core-call/{session}/{}", call.0),
            ))
            .read()?
            .value;
        Ok(runtime.map(|runtime| CallRef {
            runtime,
            id: call.0,
        }))
    }

    pub fn resolved_write_effect(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
        call: CallRef,
    ) -> Result<Option<ExternalEffect>, StoreError> {
        Ok(self
            .store
            .document::<StoredWriteResolution>(write_resolution_key(workspace, call))
            .read()?
            .value
            .filter(|saved| saved.session == session)
            .map(|saved| saved.external_effect))
    }

    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let roots = StoreRoots::new(data_dir.into())?;
        Ok(Self {
            store: BoneStore::open_at(roots)?,
            #[cfg(test)]
            faults: Arc::new(TestFaults {
                agent_record_saves_before_failure: AtomicUsize::new(NO_AGENT_RECORD_FAILURE),
                replayed_agent_record_saves: AtomicUsize::new(0),
                runtime_reconfigure_failure: AtomicBool::new(false),
                pause_core_commits: AtomicBool::new(false),
                core_commit_waiting: AtomicBool::new(false),
                fail_core_chunk: AtomicBool::new(false),
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn pause_core_commits(&self, pause: bool) {
        self.faults
            .pause_core_commits
            .store(pause, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn core_commit_waiting(&self) -> bool {
        self.faults.core_commit_waiting.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn fail_runtime_reconfigure(&self, fail: bool) {
        self.faults
            .runtime_reconfigure_failure
            .store(fail, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn fail_agent_record_saves_after(&self, successful_saves: usize) {
        self.faults
            .agent_record_saves_before_failure
            .store(successful_saves, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn clear_agent_record_save_failure(&self) {
        self.faults
            .agent_record_saves_before_failure
            .store(NO_AGENT_RECORD_FAILURE, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn replayed_agent_record_saves(&self) -> usize {
        self.faults
            .replayed_agent_record_saves
            .load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn check_agent_record_save(&self) -> Result<(), StoreError> {
        let remaining = &self.faults.agent_record_saves_before_failure;
        let current = remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if current == 0 || current == NO_AGENT_RECORD_FAILURE {
                    None
                } else {
                    Some(current - 1)
                }
            })
            .unwrap_or_else(|current| current);
        if current == 0 {
            Err(StoreError::Corrupt {
                message: "injected agent record save failure",
            })
        } else {
            Ok(())
        }
    }

    pub fn workspace(&self, root: &Path) -> Result<WorkspaceInfo, StoreError> {
        let root = root
            .canonicalize()
            .map_err(|error| StoreError::io("resolve workspace", root, error))?;
        let document = self.store.document::<WorkspaceCatalog>(catalog_key());
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut catalog = snapshot.value.unwrap_or_default();
            if let Some(workspace) = catalog.items.iter().find(|item| item.root == root) {
                return Ok(workspace.clone());
            }
            let workspace = WorkspaceInfo {
                id: WorkspaceId::new(),
                root,
            };
            catalog.items.push(workspace.clone());
            transaction.replace(&document, &catalog, snapshot.revision)?;
            Ok(workspace)
        })
    }

    pub fn workspace_by_id(&self, id: WorkspaceId) -> Result<Option<WorkspaceInfo>, StoreError> {
        Ok(self
            .store
            .document::<WorkspaceCatalog>(catalog_key())
            .read()?
            .value
            .unwrap_or_default()
            .items
            .into_iter()
            .find(|workspace| workspace.id == id))
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceInfo>, StoreError> {
        Ok(self
            .store
            .document::<WorkspaceCatalog>(catalog_key())
            .read()?
            .value
            .unwrap_or_default()
            .items)
    }

    pub fn create_session(
        &self,
        workspace: WorkspaceId,
        title: String,
    ) -> Result<SavedSession, StoreError> {
        let session = SavedSession {
            info: SessionInfo {
                id: SessionId::new(),
                workspace,
                title,
                archived: false,
            },
            next_input: 1,
            runtime: None,
            agent_through: 0,
            core_through: 0,
            draft: String::new(),
        };
        self.store
            .document(session_key(session.info.id))
            .replace(&session, Revision::default())?;
        Ok(session)
    }

    pub fn session(&self, id: SessionId) -> Result<Option<SavedSession>, StoreError> {
        self.store
            .document(session_key(id))
            .read()
            .map(|snapshot| snapshot.value)
    }

    pub fn sessions(&self, workspace: WorkspaceId) -> Result<Vec<SessionInfo>, StoreError> {
        let mut sessions = Vec::new();
        for entry in self
            .store
            .list_documents::<SavedSession>(NAMESPACE, "session/")?
        {
            let Some(session) = entry.snapshot?.value else {
                continue;
            };
            if session.info.workspace == workspace {
                sessions.push(session.info);
            }
        }
        sessions.sort_by_key(|session| session.id);
        Ok(sessions)
    }

    pub fn claim_session(&self, id: SessionId) -> Result<Lease, StoreError> {
        self.store
            .try_acquire_lease(LeaseKey::new(format!("session-{id}")))
    }

    pub fn inputs(&self, session: SessionId) -> Result<Vec<InputView>, StoreError> {
        let mut inputs = Vec::new();
        for entry in self
            .store
            .list_documents::<InputView>(NAMESPACE, &input_prefix(session))?
        {
            if let Some(input) = entry.snapshot?.value {
                inputs.push(input);
            }
        }
        inputs.sort_by_key(|input| input.id);
        Ok(inputs)
    }

    pub fn input(
        &self,
        session: SessionId,
        input: InputId,
    ) -> Result<Option<InputView>, StoreError> {
        self.store
            .document::<InputView>(input_key(session, input))
            .read()
            .map(|snapshot| snapshot.value)
    }

    pub fn accept_input(
        &self,
        session: SessionId,
        input: &SubmitInput,
    ) -> Result<(SubmissionReceipt, InputView), AcceptError> {
        let session_document = self.store.document::<SavedSession>(session_key(session));
        let request_document = self
            .store
            .document::<RequestRecord>(request_key(session, input.request_id));
        if let Some(existing) = request_document.read()?.value {
            if existing.text != input.text || existing.reply_to != input.reply_to {
                return Err(AcceptError::Conflict);
            }
            let saved = self
                .store
                .document::<InputView>(input_key(session, existing.input))
                .read()?
                .value
                .ok_or_else(|| {
                    AcceptError::Store(StoreError::Corrupt {
                        message: "request index points to a missing input",
                    })
                })?;
            return Ok((
                SubmissionReceipt {
                    input: existing.input,
                    saved_at: existing.saved_at,
                },
                saved,
            ));
        }
        if self.session(session)?.is_none() {
            return Err(AcceptError::NotFound);
        }
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store
            .transaction(|transaction| {
                let request = transaction.read(&request_document)?;
                let session_snapshot = transaction.read(&session_document)?;
                let mut saved_session = session_snapshot
                    .value
                    .expect("session existence checked while holding its process lease");
                let id = InputId(saved_session.next_input);
                saved_session.next_input = saved_session
                    .next_input
                    .checked_add(1)
                    .ok_or(StoreError::RevisionExhausted)?;
                let event = SessionEvent::InputSubmitted {
                    input: id,
                    request_id: input.request_id,
                    text: input.text.clone(),
                    reply_to: input.reply_to,
                };
                let append = transaction.append(&journal, &StoredEvent::App(event))?;
                let saved_at = SessionSeq(append.sequence);
                let saved = InputView {
                    id,
                    request_id: input.request_id,
                    text: input.text.clone(),
                    reply_to: input.reply_to,
                    state: InputState::Queued { problem: None },
                };
                transaction.replace(
                    &self.store.document::<InputView>(input_key(session, id)),
                    &saved,
                    Revision::default(),
                )?;
                transaction.replace(
                    &request_document,
                    &RequestRecord {
                        input: id,
                        text: input.text.clone(),
                        reply_to: input.reply_to,
                        saved_at,
                    },
                    request.revision,
                )?;
                transaction.replace(
                    &session_document,
                    &saved_session,
                    session_snapshot.revision,
                )?;
                Ok((
                    SubmissionReceipt {
                        input: id,
                        saved_at,
                    },
                    saved,
                ))
            })
            .map_err(AcceptError::Store)
    }

    pub fn update_input(
        &self,
        session: SessionId,
        id: InputId,
        state: InputState,
        event: Option<SessionEvent>,
    ) -> Result<(InputView, Option<SessionSeq>), StoreError> {
        let document = self.store.document::<InputView>(input_key(session, id));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut input = snapshot.value.ok_or(StoreError::Corrupt {
                message: "input does not exist",
            })?;
            input.state = state;
            let sequence = event
                .map(|event| transaction.append(&journal, &StoredEvent::App(event)))
                .transpose()?
                .map(|append| SessionSeq(append.sequence));
            transaction.replace(&document, &input, snapshot.revision)?;
            Ok((input, sequence))
        })
    }

    pub fn accept_inputs(
        &self,
        session: SessionId,
        inputs: &[InputId],
        runtime: RuntimeId,
    ) -> Result<Vec<InputView>, StoreError> {
        self.store.transaction(|transaction| {
            let mut updated = Vec::with_capacity(inputs.len());
            for id in inputs {
                let document = self.store.document::<InputView>(input_key(session, *id));
                let snapshot = transaction.read(&document)?;
                let mut input = snapshot.value.ok_or(StoreError::Corrupt {
                    message: "input does not exist",
                })?;
                input.state = InputState::Accepted { runtime };
                transaction.replace(&document, &input, snapshot.revision)?;
                updated.push(input);
            }
            Ok(updated)
        })
    }

    pub fn cancel_inputs(&self, session: SessionId, inputs: &[InputId]) -> Result<(), StoreError> {
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            for id in inputs {
                let document = self.store.document::<InputView>(input_key(session, *id));
                let snapshot = transaction.read(&document)?;
                let mut input = snapshot.value.ok_or(StoreError::Corrupt {
                    message: "input does not exist",
                })?;
                input.state = InputState::Cancelled;
                transaction.replace(&document, &input, snapshot.revision)?;
                transaction.append(
                    &journal,
                    &StoredEvent::App(SessionEvent::InputCancelled { input: *id }),
                )?;
            }
            Ok(())
        })
    }

    pub fn start_runtime(
        &self,
        session: SessionId,
        runtime: SavedRuntime,
        agent_through: u64,
    ) -> Result<SessionSeq, StoreError> {
        self.update_session_with_event(
            session,
            |saved| {
                saved.runtime = Some(runtime.clone());
                saved.agent_through = agent_through;
                Ok(())
            },
            SessionEvent::RuntimeStarted {
                runtime: runtime.id,
                config: Box::new(runtime.config.clone()),
            },
        )
    }

    pub fn reconfigure_runtime(
        &self,
        session: SessionId,
        runtime: RuntimeId,
        config: RuntimeConfig,
    ) -> Result<SessionSeq, StoreError> {
        #[cfg(test)]
        if self
            .faults
            .runtime_reconfigure_failure
            .load(Ordering::Acquire)
        {
            return Err(StoreError::Corrupt {
                message: "injected runtime reconfigure failure",
            });
        }
        self.update_session_with_event(
            session,
            |saved| {
                let current = saved
                    .runtime
                    .as_mut()
                    .filter(|current| current.id == runtime)
                    .ok_or(StoreError::Corrupt {
                        message: "runtime is not current",
                    })?;
                current.config = config.clone();
                Ok(())
            },
            SessionEvent::RuntimeReconfigured {
                runtime,
                config: Box::new(config.clone()),
            },
        )
    }

    pub fn close_runtime(
        &self,
        session: SessionId,
        runtime: RuntimeId,
    ) -> Result<SessionSeq, StoreError> {
        self.update_session_with_event(
            session,
            |saved| {
                if saved
                    .runtime
                    .as_ref()
                    .is_some_and(|item| item.id == runtime)
                {
                    saved.runtime = None;
                    saved.agent_through = 0;
                }
                Ok(())
            },
            SessionEvent::RuntimeClosed { runtime },
        )
    }

    pub fn save_agent_record(
        &self,
        session: SessionId,
        runtime: RuntimeId,
        record: &Record,
        changes: &[(InputId, InputState)],
    ) -> Result<Option<SessionSeq>, StoreError> {
        #[cfg(test)]
        self.check_agent_record_save()?;

        let origin =
            self.core_record_origin(session, record.seq.0)?
                .ok_or(StoreError::Corrupt {
                    message: "Core record has no runtime provenance",
                })?;
        if origin != runtime {
            return Err(StoreError::Corrupt {
                message: "Core record runtime does not match provenance",
            });
        }
        let write_origin = match &record.body {
            bone_core::RecordBody::ToolFinished { call, .. } => Some(
                self.core_call_origin(session, *call)?
                    .ok_or(StoreError::Corrupt {
                        message: "Core call has no runtime provenance",
                    })?,
            ),
            _ => None,
        };

        let session_document = self.store.document::<SavedSession>(session_key(session));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&session_document)?;
            let mut saved = snapshot.value.ok_or(StoreError::Corrupt {
                message: "session does not exist",
            })?;
            let through = saved.core_through;
            if record.seq.0 <= through {
                #[cfg(test)]
                self.faults
                    .replayed_agent_record_saves
                    .fetch_add(1, Ordering::AcqRel);
                return Ok(None);
            }
            if record.seq.0 != through + 1 {
                return Err(StoreError::Corrupt {
                    message: "agent record sequence is not contiguous",
                });
            }
            for (id, state) in changes {
                let document = self.store.document::<InputView>(input_key(session, *id));
                let input_snapshot = transaction.read(&document)?;
                let mut input = input_snapshot.value.ok_or(StoreError::Corrupt {
                    message: "input does not exist",
                })?;
                input.state = state.clone();
                transaction.replace(&document, &input, input_snapshot.revision)?;
            }
            if let bone_core::RecordBody::ToolFinished { outcome, .. } = &record.body
                && outcome.external_effect != ExternalEffect::Unknown
                && let Some(origin) = write_origin
            {
                let write_document = self
                    .store
                    .document::<WriteAttempt>(write_key(saved.info.workspace, origin));
                let write_snapshot = transaction.read(&write_document)?;
                if let Some(attempt) = write_snapshot.value
                    && matches!(&attempt.state, WriteState::Finished(_))
                {
                    transaction.delete(&write_document, write_snapshot.revision)?;
                }
            }
            let append = transaction.append(
                &journal,
                &StoredEvent::Agent {
                    runtime,
                    record: record.clone(),
                },
            )?;
            saved.agent_through = record.seq.0;
            saved.core_through = record.seq.0;
            transaction.replace(&session_document, &saved, snapshot.revision)?;
            Ok(Some(SessionSeq(append.sequence)))
        })
    }

    pub fn rename_session(&self, id: SessionId, title: String) -> Result<SessionInfo, StoreError> {
        self.update_session(id, |saved| saved.info.title = title)
            .map(|saved| saved.info)
    }

    pub fn save_draft(&self, id: SessionId, draft: String) -> Result<(), StoreError> {
        self.update_session(id, |saved| saved.draft = draft)
            .map(drop)
    }

    pub fn archive_session(
        &self,
        id: SessionId,
        archived: bool,
    ) -> Result<SessionInfo, StoreError> {
        self.update_session(id, |saved| saved.info.archived = archived)
            .map(|saved| saved.info)
    }

    /// Close a runtime that disappeared with its process. Inputs proven to have
    /// entered that runtime become interrupted; untouched queued inputs remain
    /// available for an explicit retry.
    pub fn recover_session(
        &self,
        session: SessionId,
    ) -> Result<(SavedSession, Vec<InputView>), StoreError> {
        let Some(current) = self.session(session)? else {
            return Err(StoreError::Corrupt {
                message: "session does not exist",
            });
        };
        self.reconcile_core_projection(session, &current)?;
        let Some(runtime) = current.runtime.clone() else {
            return Ok((
                self.session(session)?.expect("session retained"),
                self.inputs(session)?,
            ));
        };
        if self.session(session)?.is_none() {
            return Err(StoreError::Corrupt {
                message: "session disappeared while reconciling Core state",
            });
        }
        let inputs = self.inputs(session)?;
        let interrupted = inputs
            .iter()
            .filter(|input| {
                matches!(
                    input.state,
                    InputState::Posting { runtime: id }
                        | InputState::Accepted { runtime: id }
                        | InputState::WaitingForUser { runtime: id, .. }
                        | InputState::RoutingFailed { runtime: id, .. }
                        if id == runtime.id
                )
            })
            .map(|input| input.id)
            .collect::<Vec<_>>();
        let document = self.store.document::<SavedSession>(session_key(session));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut saved = snapshot.value.ok_or(StoreError::Corrupt {
                message: "session does not exist",
            })?;
            for id in &interrupted {
                let input_document = self.store.document::<InputView>(input_key(session, *id));
                let input_snapshot = transaction.read(&input_document)?;
                let mut input = input_snapshot.value.ok_or(StoreError::Corrupt {
                    message: "input does not exist",
                })?;
                input.state = InputState::Interrupted {
                    runtime: runtime.id,
                };
                transaction.replace(&input_document, &input, input_snapshot.revision)?;
            }
            if !interrupted.is_empty() {
                transaction.append(
                    &journal,
                    &StoredEvent::App(SessionEvent::Interrupted {
                        runtime: runtime.id,
                        inputs: interrupted.clone(),
                    }),
                )?;
            }
            transaction.append(
                &journal,
                &StoredEvent::App(SessionEvent::RuntimeClosed {
                    runtime: runtime.id,
                }),
            )?;
            saved.runtime = None;
            saved.agent_through = 0;
            transaction.replace(&document, &saved, snapshot.revision)?;
            Ok(saved)
        })?;
        Ok((
            self.session(session)?.expect("session was retained"),
            self.inputs(session)?,
        ))
    }

    fn reconcile_core_projection(
        &self,
        session: SessionId,
        saved: &SavedSession,
    ) -> Result<(), StoreError> {
        let Some(core) = self.read_core(session)? else {
            return Ok(());
        };
        let mut inputs = self
            .inputs(session)?
            .into_iter()
            .map(|input| (input.id, input))
            .collect::<BTreeMap<_, _>>();
        for record in core
            .records
            .iter()
            .filter(|record| record.seq.0 > saved.core_through)
        {
            let runtime =
                core.record_origins
                    .get(&record.seq.0)
                    .copied()
                    .ok_or(StoreError::Corrupt {
                        message: "Core projection record has no runtime provenance",
                    })?;
            let changes = crate::session::input_changes(runtime, record, &inputs);
            self.save_agent_record(session, runtime, record, &changes)?;
            for (id, state) in changes {
                if let Some(input) = inputs.get_mut(&id) {
                    input.state = state;
                }
            }
            if let bone_core::RecordBody::ToolFinished { call, job, .. } = &record.body
                && let Some(origin) = self.core_call_origin(session, *call)?
            {
                self.attach_write_job(
                    saved.info.workspace,
                    origin,
                    JobRef {
                        runtime: origin.runtime,
                        id: job.0,
                    },
                )?;
            }
        }
        Ok(())
    }

    pub fn begin_write(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
        call: CallRef,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<bool, StoreError> {
        let document = self
            .store
            .document::<WriteAttempt>(write_key(workspace, call));
        let resolution = self
            .store
            .document::<StoredWriteResolution>(write_resolution_key(workspace, call));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            if snapshot.value.is_some() || transaction.read(&resolution)?.value.is_some() {
                return Ok(false);
            }
            transaction.replace(
                &document,
                &WriteAttempt {
                    session,
                    call,
                    job: None,
                    tool: tool.to_owned(),
                    arguments,
                    state: WriteState::Pending,
                },
                snapshot.revision,
            )?;
            Ok(true)
        })
    }

    pub fn finish_write(
        &self,
        workspace: WorkspaceId,
        call: CallRef,
        outcome: ToolOutcome,
    ) -> Result<(), StoreError> {
        let document = self
            .store
            .document::<WriteAttempt>(write_key(workspace, call));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut attempt = snapshot.value.ok_or(StoreError::Corrupt {
                message: "write attempt does not exist",
            })?;
            if !matches!(attempt.state, WriteState::Pending) {
                return Err(StoreError::Corrupt {
                    message: "write attempt was already completed",
                });
            }
            attempt.state = WriteState::Finished(outcome);
            transaction.replace(&document, &attempt, snapshot.revision)?;
            Ok(())
        })
    }

    pub fn finished_write(
        &self,
        workspace: WorkspaceId,
        call: CallRef,
    ) -> Result<Option<ToolOutcome>, StoreError> {
        let state = self
            .store
            .document::<WriteAttempt>(write_key(workspace, call))
            .read()?
            .value
            .map(|attempt| attempt.state);
        Ok(match state {
            Some(WriteState::Finished(outcome))
                if outcome.external_effect != ExternalEffect::Unknown =>
            {
                Some(outcome)
            }
            _ => None,
        })
    }

    pub fn attach_write_job(
        &self,
        workspace: WorkspaceId,
        call: CallRef,
        job: JobRef,
    ) -> Result<(), StoreError> {
        let document = self
            .store
            .document::<WriteAttempt>(write_key(workspace, call));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let Some(mut attempt) = snapshot.value else {
                return Ok(());
            };
            if attempt.job == Some(job) {
                return Ok(());
            }
            attempt.job = Some(job);
            transaction.replace(&document, &attempt, snapshot.revision)?;
            Ok(())
        })
    }

    pub fn unresolved_writes(
        &self,
        workspace: WorkspaceId,
        session: Option<SessionId>,
    ) -> Result<Vec<UnresolvedWriteView>, StoreError> {
        let mut writes = Vec::new();
        for entry in self
            .store
            .list_documents::<WriteAttempt>(NAMESPACE, &write_prefix(workspace))?
        {
            let Some(attempt) = entry.snapshot?.value else {
                continue;
            };
            let (status, outcome) = match &attempt.state {
                WriteState::Pending => (UnresolvedWriteStatus::Pending, None),
                WriteState::Finished(outcome) => {
                    (UnresolvedWriteStatus::Finished, Some(outcome.clone()))
                }
            };
            if session.is_some_and(|id| attempt.session != id) {
                continue;
            }
            writes.push(UnresolvedWriteView {
                workspace,
                session: attempt.session,
                call: attempt.call,
                job: attempt.job,
                status,
                tool: attempt.tool,
                arguments: attempt.arguments,
                outcome,
            });
        }
        writes.sort_by_key(|write| write.call);
        Ok(writes)
    }

    pub fn resolve_write(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
        call: CallRef,
        resolution: WriteResolution,
    ) -> Result<ResolveWriteResult, StoreError> {
        let document = self
            .store
            .document::<WriteAttempt>(write_key(workspace, call));
        let saved_resolution = self
            .store
            .document::<StoredWriteResolution>(write_resolution_key(workspace, call));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let Some(attempt) = snapshot.value else {
                let saved = transaction.read(&saved_resolution)?.value;
                if let Some(saved) = saved.filter(|saved| saved.session == session) {
                    if saved.external_effect == resolution.external_effect {
                        return Ok(ResolveWriteResult::AlreadyResolved(saved.external_effect));
                    }
                    return Ok(ResolveWriteResult::Conflicts(saved.external_effect));
                }
                return Ok(ResolveWriteResult::Unchanged);
            };
            if attempt.session != session {
                return Ok(ResolveWriteResult::Unchanged);
            }
            match &attempt.state {
                WriteState::Pending => {}
                WriteState::Finished(outcome)
                    if outcome.external_effect == ExternalEffect::Unknown
                        || outcome.external_effect == resolution.external_effect => {}
                WriteState::Finished(outcome) => {
                    return Ok(ResolveWriteResult::Conflicts(outcome.external_effect));
                }
            }
            let resolution_snapshot = transaction.read(&saved_resolution)?;
            transaction.replace(
                &saved_resolution,
                &StoredWriteResolution {
                    session,
                    external_effect: resolution.external_effect,
                },
                resolution_snapshot.revision,
            )?;
            transaction.delete(&document, snapshot.revision)?;
            transaction.append(
                &journal,
                &StoredEvent::App(SessionEvent::WriteResolved {
                    call,
                    external_effect: resolution.external_effect,
                    evidence: resolution.evidence,
                }),
            )?;
            Ok(ResolveWriteResult::Applied)
        })
    }

    pub fn history(
        &self,
        session: SessionId,
        after: SessionSeq,
        limit: usize,
    ) -> Result<HistoryPage, StoreError> {
        if after.0 > i64::MAX as u64 {
            return Ok(HistoryPage {
                items: Vec::new(),
                next_cursor: after,
                has_more: false,
            });
        }
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        let page = journal.read_after(after.0, limit.max(1))?;
        let mut items = Vec::new();
        let mut next_cursor = after;
        for entry in page.entries {
            next_cursor = SessionSeq(entry.sequence);
            let event = match entry.event {
                StoredEvent::App(event) => Some(event),
                StoredEvent::Agent { runtime, record } => public_agent_event(runtime, &record),
            };
            if let Some(event) = event {
                items.push(HistoryEntry {
                    sequence: SessionSeq(entry.sequence),
                    occurred_at: entry.occurred_at,
                    event,
                });
            }
        }
        Ok(HistoryPage {
            items,
            next_cursor,
            has_more: page.has_more,
        })
    }

    pub fn history_through(&self, session: SessionId) -> Result<SessionSeq, StoreError> {
        self.store
            .journal::<StoredEvent>(journal_key(session))
            .last_sequence()
            .map(SessionSeq)
    }

    pub fn global_settings(&self) -> Result<RuntimeSettings, StoreError> {
        Ok(self
            .store
            .document::<RuntimeSettings>(global_config_key())
            .read()?
            .value
            .unwrap_or_default())
    }

    pub fn config(&self, scope: ConfigScope) -> Result<RuntimeOverrides, StoreError> {
        match scope {
            ConfigScope::User => {
                let global = self.global_settings()?;
                Ok(RuntimeOverrides {
                    worker: global.worker,
                    coordinator: global.coordinator,
                    limits: Some(global.limits),
                    tools: Some(global.tools),
                })
            }
            _ => Ok(self
                .store
                .document::<RuntimeOverrides>(config_key(scope))
                .read()?
                .value
                .unwrap_or_default()),
        }
    }

    pub fn update_config(
        &self,
        scope: ConfigScope,
        change: ConfigChange,
    ) -> Result<RuntimeOverrides, StoreError> {
        match scope {
            ConfigScope::User => {
                let document = self.store.document::<RuntimeSettings>(global_config_key());
                let next = self.store.transaction(|transaction| {
                    let snapshot = transaction.read(&document)?;
                    let mut settings = snapshot.value.unwrap_or_default();
                    match change {
                        ConfigChange::Worker(value) => settings.worker = value,
                        ConfigChange::Coordinator(value) => settings.coordinator = value,
                        ConfigChange::Limits(value) => settings.limits = value.unwrap_or_default(),
                        ConfigChange::Tools(value) => settings.tools = value.unwrap_or_default(),
                    }
                    transaction.replace(&document, &settings, snapshot.revision)?;
                    Ok(settings)
                })?;
                Ok(RuntimeOverrides {
                    worker: next.worker,
                    coordinator: next.coordinator,
                    limits: Some(next.limits),
                    tools: Some(next.tools),
                })
            }
            _ => {
                let document = self.store.document::<RuntimeOverrides>(config_key(scope));
                let values = self.store.transaction(|transaction| {
                    let snapshot = transaction.read(&document)?;
                    let mut values = snapshot.value.unwrap_or_default();
                    match change {
                        ConfigChange::Worker(value) => values.worker = value,
                        ConfigChange::Coordinator(value) => values.coordinator = value,
                        ConfigChange::Limits(value) => values.limits = value,
                        ConfigChange::Tools(value) => values.tools = value,
                    }
                    transaction.replace(&document, &values, snapshot.revision)?;
                    Ok(values)
                })?;
                Ok(values)
            }
        }
    }

    pub fn profiles(&self) -> Result<Vec<Profile>, StoreError> {
        Ok(self
            .store
            .document::<Vec<Profile>>(profiles_key())
            .read()?
            .value
            .unwrap_or_else(|| vec![Profile::chatgpt()]))
    }

    pub fn save_profile(&self, profile: Profile) -> Result<(), StoreError> {
        let document = self.store.document::<Vec<Profile>>(profiles_key());
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut profiles = snapshot.value.unwrap_or_else(|| vec![Profile::chatgpt()]);
            match profiles.iter_mut().find(|item| item.id == profile.id) {
                Some(current) => *current = profile,
                None => profiles.push(profile),
            }
            transaction.replace(&document, &profiles, snapshot.revision)?;
            Ok(())
        })
    }

    fn update_session_with_event(
        &self,
        session: SessionId,
        update: impl FnOnce(&mut SavedSession) -> Result<(), StoreError>,
        event: SessionEvent,
    ) -> Result<SessionSeq, StoreError> {
        let document = self.store.document::<SavedSession>(session_key(session));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut saved = snapshot.value.ok_or(StoreError::Corrupt {
                message: "session does not exist",
            })?;
            update(&mut saved)?;
            let append = transaction.append(&journal, &StoredEvent::App(event))?;
            transaction.replace(&document, &saved, snapshot.revision)?;
            Ok(SessionSeq(append.sequence))
        })
    }

    fn update_session(
        &self,
        session: SessionId,
        update: impl FnOnce(&mut SavedSession),
    ) -> Result<SavedSession, StoreError> {
        let document = self.store.document::<SavedSession>(session_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut saved = snapshot.value.ok_or(StoreError::Corrupt {
                message: "session does not exist",
            })?;
            update(&mut saved);
            transaction.replace(&document, &saved, snapshot.revision)?;
            Ok(saved)
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AcceptError {
    #[error("session not found")]
    NotFound,
    #[error("request ID was reused with different content")]
    Conflict,
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn public_agent_event(runtime: RuntimeId, record: &Record) -> Option<SessionEvent> {
    use bone_core::RecordBody;
    match &record.body {
        RecordBody::Reply { job, inputs, text } => Some(SessionEvent::Reply {
            job: crate::JobRef { runtime, id: job.0 },
            inputs: inputs.iter().map(|id| InputId(id.0)).collect(),
            text: text.clone(),
        }),
        RecordBody::Clarification { inputs, question } => {
            let reply_to = inputs.first().copied()?;
            Some(SessionEvent::QuestionAsked {
                question: QuestionId {
                    runtime,
                    record: record.seq.0,
                    reply_to: InputId(reply_to.0),
                },
                inputs: inputs.iter().map(|id| InputId(id.0)).collect(),
                text: question.clone(),
            })
        }
        RecordBody::InputRoutingFailed { inputs, message } => Some(SessionEvent::RoutingFailed {
            runtime,
            inputs: inputs.iter().map(|id| InputId(id.0)).collect(),
            message: message.clone(),
        }),
        RecordBody::Outcome { job, outcome } => Some(SessionEvent::JobFinished {
            job: crate::JobRef { runtime, id: job.0 },
            outcome: outcome.kind,
            summary: outcome.completion.summary.clone(),
            remaining: outcome.completion.remaining.clone(),
        }),
        RecordBody::InputFinished { input, outcome } => Some(SessionEvent::InputFinished {
            runtime,
            input: InputId(input.0),
            outcome: outcome.clone(),
        }),
        RecordBody::ToolFinished {
            job,
            call,
            request,
            outcome,
        } => Some(SessionEvent::ToolFinished {
            call: crate::CallRef {
                runtime,
                id: call.0,
            },
            job: crate::JobRef { runtime, id: job.0 },
            tool: request.name.clone(),
            outcome: outcome.as_ref().clone(),
        }),
        _ => None,
    }
}

fn catalog_key() -> DocumentKey {
    DocumentKey::new(NAMESPACE, "workspaces")
}

fn session_key(id: SessionId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("session/{id}"))
}

fn input_prefix(session: SessionId) -> String {
    format!("input/{session}/")
}

fn input_key(session: SessionId, input: InputId) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!("{}{:020}", input_prefix(session), input.0),
    )
}

fn request_key(session: SessionId, request: RequestId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("request/{session}/{request}"))
}

fn journal_key(session: SessionId) -> JournalKey {
    JournalKey::new(format!("app/session/{session}"))
}

fn global_config_key() -> DocumentKey {
    DocumentKey::new(NAMESPACE, "config/user")
}

fn config_key(scope: ConfigScope) -> DocumentKey {
    let key = match scope {
        ConfigScope::User => "config/user".to_owned(),
        ConfigScope::Workspace(id) => format!("config/workspace/{id}"),
        ConfigScope::Session(id) => format!("config/session/{id}"),
    };
    DocumentKey::new(NAMESPACE, key)
}

fn profiles_key() -> DocumentKey {
    DocumentKey::new(NAMESPACE, "profiles")
}

fn write_prefix(workspace: WorkspaceId) -> String {
    format!("write/{workspace}/")
}

fn write_key(workspace: WorkspaceId, call: CallRef) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "{}{}-{:020}",
            write_prefix(workspace),
            call.runtime,
            call.id
        ),
    )
}

fn write_resolution_key(workspace: WorkspaceId, call: CallRef) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "write-resolution/{workspace}/{}-{:020}",
            call.runtime, call.id
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;
    use crate::{ModelSelection, ProfileId};
    use bone_adapters::tools::BashOutput;

    fn core_commit(id: &str, revision: u64, from: u64, through: u64) -> DurableCommit {
        DurableCommit {
            commit_id: id.into(),
            expected_revision: revision,
            snapshot: serde_json::from_value(serde_json::json!({
                "version": 1, "epoch": 0, "through": through, "payload": {}
            }))
            .unwrap(),
            records: (from..=through)
                .map(|seq| {
                    Arc::new(Record {
                        seq: bone_core::Seq(seq),
                        origin: bone_core::Origin::Kernel,
                        body: bone_core::RecordBody::Audit {
                            message: format!("record {seq}"),
                        },
                    })
                })
                .collect(),
        }
    }

    #[test]
    fn core_commit_is_atomic_idempotent_and_session_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let store = DataStore::open(temp.path().join("data")).unwrap();
        let session = SessionId::new();
        let first = core_commit("first", 0, 1, 2);
        let receipt = store.commit_core(session, first.clone()).unwrap();
        assert_eq!(receipt.revision, 1);
        store
            .commit_core(session, core_commit("second", 1, 3, 3))
            .unwrap();
        assert_eq!(store.commit_core(session, first.clone()).unwrap(), receipt);
        let mut changed = first;
        changed.expected_revision = 100;
        assert!(matches!(
            store.commit_core(session, changed),
            Err(DurableError::Invalid(_))
        ));
        assert!(matches!(
            store.commit_core(session, core_commit("conflict", 0, 4, 4)),
            Err(DurableError::Conflict)
        ));
        assert!(matches!(
            store.commit_core(session, core_commit("gap", 2, 5, 5)),
            Err(DurableError::Invalid(_))
        ));
        let restored = store.load_core_restore(session).unwrap().unwrap();
        assert_eq!(restored.revision, 2);
        assert_eq!(restored.records.len(), 3);
        assert_eq!(restored.snapshot.through(), bone_core::Seq(3));
        assert!(store.load_core_restore(SessionId::new()).unwrap().is_none());
        drop(store);
        let reopened = DataStore::open(temp.path().join("data")).unwrap();
        assert_eq!(
            reopened
                .load_core_restore(session)
                .unwrap()
                .unwrap()
                .records
                .len(),
            3
        );
    }

    #[test]
    fn cumulative_core_history_and_snapshot_can_exceed_document_limit() {
        let temp = tempfile::tempdir().unwrap();
        let store = DataStore::open(temp.path().join("data")).unwrap();
        let session = SessionId::new();
        for seq in 1..=2 {
            let mut commit = core_commit(&format!("commit-{seq}"), seq - 1, seq, seq);
            commit.records[0] = Arc::new(Record {
                seq: bone_core::Seq(seq),
                origin: bone_core::Origin::Kernel,
                body: bone_core::RecordBody::Audit {
                    message: "🦴".repeat(5 * 1024 * 1024 / 4),
                },
            });
            let receipt = store.commit_core(session, commit.clone()).unwrap();
            assert_eq!(store.commit_core(session, commit).unwrap(), receipt);
        }
        let mut snapshot = core_commit("snapshot", 2, 3, 2);
        snapshot.snapshot = serde_json::from_value(serde_json::json!({
            "version": 1, "epoch": 0, "through": 2, "payload": { "large": "s".repeat(9 * 1024 * 1024) }
        })).unwrap();
        store.commit_core(session, snapshot).unwrap();
        drop(store);
        let store = DataStore::open(temp.path().join("data")).unwrap();
        let restored = store.load_core_restore(session).unwrap().unwrap();
        assert_eq!(restored.revision, 3);
        assert_eq!(restored.records.len(), 2);
        assert!(serde_json::to_vec(&restored.snapshot).unwrap().len() > 8 * 1024 * 1024);
        store
            .commit_core(session, core_commit("continue", 3, 3, 3))
            .unwrap();
    }

    #[test]
    fn failed_chunk_write_rolls_back_state_and_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let store = DataStore::open(temp.path().join("data")).unwrap();
        let session = SessionId::new();
        store
            .commit_core(session, core_commit("first", 0, 1, 1))
            .unwrap();
        let mut next = core_commit("next", 1, 2, 2);
        next.records[0] = Arc::new(Record {
            seq: bone_core::Seq(2),
            origin: bone_core::Origin::Kernel,
            body: bone_core::RecordBody::Audit {
                message: "x".repeat(2 * CORE_CHUNK_BYTES),
            },
        });
        store.faults.fail_core_chunk.store(true, Ordering::Release);
        assert!(matches!(
            store.commit_core(session, next.clone()),
            Err(DurableError::Storage(_))
        ));
        let restored = store.load_core_restore(session).unwrap().unwrap();
        assert_eq!(restored.revision, 1);
        assert_eq!(restored.records.len(), 1);
        store.faults.fail_core_chunk.store(false, Ordering::Release);
        let receipt = store.commit_core(session, next.clone()).unwrap();
        assert_eq!(receipt.revision, 2);
        assert_eq!(store.commit_core(session, next).unwrap(), receipt);
    }

    #[test]
    fn recovery_commit_before_runtime_registration_is_projected_once() {
        let temp = tempfile::tempdir().unwrap();
        let store = DataStore::open(temp.path().join("data")).unwrap();
        let session = store
            .create_session(WorkspaceId::new(), "startup crash".into())
            .unwrap()
            .info
            .id;
        let original = RuntimeId::new();
        let recovering = RuntimeId::new();
        let (_, input) = store
            .accept_input(session, &SubmitInput::new("unfinished work"))
            .unwrap();
        store
            .update_input(
                session,
                input.id,
                InputState::Interrupted { runtime: original },
                None,
            )
            .unwrap();
        let first = core_commit("original", 0, 1, 1);
        store
            .commit_core_in_runtime(session, original, first.clone())
            .unwrap();
        store
            .save_agent_record(session, original, &first.records[0], &[])
            .unwrap();
        assert_eq!(store.core_projection_through(session).unwrap(), 1);

        // Core has durably finished interrupted work, but App never registered
        // the recovering runtime. Both process loss and cancelled startup leave
        // exactly this persistent state.
        let mut recovery = core_commit("restore", 1, 2, 2);
        recovery.records[0] = Arc::new(Record {
            seq: bone_core::Seq(2),
            origin: bone_core::Origin::Kernel,
            body: bone_core::RecordBody::InputFinished {
                input: bone_core::InputId(input.id.0),
                outcome: bone_core::InputOutcome::Failed,
            },
        });
        store
            .commit_core_in_runtime(session, recovering, recovery)
            .unwrap();
        assert!(store.session(session).unwrap().unwrap().runtime.is_none());
        assert_eq!(store.core_projection_through(session).unwrap(), 1);
        drop(store);

        let store = DataStore::open(temp.path().join("data")).unwrap();
        let (_, inputs) = store.recover_session(session).unwrap();
        assert!(
            matches!(inputs[0].state, InputState::Finished { runtime, outcome: bone_core::InputOutcome::Failed } if runtime == recovering)
        );
        assert_eq!(store.core_projection_through(session).unwrap(), 2);
        store.recover_session(session).unwrap();
        let history = store
            .store
            .journal::<StoredEvent>(journal_key(session))
            .read()
            .unwrap();
        assert_eq!(history.entries.iter().filter(|entry| matches!(&entry.event, StoredEvent::Agent { runtime, record } if *runtime == recovering && record.seq.0 == 2)).count(), 1);
    }

    #[test]
    fn concurrent_core_commit_retries_append_once() {
        let temp = tempfile::tempdir().unwrap();
        let store = DataStore::open(temp.path().join("data")).unwrap();
        let session = SessionId::new();
        let barrier = Arc::new(Barrier::new(3));
        let threads = (0..2)
            .map(|_| {
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store
                        .commit_core(session, core_commit("same", 0, 1, 1))
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let receipts = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(receipts[0], receipts[1]);
        assert_eq!(
            store
                .load_core_restore(session)
                .unwrap()
                .unwrap()
                .records
                .len(),
            1
        );
    }

    #[test]
    fn core_call_provenance_survives_runtime_change_and_rejects_collision() {
        let temp = tempfile::tempdir().unwrap();
        let store = DataStore::open(temp.path().join("data")).unwrap();
        let session = SessionId::new();
        let original = RuntimeId::new();
        let next = RuntimeId::new();
        let mut first = core_commit("first", 0, 1, 1);
        first.records[0] = Arc::new(Record {
            seq: bone_core::Seq(1),
            origin: bone_core::Origin::Kernel,
            body: bone_core::RecordBody::CallStarted {
                call: bone_core::CallId(7),
                kind: bone_core::CallKind::Tool,
                job: None,
                tool: None,
            },
        });
        store
            .commit_core_in_runtime(session, original, first)
            .unwrap();
        store
            .commit_core_in_runtime(session, next, core_commit("restore", 1, 2, 1))
            .unwrap();
        assert_eq!(
            store
                .core_call_origin(session, bone_core::CallId(7))
                .unwrap(),
            Some(CallRef {
                runtime: original,
                id: 7
            })
        );
        let mut collision = core_commit("collision", 2, 2, 2);
        collision.records[0] = Arc::new(Record {
            seq: bone_core::Seq(2),
            origin: bone_core::Origin::Kernel,
            body: bone_core::RecordBody::CallStarted {
                call: bone_core::CallId(7),
                kind: bone_core::CallKind::Tool,
                job: None,
                tool: None,
            },
        });
        assert!(matches!(
            store.commit_core_in_runtime(session, next, collision),
            Err(DurableError::Invalid(_))
        ));
        let document = store
            .store
            .document::<CoreDocument>(DocumentKey::new(NAMESPACE, format!("core/{session}")));
        let saved = document.read().unwrap();
        let mut json = serde_json::to_value(store.read_core(session).unwrap().unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("call_origins");
        assert!(serde_json::from_value::<StoredCoreState>(json).is_err());
        let old_format = store.read_core(session).unwrap().unwrap();
        let raw = store
            .store
            .document::<StoredCoreState>(DocumentKey::new(NAMESPACE, format!("core/{session}")));
        store
            .store
            .transaction(|transaction| {
                transaction.replace(&raw, &old_format, saved.revision)?;
                Ok(())
            })
            .unwrap();
        assert!(store.load_core_restore(session).is_err());
    }

    fn selection(model: &str) -> ModelSelection {
        ModelSelection::new(ProfileId::new("test").unwrap(), model).unwrap()
    }

    fn update_when_available(store: &DataStore, change: ConfigChange) {
        loop {
            match store.update_config(ConfigScope::User, change.clone()) {
                Ok(_) => return,
                Err(StoreError::Busy) => thread::yield_now(),
                Err(error) => panic!("config update failed: {error}"),
            }
        }
    }

    #[test]
    fn concurrent_config_field_updates_do_not_overwrite_each_other() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(DataStore::open(temporary.path().join("data")).unwrap());
        let barrier = Arc::new(Barrier::new(3));
        let worker = selection("worker");
        let coordinator = selection("coordinator");

        let worker_thread = {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let worker = worker.clone();
            thread::spawn(move || {
                barrier.wait();
                update_when_available(&store, ConfigChange::Worker(Some(worker)));
            })
        };
        let coordinator_thread = {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let coordinator = coordinator.clone();
            thread::spawn(move || {
                barrier.wait();
                update_when_available(&store, ConfigChange::Coordinator(Some(coordinator)));
            })
        };

        barrier.wait();
        worker_thread.join().unwrap();
        coordinator_thread.join().unwrap();

        let saved = store.config(ConfigScope::User).unwrap();
        assert_eq!(saved.worker, Some(worker));
        assert_eq!(saved.coordinator, Some(coordinator));
    }

    #[test]
    fn maximum_bash_streams_fit_in_one_write_record() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let store = DataStore::open(temporary.path().join("data")).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let session = store
            .create_session(workspace.id, "session".into())
            .unwrap();
        let call = CallRef {
            runtime: RuntimeId::new(),
            id: 1,
        };
        store
            .begin_write(
                workspace.id,
                session.info.id,
                call,
                "bash",
                serde_json::json!({ "command": "x".repeat(1024 * 1024 - 1024) }),
            )
            .unwrap();
        let stream = "\0".repeat(512 * 1024);
        let output = BashOutput {
            stdout: stream.clone(),
            stderr: stream,
            exit_code: Some(0),
            timed_out: false,
            truncated: false,
        };
        store
            .finish_write(
                workspace.id,
                call,
                ToolOutcome {
                    result: Ok(serde_json::to_value(output).unwrap()),
                    external_effect: ExternalEffect::Applied,
                },
            )
            .unwrap();
    }

    #[test]
    fn resolved_attempts_leave_the_hot_write_prefix() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let store = DataStore::open(temporary.path().join("data")).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let session = store
            .create_session(workspace.id, "session".into())
            .unwrap();
        let call = CallRef {
            runtime: RuntimeId::new(),
            id: 1,
        };
        store
            .begin_write(
                workspace.id,
                session.info.id,
                call,
                "bash",
                serde_json::json!({ "command": "touch marker" }),
            )
            .unwrap();
        store
            .resolve_write(
                workspace.id,
                session.info.id,
                call,
                WriteResolution {
                    external_effect: ExternalEffect::Applied,
                    evidence: "marker exists".into(),
                },
            )
            .unwrap();

        assert!(
            store
                .store
                .list_documents::<WriteAttempt>(NAMESPACE, &write_prefix(workspace.id))
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .store
                .document::<StoredWriteResolution>(write_resolution_key(workspace.id, call))
                .read()
                .unwrap()
                .value
                .is_some()
        );
    }
}
