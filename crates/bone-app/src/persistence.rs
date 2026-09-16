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
    AcceptanceCursor, AcceptanceId, AcceptancePage, AcceptanceReceipt, AcceptanceRecord,
    AcceptanceRequestId, AcceptanceSubmission, AttentionItem, CallRef, ConfigChange, ConfigScope,
    EvidenceAvailability, EvidenceCursor, EvidencePage, EvidenceRef, EvidenceSourceKind,
    EvidenceSummary, HistoryCursor, HistoryEntry, HistoryPage, InputId, InputState, InputView,
    JobRef, Profile, QuestionId, RecentHistoryPage, RequestId, ResultArtifact, ResultPage,
    ResultRef, ResultSummary, RuntimeConfig, RuntimeId, RuntimeOverrides, RuntimeSettings,
    SessionEvent, SessionId, SessionInfo, SessionSeq, SessionSummary, SubmissionReceipt,
    SubmitInput, UnresolvedWriteStatus, UnresolvedWriteView, WorkspaceId, WorkspaceInfo,
    WorkspaceOverview, WriteResolution,
    storage::{
        BoneStore, DocumentKey, Journal, JournalAppend, JournalKey, Lease, LeaseKey,
        MAX_JOURNAL_ENTRY_BYTES, Revision, StoreError, StoreRoots, WriteTransaction,
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
const EVIDENCE_BODY_CHUNK_BYTES: usize = 64 * 1024;
const LEGACY_EVIDENCE_BACKFILL_LIMIT: usize = 4;
const SESSION_SUMMARY_BACKFILL_RECORDS: usize = 64;
const SESSION_SUMMARY_BACKFILL_BYTES: usize = MAX_JOURNAL_ENTRY_BYTES;
const LATEST_REPLY_PREVIEW_BYTES: usize = 1024;

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
    configs: crate::file_config::FileConfigs,
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
    result_backfill_decodes: AtomicUsize,
    attention_backfill_decodes: AtomicUsize,
    evidence_body_reads: AtomicUsize,
    legacy_evidence_reads: AtomicUsize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct WorkspaceCatalog {
    items: Vec<WorkspaceInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedSession {
    pub info: SessionInfo,
    #[serde(default)]
    pub title_is_provisional: bool,
    pub next_input: u64,
    pub runtime: Option<SavedRuntime>,
    pub agent_through: u64,
    pub core_through: u64,
    pub draft: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct StoredCreateSessionRequest {
    workspace: WorkspaceId,
    title: String,
    provisional: bool,
    session: SessionId,
}

/// Durable projection used by workspace navigation. `through` refers to the
/// raw session journal, including records that do not produce a visible
/// message, so a missing legacy projection can be caught up exactly once.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct SessionSummaryProjection {
    through: SessionSeq,
    message_count: u64,
    latest_reply_preview: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SessionSummaryProjectionWork {
    records: usize,
    payload_bytes: usize,
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
struct AcceptanceRequestRecord {
    submission: AcceptanceSubmission,
    receipt: AcceptanceReceipt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ResultProjectionState {
    snapshot_through: SessionSeq,
    before: SessionSeq,
    complete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct EvidenceProjectionState {
    snapshot_through: SessionSeq,
    before: SessionSeq,
    complete: bool,
}

/// Durable result projection. Its first four fields intentionally retain the
/// old public `ResultSummary` JSON shape; `evidence` defaults empty when an
/// existing database is opened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct StoredResult {
    result: ResultRef,
    outcome: bone_core::OutcomeKind,
    summary: String,
    remaining: Vec<String>,
    #[serde(default)]
    evidence: Vec<EvidenceRef>,
}

impl StoredResult {
    fn public(&self) -> ResultSummary {
        ResultSummary {
            result: self.result,
            outcome: self.outcome,
            summary: self.summary.clone(),
            remaining: self.remaining.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum StoredEvidenceSource {
    Public {
        kind: EvidenceSourceKind,
        title: String,
        body: String,
    },
    /// Deliberately records only that the referenced Core record exists. Its
    /// internal contents never enter the product-facing source projection.
    Private,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum StoredEvidenceMetadata {
    Public {
        kind: EvidenceSourceKind,
        title: String,
        byte_len: u64,
        chunks: Vec<EvidenceBodyChunk>,
    },
    Private,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct EvidenceBodyChunk {
    start: u64,
    end: u64,
}

pub(crate) struct StoredEvidencePage {
    pub metadata: StoredEvidenceMetadata,
    pub text: Option<String>,
    pub offset: u64,
    pub next_offset: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum AttentionProjectionRecord {
    WaitingForUser {
        workspace: WorkspaceId,
        session: SessionId,
        input: InputId,
        runtime: RuntimeId,
        question: QuestionId,
        text: String,
    },
    UnresolvedWrite(UnresolvedWriteView),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct AttentionProjectionState {
    input_before: Option<String>,
    inputs_complete: bool,
    write_before: Option<String>,
    writes_complete: bool,
}

enum AcceptanceSaveOutcome {
    Saved(AcceptanceReceipt, Option<InputView>),
    Existing(AcceptanceReceipt, Option<InputView>),
    Conflict,
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
    fn sync_evidence_source(
        &self,
        transaction: &WriteTransaction<'_, '_>,
        source_ref: EvidenceRef,
        source: &StoredEvidenceSource,
    ) -> Result<(), StoreError> {
        let metadata_document = self
            .store
            .document::<StoredEvidenceMetadata>(evidence_metadata_key(source_ref));
        let existing = transaction.read(&metadata_document)?;
        let metadata = match source {
            StoredEvidenceSource::Private => StoredEvidenceMetadata::Private,
            StoredEvidenceSource::Public { kind, title, body } => {
                let mut chunks = Vec::new();
                let mut start = 0;
                while start < body.len() {
                    let mut end = (start + EVIDENCE_BODY_CHUNK_BYTES).min(body.len());
                    while end > start && !body.is_char_boundary(end) {
                        end -= 1;
                    }
                    let index = chunks.len();
                    let document = self
                        .store
                        .document::<String>(evidence_body_chunk_key(source_ref, index));
                    let snapshot = transaction.read(&document)?;
                    transaction.replace(
                        &document,
                        &body[start..end].to_owned(),
                        snapshot.revision,
                    )?;
                    chunks.push(EvidenceBodyChunk {
                        start: start as u64,
                        end: end as u64,
                    });
                    start = end;
                }
                StoredEvidenceMetadata::Public {
                    kind: *kind,
                    title: title.clone(),
                    byte_len: body.len() as u64,
                    chunks,
                }
            }
        };
        match existing.value {
            Some(saved) if saved == metadata => Ok(()),
            Some(_) => Err(StoreError::Corrupt {
                message: "evidence source projection conflicts with durable history",
            }),
            None => transaction
                .replace(&metadata_document, &metadata, existing.revision)
                .map(|_| ()),
        }
    }

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

    #[cfg(test)]
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let data_dir = data_dir.into();
        Self::open_with_home(&data_dir, data_dir.join(".bone"))
    }

    pub fn open_with_home(
        data_dir: impl Into<PathBuf>,
        bone_home: impl Into<PathBuf>,
    ) -> Result<Self, StoreError> {
        let roots = StoreRoots::new(data_dir.into())?;
        Ok(Self {
            store: BoneStore::open_at(roots)?,
            configs: crate::file_config::FileConfigs::open(bone_home.into())?,
            #[cfg(test)]
            faults: Arc::new(TestFaults {
                agent_record_saves_before_failure: AtomicUsize::new(NO_AGENT_RECORD_FAILURE),
                replayed_agent_record_saves: AtomicUsize::new(0),
                runtime_reconfigure_failure: AtomicBool::new(false),
                pause_core_commits: AtomicBool::new(false),
                core_commit_waiting: AtomicBool::new(false),
                fail_core_chunk: AtomicBool::new(false),
                result_backfill_decodes: AtomicUsize::new(0),
                attention_backfill_decodes: AtomicUsize::new(0),
                evidence_body_reads: AtomicUsize::new(0),
                legacy_evidence_reads: AtomicUsize::new(0),
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
    pub(crate) fn reset_result_projection_for_test(
        &self,
        session: SessionId,
    ) -> Result<(), StoreError> {
        let state = self
            .store
            .document::<ResultProjectionState>(result_projection_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&state)?;
            if snapshot.value.is_some() {
                transaction.delete(&state, snapshot.revision)?;
            }
            for entry in
                transaction.list_documents::<StoredResult>(NAMESPACE, &result_prefix(session))?
            {
                let snapshot = entry.snapshot?;
                if snapshot.value.is_some() {
                    transaction.delete(
                        &self.store.document::<StoredResult>(entry.key),
                        snapshot.revision,
                    )?;
                }
            }
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn delete_evidence_source_for_test(
        &self,
        source: EvidenceRef,
    ) -> Result<(), StoreError> {
        let legacy = self
            .store
            .document::<StoredEvidenceSource>(evidence_source_key(source));
        let metadata = self
            .store
            .document::<StoredEvidenceMetadata>(evidence_metadata_key(source));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&legacy)?;
            if snapshot.value.is_some() {
                transaction.delete(&legacy, snapshot.revision)?;
            }
            let snapshot = transaction.read(&metadata)?;
            if let Some(StoredEvidenceMetadata::Public { chunks, .. }) = &snapshot.value {
                for index in 0..chunks.len() {
                    let chunk = self
                        .store
                        .document::<String>(evidence_body_chunk_key(source, index));
                    let saved = transaction.read(&chunk)?;
                    if saved.value.is_some() {
                        transaction.delete(&chunk, saved.revision)?;
                    }
                }
            }
            if snapshot.value.is_some() {
                transaction.delete(&metadata, snapshot.revision)?;
            }
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn reset_evidence_projection_for_test(
        &self,
        session: SessionId,
    ) -> Result<(), StoreError> {
        let state = self
            .store
            .document::<EvidenceProjectionState>(evidence_projection_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&state)?;
            if snapshot.value.is_some() {
                transaction.delete(&state, snapshot.revision)?;
            }
            for entry in transaction.list_documents::<StoredEvidenceSource>(
                NAMESPACE,
                &evidence_source_prefix(session),
            )? {
                let snapshot = entry.snapshot?;
                if snapshot.value.is_some() {
                    transaction.delete(
                        &self.store.document::<StoredEvidenceSource>(entry.key),
                        snapshot.revision,
                    )?;
                }
            }
            for entry in transaction.list_documents::<StoredEvidenceMetadata>(
                NAMESPACE,
                &evidence_metadata_prefix(session),
            )? {
                let snapshot = entry.snapshot?;
                if snapshot.value.is_some() {
                    transaction.delete(
                        &self.store.document::<StoredEvidenceMetadata>(entry.key),
                        snapshot.revision,
                    )?;
                }
            }
            for entry in
                transaction.list_documents::<String>(NAMESPACE, &evidence_body_prefix(session))?
            {
                let snapshot = entry.snapshot?;
                if snapshot.value.is_some() {
                    transaction
                        .delete(&self.store.document::<String>(entry.key), snapshot.revision)?;
                }
            }
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn result_backfill_decode_count(&self) -> usize {
        self.faults.result_backfill_decodes.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn evidence_body_read_counts(&self) -> (usize, usize) {
        (
            self.faults.evidence_body_reads.load(Ordering::Acquire),
            self.faults.legacy_evidence_reads.load(Ordering::Acquire),
        )
    }

    #[cfg(test)]
    pub(crate) fn seed_result_evidence_for_test(
        &self,
        result: ResultRef,
        sources: Vec<(EvidenceRef, StoredEvidenceSource)>,
    ) -> Result<(), StoreError> {
        self.store.transaction(|transaction| {
            for (source, projection) in &sources {
                self.sync_evidence_source(transaction, *source, projection)?;
            }
            let document = self.store.document::<StoredResult>(result_key(result));
            let snapshot = transaction.read(&document)?;
            transaction.replace(
                &document,
                &StoredResult {
                    result,
                    outcome: bone_core::OutcomeKind::Completed,
                    summary: "large evidence fixture".to_owned(),
                    remaining: Vec::new(),
                    evidence: sources.iter().map(|(source, _)| *source).collect(),
                },
                snapshot.revision,
            )?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn seed_legacy_result_evidence_for_test(
        &self,
        result: ResultRef,
        sources: Vec<(EvidenceRef, StoredEvidenceSource)>,
    ) -> Result<(), StoreError> {
        self.store.transaction(|transaction| {
            for (source, projection) in &sources {
                let document = self
                    .store
                    .document::<StoredEvidenceSource>(evidence_source_key(*source));
                let snapshot = transaction.read(&document)?;
                transaction.replace(&document, projection, snapshot.revision)?;
            }
            let document = self.store.document::<StoredResult>(result_key(result));
            let snapshot = transaction.read(&document)?;
            transaction.replace(
                &document,
                &StoredResult {
                    result,
                    outcome: bone_core::OutcomeKind::Completed,
                    summary: "legacy evidence fixture".to_owned(),
                    remaining: Vec::new(),
                    evidence: sources.iter().map(|(source, _)| *source).collect(),
                },
                snapshot.revision,
            )?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn reset_attention_projection_for_test(&self) -> Result<(), StoreError> {
        let state = self
            .store
            .document::<AttentionProjectionState>(attention_projection_state_key());
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&state)?;
            if snapshot.value.is_some() {
                transaction.delete(&state, snapshot.revision)?;
            }
            for entry in
                transaction.list_documents::<AttentionProjectionRecord>(NAMESPACE, "attention/")?
            {
                let snapshot = entry.snapshot?;
                if snapshot.value.is_some() {
                    transaction.delete(
                        &self.store.document::<AttentionProjectionRecord>(entry.key),
                        snapshot.revision,
                    )?;
                }
            }
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn attention_backfill_decode_count(&self) -> usize {
        self.faults
            .attention_backfill_decodes
            .load(Ordering::Acquire)
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
        let workspace = self.store.transaction(|transaction| {
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
        })?;
        self.configs.load_workspace(&workspace)?;
        Ok(workspace)
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
            title_is_provisional: false,
            next_input: 1,
            runtime: None,
            agent_through: 0,
            core_through: 0,
            draft: String::new(),
        };
        let session_document = self.store.document(session_key(session.info.id));
        let summary_document = self
            .store
            .document(session_summary_projection_key(session.info.id));
        let projection_document = self.store.document(result_projection_key(session.info.id));
        let evidence_projection_document = self
            .store
            .document(evidence_projection_key(session.info.id));
        self.store.transaction(|transaction| {
            transaction.replace(&session_document, &session, Revision::default())?;
            transaction.replace(
                &summary_document,
                &SessionSummaryProjection::default(),
                Revision::default(),
            )?;
            transaction.replace(
                &projection_document,
                &ResultProjectionState {
                    snapshot_through: SessionSeq(0),
                    before: SessionSeq(1),
                    complete: true,
                },
                Revision::default(),
            )?;
            transaction.replace(
                &evidence_projection_document,
                &EvidenceProjectionState {
                    snapshot_through: SessionSeq(0),
                    before: SessionSeq(1),
                    complete: true,
                },
                Revision::default(),
            )?;
            Ok(())
        })?;
        Ok(session)
    }

    pub fn create_session_idempotent(
        &self,
        request_id: RequestId,
        workspace: WorkspaceId,
        title: String,
        provisional: bool,
    ) -> Result<Option<SavedSession>, StoreError> {
        let request_document = self
            .store
            .document::<StoredCreateSessionRequest>(create_session_request_key(request_id));
        self.store.transaction(|transaction| {
            let request_snapshot = transaction.read(&request_document)?;
            if let Some(prior) = request_snapshot.value {
                if prior.workspace != workspace
                    || prior.title != title
                    || prior.provisional != provisional
                {
                    return Ok(None);
                }
                let saved = transaction
                    .read(
                        &self
                            .store
                            .document::<SavedSession>(session_key(prior.session)),
                    )?
                    .value
                    .ok_or(StoreError::Corrupt {
                        message: "create-session request points to a missing session",
                    })?;
                return Ok(Some(saved));
            }

            let session = SavedSession {
                info: SessionInfo {
                    id: SessionId::new(),
                    workspace,
                    title: title.clone(),
                    archived: false,
                },
                title_is_provisional: provisional,
                next_input: 1,
                runtime: None,
                agent_through: 0,
                core_through: 0,
                draft: String::new(),
            };
            transaction.replace(
                &self.store.document(session_key(session.info.id)),
                &session,
                Revision::default(),
            )?;
            transaction.replace(
                &self
                    .store
                    .document(session_summary_projection_key(session.info.id)),
                &SessionSummaryProjection::default(),
                Revision::default(),
            )?;
            transaction.replace(
                &self.store.document(result_projection_key(session.info.id)),
                &ResultProjectionState {
                    snapshot_through: SessionSeq(0),
                    before: SessionSeq(1),
                    complete: true,
                },
                Revision::default(),
            )?;
            transaction.replace(
                &self
                    .store
                    .document(evidence_projection_key(session.info.id)),
                &EvidenceProjectionState {
                    snapshot_through: SessionSeq(0),
                    before: SessionSeq(1),
                    complete: true,
                },
                Revision::default(),
            )?;
            transaction.replace(
                &request_document,
                &StoredCreateSessionRequest {
                    workspace,
                    title,
                    provisional,
                    session: session.info.id,
                },
                request_snapshot.revision,
            )?;
            Ok(Some(session))
        })
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

    pub fn last_active_session(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<SessionId>, StoreError> {
        self.store
            .document::<SessionId>(last_active_session_key(workspace))
            .read()
            .map(|snapshot| snapshot.value)
    }

    pub fn set_last_active_session(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
    ) -> Result<bool, StoreError> {
        let active = self
            .store
            .document::<SessionId>(last_active_session_key(workspace));
        self.store.transaction(|transaction| {
            let saved = transaction
                .read(&self.store.document::<SavedSession>(session_key(session)))?
                .value;
            let Some(saved) = saved else {
                return Ok(false);
            };
            if saved.info.workspace != workspace {
                return Ok(false);
            }
            let snapshot = transaction.read(&active)?;
            transaction.replace(&active, &session, snapshot.revision)?;
            Ok(true)
        })
    }

    /// Build a workspace navigation snapshot without acquiring a Session lease
    /// or starting a Runtime. Legacy databases advance one bounded navigation
    /// slice and one bounded attention backfill window; business records remain
    /// read-only. Incomplete navigation fields are marked on each summary.
    pub fn workspace_overview(
        &self,
        workspace: WorkspaceInfo,
    ) -> Result<WorkspaceOverview, StoreError> {
        self.advance_session_summary_projections(workspace.id)?;
        let attention_projection_pending = self.backfill_attention_projection()?;
        self.store.read_transaction(|transaction| {
            let mut saved_sessions = Vec::new();
            for entry in transaction.list_documents::<SavedSession>(NAMESPACE, "session/")? {
                let created_at = entry.created_at;
                let Some(session) = entry.snapshot?.value else {
                    continue;
                };
                if session.info.workspace == workspace.id {
                    saved_sessions.push((session, created_at));
                }
            }
            saved_sessions.sort_by_key(|(session, _)| session.info.id);

            let mut sessions = Vec::with_capacity(saved_sessions.len());
            for (saved, created_at) in saved_sessions {
                let journal = self
                    .store
                    .journal::<StoredEvent>(journal_key(saved.info.id));
                let history_through = SessionSeq(transaction.journal_last_sequence(&journal)?);
                let projection = transaction
                    .read(&self.store.document::<SessionSummaryProjection>(
                        session_summary_projection_key(saved.info.id),
                    ))?
                    .value
                    .unwrap_or_default();
                if projection.through > history_through {
                    return Err(StoreError::Corrupt {
                        message: "session summary projection is ahead of its journal",
                    });
                }
                sessions.push(SessionSummary {
                    session: saved.info,
                    created_at,
                    message_count: projection.message_count,
                    latest_reply_preview: projection.latest_reply_preview,
                    projection_pending: projection.through != history_through,
                    has_draft: !saved.draft.is_empty(),
                    draft_bytes: saved.draft.len() as u64,
                    persisted_runtime: saved.runtime.map(|runtime| runtime.id),
                    history_through,
                });
            }

            let mut questions =
                BTreeMap::<(SessionId, RuntimeId, u64), (QuestionId, String, Vec<InputId>)>::new();
            let mut unresolved_writes = Vec::new();
            for entry in transaction.list_documents::<AttentionProjectionRecord>(
                NAMESPACE,
                &attention_workspace_prefix(workspace.id),
            )? {
                let Some(record) = entry.snapshot?.value else {
                    continue;
                };
                match record {
                    AttentionProjectionRecord::WaitingForUser {
                        workspace: saved_workspace,
                        session,
                        input,
                        runtime,
                        question,
                        text,
                    } => {
                        if saved_workspace != workspace.id {
                            return Err(StoreError::Corrupt {
                                message: "attention record is stored under the wrong workspace",
                            });
                        }
                        let item = questions
                            .entry((session, runtime, question.record))
                            .or_insert_with(|| (question, text, Vec::new()));
                        item.2.push(input);
                    }
                    AttentionProjectionRecord::UnresolvedWrite(write) => {
                        if write.workspace != workspace.id {
                            return Err(StoreError::Corrupt {
                                message: "write attention is stored under the wrong workspace",
                            });
                        }
                        unresolved_writes.push(write);
                    }
                }
            }
            let mut attention = questions
                .into_iter()
                .map(|((session, runtime, _), (question, text, mut inputs))| {
                    inputs.sort();
                    AttentionItem::WaitingForUser {
                        session,
                        inputs,
                        runtime,
                        question,
                        text,
                    }
                })
                .collect::<Vec<_>>();
            unresolved_writes.sort_by_key(|write| write.call);
            attention.extend(unresolved_writes.iter().map(|write| {
                AttentionItem::UnresolvedWrite {
                    session: write.session,
                    call: write.call,
                    status: write.status,
                }
            }));

            Ok(WorkspaceOverview {
                workspace,
                sessions,
                attention,
                unresolved_writes,
                attention_projection_pending,
            })
        })
    }

    /// Advance at most one globally bounded slice of legacy or mixed-version
    /// history. A later overview resumes from each projection's durable cursor.
    fn advance_session_summary_projections(
        &self,
        workspace: WorkspaceId,
    ) -> Result<(), StoreError> {
        let mut sessions = Vec::new();
        for entry in self
            .store
            .list_documents::<SavedSession>(NAMESPACE, "session/")?
        {
            let Some(saved) = entry.snapshot?.value else {
                continue;
            };
            if saved.info.workspace == workspace {
                sessions.push(saved.info.id);
            }
        }
        let mut records = SESSION_SUMMARY_BACKFILL_RECORDS;
        let mut payload_bytes = SESSION_SUMMARY_BACKFILL_BYTES;
        for session in sessions {
            if records == 0 || payload_bytes == 0 {
                break;
            }
            let work = self.advance_session_summary_projection(session, records, payload_bytes)?;
            records = records.saturating_sub(work.records);
            payload_bytes = payload_bytes.saturating_sub(work.payload_bytes);
        }
        Ok(())
    }

    fn advance_session_summary_projection(
        &self,
        session: SessionId,
        maximum_records: usize,
        maximum_payload_bytes: usize,
    ) -> Result<SessionSummaryProjectionWork, StoreError> {
        let document = self
            .store
            .document::<SessionSummaryProjection>(session_summary_projection_key(session));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        let current = self.store.read_transaction(|transaction| {
            let projection = transaction.read(&document)?.value.unwrap_or_default();
            let history_through = transaction.journal_last_sequence(&journal)?;
            if projection.through.0 > history_through {
                return Err(StoreError::Corrupt {
                    message: "session summary projection is ahead of its journal",
                });
            }
            Ok(projection.through.0 == history_through)
        })?;
        if current {
            return Ok(SessionSummaryProjectionWork::default());
        }
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let prior = snapshot.value;
            let mut projection = prior.clone().unwrap_or_default();
            let work = advance_session_summary_projection(
                transaction,
                &journal,
                &mut projection,
                maximum_records,
                maximum_payload_bytes,
            )?;
            if prior.as_ref() != Some(&projection) {
                transaction.replace(&document, &projection, snapshot.revision)?;
            }
            Ok(work)
        })
    }

    /// Keep ordinary writes constant-time. A current projection advances in
    /// the same transaction; a missing or stale projection remains a durable
    /// prefix and is resumed by later overview calls.
    fn append_session_event(
        &self,
        transaction: &WriteTransaction<'_, '_>,
        session: SessionId,
        journal: &Journal<StoredEvent>,
        event: &StoredEvent,
    ) -> Result<JournalAppend, StoreError> {
        let document = self
            .store
            .document::<SessionSummaryProjection>(session_summary_projection_key(session));
        let snapshot = transaction.read(&document)?;
        let history_through = transaction.journal_last_sequence(journal)?;
        if snapshot
            .value
            .as_ref()
            .is_some_and(|projection| projection.through.0 > history_through)
        {
            return Err(StoreError::Corrupt {
                message: "session summary projection is ahead of its journal",
            });
        }
        let mut projection = snapshot
            .value
            .filter(|projection| projection.through.0 == history_through);
        let append = transaction.append(journal, event)?;
        if let Some(projection) = &mut projection {
            apply_session_summary_event(projection, append.sequence, event)?;
            transaction.replace(&document, projection, snapshot.revision)?;
        }
        Ok(append)
    }

    /// Advance the legacy attention projection by one fixed document window.
    /// Derived records and both cursors commit atomically, so cancellation can
    /// only interrupt between durable windows.
    fn backfill_attention_projection(&self) -> Result<bool, StoreError> {
        const BACKFILL_WINDOW: usize = 128;
        let state_document = self
            .store
            .document::<AttentionProjectionState>(attention_projection_state_key());
        if state_document
            .read()?
            .value
            .is_some_and(|state| state.inputs_complete && state.writes_complete)
        {
            return Ok(false);
        }
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&state_document)?;
            let original = snapshot.value.clone();
            let mut state = snapshot.value.unwrap_or_default();
            if !state.inputs_complete {
                let page = transaction.recent_documents::<InputView>(
                    NAMESPACE,
                    "input/",
                    state.input_before.as_deref(),
                    BACKFILL_WINDOW,
                )?;
                #[cfg(test)]
                self.faults.attention_backfill_decodes.fetch_add(
                    page.entries.len() + usize::from(page.has_older),
                    Ordering::AcqRel,
                );
                let next_before = page.entries.last().map(|entry| entry.key.key().to_owned());
                for entry in page.entries {
                    let session = session_from_input_key(entry.key.key())?;
                    let Some(input) = entry.snapshot?.value else {
                        continue;
                    };
                    let saved = transaction
                        .read(&self.store.document::<SavedSession>(session_key(session)))?
                        .value
                        .ok_or(StoreError::Corrupt {
                            message: "input belongs to a missing session",
                        })?;
                    self.sync_input_attention(transaction, saved.info.workspace, session, &input)?;
                }
                state.inputs_complete = !page.has_older;
                state.input_before = next_before;
            }
            if !state.writes_complete {
                let page = transaction.recent_documents::<WriteAttempt>(
                    NAMESPACE,
                    "write/",
                    state.write_before.as_deref(),
                    BACKFILL_WINDOW,
                )?;
                #[cfg(test)]
                self.faults.attention_backfill_decodes.fetch_add(
                    page.entries.len() + usize::from(page.has_older),
                    Ordering::AcqRel,
                );
                let next_before = page.entries.last().map(|entry| entry.key.key().to_owned());
                for entry in page.entries {
                    let Some(attempt) = entry.snapshot?.value else {
                        continue;
                    };
                    let saved = transaction
                        .read(
                            &self
                                .store
                                .document::<SavedSession>(session_key(attempt.session)),
                        )?
                        .value
                        .ok_or(StoreError::Corrupt {
                            message: "write attempt belongs to a missing session",
                        })?;
                    self.sync_write_attention(transaction, saved.info.workspace, &attempt)?;
                }
                state.writes_complete = !page.has_older;
                state.write_before = next_before;
            }
            if original.as_ref() != Some(&state) {
                transaction.replace(&state_document, &state, snapshot.revision)?;
            }
            Ok(!(state.inputs_complete && state.writes_complete))
        })
    }

    fn sync_input_attention(
        &self,
        transaction: &WriteTransaction<'_, '_>,
        workspace: WorkspaceId,
        session: SessionId,
        input: &InputView,
    ) -> Result<(), StoreError> {
        let document = self
            .store
            .document::<AttentionProjectionRecord>(attention_input_key(
                workspace, session, input.id,
            ));
        let snapshot = transaction.read(&document)?;
        if let InputState::WaitingForUser {
            runtime,
            question,
            text,
        } = &input.state
        {
            transaction.replace(
                &document,
                &AttentionProjectionRecord::WaitingForUser {
                    workspace,
                    session,
                    input: input.id,
                    runtime: *runtime,
                    question: *question,
                    text: text.clone(),
                },
                snapshot.revision,
            )?;
        } else if snapshot.value.is_some() {
            transaction.delete(&document, snapshot.revision)?;
        }
        Ok(())
    }

    fn sync_write_attention(
        &self,
        transaction: &WriteTransaction<'_, '_>,
        workspace: WorkspaceId,
        attempt: &WriteAttempt,
    ) -> Result<(), StoreError> {
        let document = self
            .store
            .document::<AttentionProjectionRecord>(attention_write_key(workspace, attempt.call));
        let snapshot = transaction.read(&document)?;
        transaction.replace(
            &document,
            &AttentionProjectionRecord::UnresolvedWrite(write_view(workspace, attempt)),
            snapshot.revision,
        )?;
        Ok(())
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
                let append = self.append_session_event(
                    transaction,
                    session,
                    &journal,
                    &StoredEvent::App(event),
                )?;
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
        let session_document = self.store.document::<SavedSession>(session_key(session));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let saved = transaction
                .read(&session_document)?
                .value
                .ok_or(StoreError::Corrupt {
                    message: "input belongs to a missing session",
                })?;
            let snapshot = transaction.read(&document)?;
            let mut input = snapshot.value.ok_or(StoreError::Corrupt {
                message: "input does not exist",
            })?;
            input.state = state;
            let sequence = event
                .map(|event| {
                    self.append_session_event(
                        transaction,
                        session,
                        &journal,
                        &StoredEvent::App(event),
                    )
                })
                .transpose()?
                .map(|append| SessionSeq(append.sequence));
            transaction.replace(&document, &input, snapshot.revision)?;
            self.sync_input_attention(transaction, saved.info.workspace, session, &input)?;
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
            let saved = transaction
                .read(&self.store.document::<SavedSession>(session_key(session)))?
                .value
                .ok_or(StoreError::Corrupt {
                    message: "input belongs to a missing session",
                })?;
            let mut updated = Vec::with_capacity(inputs.len());
            for id in inputs {
                let document = self.store.document::<InputView>(input_key(session, *id));
                let snapshot = transaction.read(&document)?;
                let mut input = snapshot.value.ok_or(StoreError::Corrupt {
                    message: "input does not exist",
                })?;
                input.state = InputState::Accepted { runtime };
                transaction.replace(&document, &input, snapshot.revision)?;
                self.sync_input_attention(transaction, saved.info.workspace, session, &input)?;
                updated.push(input);
            }
            Ok(updated)
        })
    }

    pub fn cancel_inputs(&self, session: SessionId, inputs: &[InputId]) -> Result<(), StoreError> {
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let saved = transaction
                .read(&self.store.document::<SavedSession>(session_key(session)))?
                .value
                .ok_or(StoreError::Corrupt {
                    message: "input belongs to a missing session",
                })?;
            for id in inputs {
                let document = self.store.document::<InputView>(input_key(session, *id));
                let snapshot = transaction.read(&document)?;
                let mut input = snapshot.value.ok_or(StoreError::Corrupt {
                    message: "input does not exist",
                })?;
                input.state = InputState::Cancelled;
                transaction.replace(&document, &input, snapshot.revision)?;
                self.sync_input_attention(transaction, saved.info.workspace, session, &input)?;
                self.append_session_event(
                    transaction,
                    session,
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
                self.sync_input_attention(transaction, saved.info.workspace, session, &input)?;
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
                    let attention =
                        self.store
                            .document::<AttentionProjectionRecord>(attention_write_key(
                                saved.info.workspace,
                                origin,
                            ));
                    let attention_snapshot = transaction.read(&attention)?;
                    if attention_snapshot.value.is_some() {
                        transaction.delete(&attention, attention_snapshot.revision)?;
                    }
                }
            }
            let stored = StoredEvent::Agent {
                runtime,
                record: record.clone(),
            };
            let source_ref = EvidenceRef {
                session,
                record: record.seq.0,
            };
            let source = evidence_source_projection(record);
            self.sync_evidence_source(transaction, source_ref, &source)?;
            let append = self.append_session_event(transaction, session, &journal, &stored)?;
            if let Some(result) = result_summary(session, SessionSeq(append.sequence), &stored) {
                transaction.replace(
                    &self
                        .store
                        .document::<StoredResult>(result_key(result.result)),
                    &result,
                    Revision::default(),
                )?;
            }
            saved.agent_through = record.seq.0;
            saved.core_through = record.seq.0;
            transaction.replace(&session_document, &saved, snapshot.revision)?;
            Ok(Some(SessionSeq(append.sequence)))
        })
    }

    pub fn rename_session(&self, id: SessionId, title: String) -> Result<SessionInfo, StoreError> {
        self.update_session(id, |saved| {
            saved.info.title = title;
            saved.title_is_provisional = false;
        })
        .map(|saved| saved.info)
    }

    pub fn rename_session_if_provisional(
        &self,
        id: SessionId,
        title: String,
    ) -> Result<Option<SessionInfo>, StoreError> {
        let document = self.store.document::<SavedSession>(session_key(id));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&document)?;
            let mut saved = snapshot.value.ok_or(StoreError::Corrupt {
                message: "session does not exist",
            })?;
            if !saved.title_is_provisional {
                return Ok(None);
            }
            saved.info.title = title;
            saved.title_is_provisional = false;
            transaction.replace(&document, &saved, snapshot.revision)?;
            Ok(Some(saved.info))
        })
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
                        | InputState::ConversationFailed { runtime: id, .. }
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
                self.sync_input_attention(transaction, saved.info.workspace, session, &input)?;
            }
            if !interrupted.is_empty() {
                self.append_session_event(
                    transaction,
                    session,
                    &journal,
                    &StoredEvent::App(SessionEvent::Interrupted {
                        runtime: runtime.id,
                        inputs: interrupted.clone(),
                    }),
                )?;
            }
            self.append_session_event(
                transaction,
                session,
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
            let saved = transaction
                .read(&self.store.document::<SavedSession>(session_key(session)))?
                .value
                .ok_or(StoreError::Corrupt {
                    message: "write attempt belongs to a missing session",
                })?;
            if saved.info.workspace != workspace {
                return Err(StoreError::Corrupt {
                    message: "write attempt workspace does not match its session",
                });
            }
            let snapshot = transaction.read(&document)?;
            if snapshot.value.is_some() || transaction.read(&resolution)?.value.is_some() {
                return Ok(false);
            }
            let attempt = WriteAttempt {
                session,
                call,
                job: None,
                tool: tool.to_owned(),
                arguments,
                state: WriteState::Pending,
            };
            transaction.replace(&document, &attempt, snapshot.revision)?;
            self.sync_write_attention(transaction, workspace, &attempt)?;
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
            self.sync_write_attention(transaction, workspace, &attempt)?;
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
            self.sync_write_attention(transaction, workspace, &attempt)?;
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
            let attention = self
                .store
                .document::<AttentionProjectionRecord>(attention_write_key(workspace, call));
            let attention_snapshot = transaction.read(&attention)?;
            if attention_snapshot.value.is_some() {
                transaction.delete(&attention, attention_snapshot.revision)?;
            }
            self.append_session_event(
                transaction,
                session,
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

    pub fn recent_history(
        &self,
        session: SessionId,
        cursor: Option<HistoryCursor>,
        limit: usize,
    ) -> Result<RecentHistoryPage, StoreError> {
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        let page = journal.read_recent(
            cursor.map(|cursor| (cursor.before().0, cursor.snapshot_through().0)),
            limit.max(1),
        )?;
        let mut items = Vec::new();
        for entry in page.entries {
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
        Ok(RecentHistoryPage {
            items,
            older_cursor: page.older_before.map(|before| {
                HistoryCursor::new(
                    session,
                    SessionSeq(before),
                    SessionSeq(page.snapshot_through),
                )
            }),
            snapshot_through: SessionSeq(page.snapshot_through),
        })
    }

    pub fn results(
        &self,
        session: SessionId,
        cursor: Option<HistoryCursor>,
        limit: usize,
    ) -> Result<ResultPage, StoreError> {
        let projection_pending = self.backfill_result_projection(session)?;
        let snapshot_through = match cursor {
            Some(cursor) => cursor.snapshot_through(),
            None => self.history_through(session)?,
        };
        let before_sequence = cursor.map_or_else(
            || {
                snapshot_through
                    .0
                    .checked_add(1)
                    .map(SessionSeq)
                    .ok_or(StoreError::RevisionExhausted)
            },
            |cursor| Ok(cursor.before()),
        )?;
        let prefix = result_prefix(session);
        let before = format!("{}{:020}", prefix, before_sequence.0);
        let page = self.store.recent_documents::<StoredResult>(
            NAMESPACE,
            &prefix,
            Some(&before),
            limit.max(1),
        )?;
        let mut items = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            if let Some(result) = entry.snapshot?.value {
                if result.result.session != session
                    || result.result.version.0 >= before_sequence.0
                    || result.result.version.0 > snapshot_through.0
                {
                    return Err(StoreError::Corrupt {
                        message: "result record is stored under the wrong key range",
                    });
                }
                items.push(result.public());
            }
        }
        items.sort_by_key(|item| item.result.version);
        let older_cursor = page.has_older.then(|| {
            HistoryCursor::new(
                session,
                items
                    .first()
                    .expect("an older result requires a non-empty page")
                    .result
                    .version,
                snapshot_through,
            )
        });
        Ok(ResultPage {
            items,
            older_cursor,
            snapshot_through,
            projection_pending,
        })
    }

    /// Advance a legacy database's result projection by at most one fixed
    /// journal window. The progress document and derived result records commit
    /// atomically, so cancellation can only occur between complete windows.
    fn backfill_result_projection(&self, session: SessionId) -> Result<bool, StoreError> {
        const BACKFILL_WINDOW: usize = 256;
        let state_document = self
            .store
            .document::<ResultProjectionState>(result_projection_key(session));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&state_document)?;
            let mut state = match snapshot.value {
                Some(state) => state,
                None => {
                    let through = transaction.journal_last_sequence(&journal)?;
                    ResultProjectionState {
                        snapshot_through: SessionSeq(through),
                        before: SessionSeq(
                            through
                                .checked_add(1)
                                .ok_or(StoreError::RevisionExhausted)?,
                        ),
                        complete: through == 0,
                    }
                }
            };
            if state.complete {
                return Ok(false);
            }
            let page = transaction.read_recent(
                &journal,
                Some((state.before.0, state.snapshot_through.0)),
                BACKFILL_WINDOW,
            )?;
            #[cfg(test)]
            self.faults.result_backfill_decodes.fetch_add(
                page.entries.len() + usize::from(page.older_before.is_some()),
                Ordering::AcqRel,
            );
            for entry in page.entries {
                let Some(result) =
                    result_summary(session, SessionSeq(entry.sequence), &entry.event)
                else {
                    continue;
                };
                let document = self
                    .store
                    .document::<StoredResult>(result_key(result.result));
                let existing = transaction.read(&document)?;
                match existing.value {
                    Some(saved) if saved == result => {}
                    Some(_) => {
                        return Err(StoreError::Corrupt {
                            message: "result projection conflicts with durable history",
                        });
                    }
                    None => {
                        transaction.replace(&document, &result, existing.revision)?;
                    }
                }
            }
            state.complete = page.older_before.is_none();
            state.before = SessionSeq(page.older_before.unwrap_or(1));
            transaction.replace(&state_document, &state, snapshot.revision)?;
            Ok(!state.complete)
        })
    }

    /// Advance the source projection independently from the older result
    /// projection. Existing databases may have completed result backfill
    /// before source documents existed.
    fn backfill_evidence_projection(&self, session: SessionId) -> Result<bool, StoreError> {
        const BACKFILL_WINDOW: usize = 256;
        let state_document = self
            .store
            .document::<EvidenceProjectionState>(evidence_projection_key(session));
        let journal = self.store.journal::<StoredEvent>(journal_key(session));
        self.store.transaction(|transaction| {
            let snapshot = transaction.read(&state_document)?;
            let mut state = match snapshot.value {
                Some(state) => state,
                None => {
                    let through = transaction.journal_last_sequence(&journal)?;
                    EvidenceProjectionState {
                        snapshot_through: SessionSeq(through),
                        before: SessionSeq(
                            through
                                .checked_add(1)
                                .ok_or(StoreError::RevisionExhausted)?,
                        ),
                        complete: through == 0,
                    }
                }
            };
            if state.complete {
                return Ok(false);
            }
            let page = transaction.read_recent(
                &journal,
                Some((state.before.0, state.snapshot_through.0)),
                BACKFILL_WINDOW,
            )?;
            for entry in page.entries {
                let StoredEvent::Agent { record, .. } = entry.event else {
                    continue;
                };
                let source_ref = EvidenceRef {
                    session,
                    record: record.seq.0,
                };
                let source = evidence_source_projection(&record);
                self.sync_evidence_source(transaction, source_ref, &source)?;
            }
            state.complete = page.older_before.is_none();
            state.before = SessionSeq(page.older_before.unwrap_or(1));
            transaction.replace(&state_document, &state, snapshot.revision)?;
            Ok(!state.complete)
        })
    }

    pub fn acceptances(
        &self,
        result: ResultRef,
        cursor: Option<AcceptanceCursor>,
        limit: usize,
    ) -> Result<AcceptancePage, StoreError> {
        let prefix = acceptance_prefix(result);
        let before = cursor.map(|cursor| format!("{}{:020}", prefix, cursor.before().0));
        let page = self.store.recent_documents::<AcceptanceRecord>(
            NAMESPACE,
            &prefix,
            before.as_deref(),
            limit.max(1),
        )?;
        let mut records = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            if let Some(record) = entry.snapshot?.value {
                if record.result != result {
                    return Err(StoreError::Corrupt {
                        message: "acceptance record is stored under the wrong result",
                    });
                }
                records.push(record);
            }
        }
        records.sort_by_key(|record| record.saved_at);
        let older_cursor = page
            .has_older
            .then(|| {
                records
                    .first()
                    .map(|record| AcceptanceCursor::new(result, record.saved_at))
            })
            .flatten();
        Ok(AcceptancePage {
            items: records,
            older_cursor,
        })
    }

    pub fn record_acceptance(
        &self,
        submission: &AcceptanceSubmission,
    ) -> Result<(AcceptanceReceipt, Option<InputView>), AcceptanceError> {
        let result = self
            .result(submission.result)?
            .ok_or(AcceptanceError::ResultNotFound)?;
        if result.result != submission.result {
            return Err(AcceptanceError::ResultNotFound);
        }
        let session_document = self
            .store
            .document::<SavedSession>(session_key(submission.result.session));
        let request_document =
            self.store
                .document::<AcceptanceRequestRecord>(acceptance_request_key(
                    submission.result.session,
                    submission.request_id,
                ));
        let journal = self
            .store
            .journal::<StoredEvent>(journal_key(submission.result.session));
        let acceptance = AcceptanceId::new();
        let outcome = self.store.transaction(|transaction| {
            let request_snapshot = transaction.read(&request_document)?;
            if let Some(existing) = request_snapshot.value {
                if existing.submission == *submission {
                    let input = match existing.receipt.rework {
                        Some(receipt) => Some(
                            transaction
                                .read(&self.store.document::<InputView>(input_key(
                                    submission.result.session,
                                    receipt.input,
                                )))?
                                .value
                                .ok_or(StoreError::Corrupt {
                                    message: "acceptance rework input is missing",
                                })?,
                        ),
                        None => None,
                    };
                    return Ok(AcceptanceSaveOutcome::Existing(existing.receipt, input));
                }
                return Ok(AcceptanceSaveOutcome::Conflict);
            }
            let session_snapshot = transaction.read(&session_document)?;
            let mut saved_session = session_snapshot.value.ok_or(StoreError::Corrupt {
                message: "acceptance result belongs to a missing session",
            })?;
            let input_request = submission.rework.as_ref().map(|input| {
                self.store.document::<RequestRecord>(request_key(
                    submission.result.session,
                    input.request_id,
                ))
            });
            let input_request_snapshot = match &input_request {
                Some(document) => {
                    let snapshot = transaction.read(document)?;
                    if snapshot.value.is_some() {
                        return Ok(AcceptanceSaveOutcome::Conflict);
                    }
                    Some(snapshot)
                }
                None => None,
            };
            let accepted = self.append_session_event(
                transaction,
                submission.result.session,
                &journal,
                &StoredEvent::App(SessionEvent::AcceptanceRecorded {
                    acceptance,
                    result: submission.result,
                    decision: submission.decision,
                    reason: submission.reason.clone(),
                }),
            )?;
            let mut saved_input = None;
            let rework = if let Some(input) = &submission.rework {
                let id = InputId(saved_session.next_input);
                saved_session.next_input = saved_session
                    .next_input
                    .checked_add(1)
                    .ok_or(StoreError::RevisionExhausted)?;
                let submitted_event = StoredEvent::App(SessionEvent::InputSubmitted {
                    input: id,
                    request_id: input.request_id,
                    text: input.text.clone(),
                    reply_to: input.reply_to,
                });
                let submitted = self.append_session_event(
                    transaction,
                    submission.result.session,
                    &journal,
                    &submitted_event,
                )?;
                let receipt = SubmissionReceipt {
                    input: id,
                    saved_at: SessionSeq(submitted.sequence),
                };
                let view = InputView {
                    id,
                    request_id: input.request_id,
                    text: input.text.clone(),
                    reply_to: input.reply_to,
                    state: InputState::Queued { problem: None },
                };
                transaction.replace(
                    &self
                        .store
                        .document::<InputView>(input_key(submission.result.session, id)),
                    &view,
                    Revision::default(),
                )?;
                transaction.replace(
                    input_request.as_ref().expect("rework request exists"),
                    &RequestRecord {
                        input: id,
                        text: input.text.clone(),
                        reply_to: input.reply_to,
                        saved_at: receipt.saved_at,
                    },
                    input_request_snapshot
                        .as_ref()
                        .expect("rework request snapshot exists")
                        .revision,
                )?;
                saved_input = Some(view);
                Some(receipt)
            } else {
                None
            };
            let receipt = AcceptanceReceipt {
                id: acceptance,
                saved_at: SessionSeq(accepted.sequence),
                rework,
            };
            let record = AcceptanceRecord {
                id: acceptance,
                request_id: submission.request_id,
                result: submission.result,
                decision: submission.decision,
                reason: submission.reason.clone(),
                saved_at: receipt.saved_at,
                rework: receipt.rework,
            };
            transaction.replace(
                &self
                    .store
                    .document::<AcceptanceRecord>(acceptance_key(&record)),
                &record,
                Revision::default(),
            )?;
            transaction.replace(
                &request_document,
                &AcceptanceRequestRecord {
                    submission: submission.clone(),
                    receipt: receipt.clone(),
                },
                request_snapshot.revision,
            )?;
            transaction.replace(&session_document, &saved_session, session_snapshot.revision)?;
            Ok(AcceptanceSaveOutcome::Saved(receipt, saved_input))
        })?;
        match outcome {
            AcceptanceSaveOutcome::Saved(receipt, input)
            | AcceptanceSaveOutcome::Existing(receipt, input) => Ok((receipt, input)),
            AcceptanceSaveOutcome::Conflict => Err(AcceptanceError::Conflict),
        }
    }

    pub fn result(&self, result: ResultRef) -> Result<Option<ResultSummary>, StoreError> {
        self.stored_result(result)
            .map(|saved| saved.map(|saved| saved.public()))
    }

    fn stored_result(&self, result: ResultRef) -> Result<Option<StoredResult>, StoreError> {
        if let Some(saved) = self
            .store
            .document::<StoredResult>(result_key(result))
            .read()?
            .value
        {
            return if saved.result == result {
                Ok(Some(saved))
            } else {
                Err(StoreError::Corrupt {
                    message: "result projection is stored under the wrong key",
                })
            };
        }
        let journal = self
            .store
            .journal::<StoredEvent>(journal_key(result.session));
        let Some(entry) = journal.read_entry(result.version.0)? else {
            return Ok(None);
        };
        Ok(result_summary(result.session, result.version, &entry.event)
            .filter(|saved| saved.result == result))
    }

    pub fn result_artifact(&self, result: ResultRef) -> Result<Option<ResultArtifact>, StoreError> {
        Ok(self.stored_result(result)?.map(|saved| ResultArtifact {
            result: saved.result,
            outcome: saved.outcome,
            summary: saved.summary,
            remaining: saved.remaining,
            evidence_count: saved.evidence.len(),
        }))
    }

    pub fn result_evidence(
        &self,
        result: ResultRef,
        cursor: Option<EvidenceCursor>,
        limit: usize,
    ) -> Result<Option<EvidencePage>, StoreError> {
        let mut projection_pending = self.backfill_result_projection(result.session)?
            | self.backfill_evidence_projection(result.session)?;
        let Some(saved) = self.stored_result(result)? else {
            return Ok(None);
        };
        let start = cursor.map_or(0, EvidenceCursor::offset);
        let end = start.saturating_add(limit.max(1)).min(saved.evidence.len());
        projection_pending |=
            self.prepare_evidence_metadata(saved.evidence.get(start..end).unwrap_or_default())?;
        let mut items = Vec::with_capacity(end.saturating_sub(start));
        for source in saved.evidence.get(start..end).unwrap_or_default() {
            let projected = self
                .store
                .document::<StoredEvidenceMetadata>(evidence_metadata_key(*source))
                .read()?
                .value;
            let availability = match projected {
                Some(StoredEvidenceMetadata::Public { kind, title, .. }) => {
                    EvidenceAvailability::Available { kind, title }
                }
                Some(StoredEvidenceMetadata::Private) => EvidenceAvailability::Private,
                None => EvidenceAvailability::Missing,
            };
            items.push(EvidenceSummary {
                source: *source,
                availability,
            });
        }
        Ok(Some(EvidencePage {
            result,
            items,
            next_cursor: (end < saved.evidence.len()).then(|| EvidenceCursor::new(result, end)),
            projection_pending,
        }))
    }

    fn prepare_evidence_metadata(&self, sources: &[EvidenceRef]) -> Result<bool, StoreError> {
        let mut pending = false;
        let mut migrated = 0;
        self.store.transaction(|transaction| {
            for source in sources {
                let metadata = self
                    .store
                    .document::<StoredEvidenceMetadata>(evidence_metadata_key(*source));
                if transaction.read(&metadata)?.value.is_some() {
                    continue;
                }
                if migrated >= LEGACY_EVIDENCE_BACKFILL_LIMIT {
                    pending = true;
                    continue;
                }
                #[cfg(test)]
                self.faults
                    .legacy_evidence_reads
                    .fetch_add(1, Ordering::AcqRel);
                let legacy = self
                    .store
                    .document::<StoredEvidenceSource>(evidence_source_key(*source));
                if let Some(source_record) = transaction.read(&legacy)?.value {
                    self.sync_evidence_source(transaction, *source, &source_record)?;
                    migrated += 1;
                }
            }
            Ok(())
        })?;
        Ok(pending)
    }

    pub(crate) fn evidence_projection_pending(
        &self,
        session: SessionId,
    ) -> Result<bool, StoreError> {
        Ok(self.backfill_result_projection(session)?
            | self.backfill_evidence_projection(session)?)
    }

    pub(crate) fn evidence_source_page_record(
        &self,
        source: EvidenceRef,
        offset: u64,
        max_bytes: usize,
    ) -> Result<Option<StoredEvidencePage>, StoreError> {
        self.prepare_evidence_metadata(&[source])?;
        let Some(metadata) = self
            .store
            .document::<StoredEvidenceMetadata>(evidence_metadata_key(source))
            .read()?
            .value
        else {
            return Ok(None);
        };
        let StoredEvidenceMetadata::Public {
            byte_len, chunks, ..
        } = &metadata
        else {
            return Ok(Some(StoredEvidencePage {
                metadata,
                text: None,
                offset: 0,
                next_offset: None,
            }));
        };
        let byte_len = *byte_len;
        if offset > byte_len {
            return Err(StoreError::Corrupt {
                message: "evidence offset is out of range",
            });
        }
        if offset == byte_len {
            return Ok(Some(StoredEvidencePage {
                metadata,
                text: Some(String::new()),
                offset,
                next_offset: None,
            }));
        }
        let budget = max_bytes.clamp(1, EVIDENCE_BODY_CHUNK_BYTES);
        let mut text = String::new();
        let mut position = offset;
        let first_chunk = chunks.partition_point(|chunk| chunk.end <= position);
        for (index, chunk) in chunks.iter().enumerate().skip(first_chunk) {
            if position < chunk.start || chunk.end <= chunk.start || chunk.end > byte_len {
                return Err(StoreError::Corrupt {
                    message: "invalid evidence body manifest",
                });
            }
            #[cfg(test)]
            self.faults
                .evidence_body_reads
                .fetch_add(1, Ordering::AcqRel);
            let body = self
                .store
                .document::<String>(evidence_body_chunk_key(source, index))
                .read()?
                .value
                .ok_or(StoreError::Corrupt {
                    message: "missing evidence body chunk",
                })?;
            if body.len() as u64 != chunk.end - chunk.start {
                return Err(StoreError::Corrupt {
                    message: "invalid evidence body chunk length",
                });
            }
            let local =
                usize::try_from(position - chunk.start).map_err(|_| StoreError::Corrupt {
                    message: "invalid evidence body offset",
                })?;
            if !body.is_char_boundary(local) {
                return Err(StoreError::Corrupt {
                    message: "evidence offset is not a valid page boundary",
                });
            }
            let remaining = budget - text.len();
            let mut take = remaining.min(body.len() - local);
            while take > 0 && !body.is_char_boundary(local + take) {
                take -= 1;
            }
            if take == 0 {
                return Err(StoreError::Corrupt {
                    message: "evidence page budget cannot contain the next UTF-8 character",
                });
            }
            text.push_str(&body[local..local + take]);
            position += take as u64;
            if text.len() == budget || position == byte_len {
                break;
            }
        }
        if text.is_empty() {
            return Err(StoreError::Corrupt {
                message: "evidence body manifest does not cover the requested offset",
            });
        }
        Ok(Some(StoredEvidencePage {
            metadata,
            text: Some(text),
            offset,
            next_offset: (position < byte_len).then_some(position),
        }))
    }

    pub(crate) fn result_cites(
        &self,
        result: ResultRef,
        source: EvidenceRef,
    ) -> Result<bool, StoreError> {
        Ok(self
            .stored_result(result)?
            .is_some_and(|saved| saved.evidence.contains(&source)))
    }

    pub fn history_through(&self, session: SessionId) -> Result<SessionSeq, StoreError> {
        self.store
            .journal::<StoredEvent>(journal_key(session))
            .last_sequence()
            .map(SessionSeq)
    }

    pub fn global_settings(&self) -> Result<RuntimeSettings, StoreError> {
        Ok(self.configs.user_settings())
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
            ConfigScope::Workspace(id) => {
                self.ensure_workspace_config_loaded(id)?;
                Ok(self.configs.overrides(id))
            }
            ConfigScope::Session(session) => Ok(self
                .store
                .document::<RuntimeOverrides>(session_config_key(session))
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
            ConfigScope::User => self.configs.update(scope, change),
            ConfigScope::Workspace(workspace) => {
                self.ensure_workspace_config_loaded(workspace)?;
                self.configs.update_workspace(workspace, change)
            }
            ConfigScope::Session(session) => {
                let document = self
                    .store
                    .document::<RuntimeOverrides>(session_config_key(session));
                let values = self.store.transaction(|transaction| {
                    let snapshot = transaction.read(&document)?;
                    let mut values = snapshot.value.unwrap_or_default();
                    match change {
                        ConfigChange::Model(value) => {
                            values.worker = value.clone();
                            values.coordinator = value;
                        }
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
        Ok(self.configs.profiles())
    }

    pub fn save_profile(&self, profile: Profile) -> Result<(), StoreError> {
        self.configs.save_profile(profile)
    }

    pub fn project_config_status(
        &self,
        workspace: WorkspaceId,
    ) -> Result<crate::ProjectConfigStatus, StoreError> {
        self.ensure_workspace_config_loaded(workspace)?;
        self.configs
            .project_status(workspace)
            .ok_or(StoreError::Corrupt {
                message: "workspace configuration was not loaded",
            })
    }

    pub fn reload_user_config(&self) -> Result<(), StoreError> {
        self.configs.reload_user()
    }

    pub fn reload_workspace_config(
        &self,
        workspace: WorkspaceId,
    ) -> Result<crate::ProjectConfigStatus, StoreError> {
        let info = self
            .workspace_by_id(workspace)?
            .ok_or(StoreError::Corrupt {
                message: "workspace does not exist",
            })?;
        self.configs.load_workspace(&info)
    }

    pub fn reload_config(
        &self,
        workspace: WorkspaceId,
    ) -> Result<crate::ProjectConfigStatus, StoreError> {
        let info = self
            .workspace_by_id(workspace)?
            .ok_or(StoreError::Corrupt {
                message: "workspace does not exist",
            })?;
        self.configs.reload_all(&info)
    }

    fn ensure_workspace_config_loaded(&self, workspace: WorkspaceId) -> Result<(), StoreError> {
        if self.configs.project_status(workspace).is_some() {
            return Ok(());
        }
        let info = self
            .workspace_by_id(workspace)?
            .ok_or(StoreError::Corrupt {
                message: "workspace does not exist",
            })?;
        self.configs.load_workspace(&info)?;
        Ok(())
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
            let append = self.append_session_event(
                transaction,
                session,
                &journal,
                &StoredEvent::App(event),
            )?;
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum AcceptanceError {
    #[error("result not found")]
    ResultNotFound,
    #[error("acceptance request ID was reused with different content")]
    Conflict,
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn public_agent_event(runtime: RuntimeId, record: &Record) -> Option<SessionEvent> {
    use bone_core::RecordBody;
    match &record.body {
        RecordBody::JobCreated {
            job,
            spec,
            owner,
            allowed_tools,
        } => Some(SessionEvent::JobCreated {
            allowed_tools: allowed_tools.clone(),
            job: crate::JobRef { runtime, id: job.0 },
            owner: match owner {
                bone_core::Owner::User => crate::JobOwner::User,
                bone_core::Owner::Job(parent) => crate::JobOwner::Job(crate::JobRef {
                    runtime,
                    id: parent.0,
                }),
            },
            goal: spec.goal.clone(),
            scope: spec.scope.clone(),
            done_when: spec.done_when.clone(),
        }),
        RecordBody::CallStarted {
            call,
            kind,
            job,
            tool,
        } => Some(SessionEvent::CallStarted {
            call: crate::CallRef {
                runtime,
                id: call.0,
            },
            job: job.map(|job| crate::JobRef { runtime, id: job.0 }),
            kind: crate::ActivityKind::from_call(*kind, tool.as_deref()),
        }),
        RecordBody::CallFinished {
            call,
            error,
            external_effect,
        } => Some(SessionEvent::CallFinished {
            call: crate::CallRef {
                runtime,
                id: call.0,
            },
            error: error.clone(),
            external_effect: *external_effect,
        }),
        RecordBody::Reply { inputs, text } => Some(SessionEvent::Reply {
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
        RecordBody::ConversationFailed { inputs, message } => {
            Some(SessionEvent::ConversationFailed {
                runtime,
                inputs: inputs.iter().map(|id| InputId(id.0)).collect(),
                message: message.clone(),
            })
        }
        RecordBody::JobNeedsInput { job, question } => Some(SessionEvent::JobNeedsInput {
            job: crate::JobRef { runtime, id: job.0 },
            question: question.clone(),
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
            arguments: request.arguments.clone(),
            outcome: outcome.as_ref().clone(),
        }),
        _ => None,
    }
}

fn advance_session_summary_projection(
    transaction: &WriteTransaction<'_, '_>,
    journal: &Journal<StoredEvent>,
    projection: &mut SessionSummaryProjection,
    maximum_records: usize,
    maximum_payload_bytes: usize,
) -> Result<SessionSummaryProjectionWork, StoreError> {
    let history_through = transaction.journal_last_sequence(journal)?;
    if projection.through.0 > history_through {
        return Err(StoreError::Corrupt {
            message: "session summary projection is ahead of its journal",
        });
    }
    if projection.through.0 == history_through {
        return Ok(SessionSummaryProjectionWork::default());
    }
    let page = transaction.read_after_bounded(
        journal,
        projection.through.0,
        maximum_records,
        maximum_payload_bytes,
    )?;
    let work = SessionSummaryProjectionWork {
        records: page.entries.len(),
        payload_bytes: page.payload_bytes,
    };
    let has_more = page.has_more;
    for entry in page.entries {
        apply_session_summary_event(projection, entry.sequence, &entry.event)?;
    }
    debug_assert_eq!(has_more, projection.through.0 < history_through);
    Ok(work)
}

fn apply_session_summary_event(
    projection: &mut SessionSummaryProjection,
    sequence: u64,
    event: &StoredEvent,
) -> Result<(), StoreError> {
    let expected = projection
        .through
        .0
        .checked_add(1)
        .ok_or(StoreError::RevisionExhausted)?;
    if sequence != expected {
        return Err(StoreError::Corrupt {
            message: "session summary projection is not contiguous",
        });
    }
    match event {
        StoredEvent::App(SessionEvent::InputSubmitted { .. }) => {
            projection.message_count = projection
                .message_count
                .checked_add(1)
                .ok_or(StoreError::RevisionExhausted)?;
        }
        StoredEvent::App(SessionEvent::Reply { text, .. })
        | StoredEvent::Agent {
            record:
                Record {
                    body: bone_core::RecordBody::Reply { text, .. },
                    ..
                },
            ..
        } => {
            projection.message_count = projection
                .message_count
                .checked_add(1)
                .ok_or(StoreError::RevisionExhausted)?;
            projection.latest_reply_preview = Some(bounded_reply_preview(text));
        }
        _ => {}
    }
    projection.through = SessionSeq(sequence);
    Ok(())
}

fn bounded_reply_preview(text: &str) -> String {
    if text.len() <= LATEST_REPLY_PREVIEW_BYTES {
        return text.to_owned();
    }
    let mut end = LATEST_REPLY_PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn result_summary(
    session: SessionId,
    version: SessionSeq,
    stored: &StoredEvent,
) -> Option<StoredResult> {
    let (job, outcome, summary, remaining, evidence) = match stored {
        StoredEvent::App(SessionEvent::JobFinished {
            job,
            outcome,
            summary,
            remaining,
        }) => (*job, *outcome, summary, remaining, Vec::new()),
        StoredEvent::Agent {
            runtime,
            record:
                Record {
                    body: bone_core::RecordBody::Outcome { job, outcome },
                    ..
                },
        } => (
            crate::JobRef {
                runtime: *runtime,
                id: job.0,
            },
            outcome.kind,
            &outcome.completion.summary,
            &outcome.completion.remaining,
            outcome
                .completion
                .evidence
                .iter()
                .map(|record| EvidenceRef {
                    session,
                    record: record.0,
                })
                .collect(),
        ),
        _ => return None,
    };
    Some(StoredResult {
        result: ResultRef {
            session,
            job,
            version,
        },
        outcome,
        summary: summary.clone(),
        remaining: remaining.clone(),
        evidence,
    })
}

fn evidence_source_projection(record: &Record) -> StoredEvidenceSource {
    use bone_core::RecordBody;

    match &record.body {
        RecordBody::ToolFinished {
            request, outcome, ..
        } => {
            let body = match &outcome.result {
                Ok(value) => {
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_owned())
                }
                Err(error) => error.to_string(),
            };
            StoredEvidenceSource::Public {
                kind: EvidenceSourceKind::ToolResult,
                title: request.name.clone(),
                body,
            }
        }
        RecordBody::Published { result, .. } => StoredEvidenceSource::Public {
            kind: EvidenceSourceKind::PublishedReport,
            title: "Published report".to_owned(),
            body: result.summary.clone(),
        },
        RecordBody::Reply { text, .. } => StoredEvidenceSource::Public {
            kind: EvidenceSourceKind::Reply,
            title: "Reply".to_owned(),
            body: text.clone(),
        },
        _ => StoredEvidenceSource::Private,
    }
}

fn catalog_key() -> DocumentKey {
    DocumentKey::new(NAMESPACE, "workspaces")
}

fn session_key(id: SessionId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("session/{id}"))
}

fn session_summary_projection_key(id: SessionId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("session-summary-projection/{id}"))
}

fn create_session_request_key(id: RequestId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("create-session-request/{id}"))
}

fn last_active_session_key(workspace: WorkspaceId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("last-active-session/{workspace}"))
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

fn acceptance_request_key(session: SessionId, request: AcceptanceRequestId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("acceptance-request/{session}/{request}"))
}

fn result_projection_key(session: SessionId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("result-projection/{session}"))
}

fn evidence_projection_key(session: SessionId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("evidence-projection/{session}"))
}

fn evidence_source_prefix(session: SessionId) -> String {
    format!("evidence-source/{session}/")
}

fn evidence_metadata_prefix(session: SessionId) -> String {
    format!("evidence-metadata/{session}/")
}

fn evidence_body_prefix(session: SessionId) -> String {
    format!("evidence-body/{session}/")
}

fn result_prefix(session: SessionId) -> String {
    format!("result/{session}/")
}

fn result_key(result: ResultRef) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!("{}{:020}", result_prefix(result.session), result.version.0),
    )
}

fn evidence_source_key(source: EvidenceRef) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "{}{}",
            evidence_source_prefix(source.session),
            source.record
        ),
    )
}

fn evidence_metadata_key(source: EvidenceRef) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "{}{}",
            evidence_metadata_prefix(source.session),
            source.record
        ),
    )
}

fn evidence_body_chunk_key(source: EvidenceRef, index: usize) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "{}{}:{index:06}",
            evidence_body_prefix(source.session),
            source.record
        ),
    )
}

fn attention_projection_state_key() -> DocumentKey {
    DocumentKey::new(NAMESPACE, "attention-projection")
}

fn attention_workspace_prefix(workspace: WorkspaceId) -> String {
    format!("attention/{workspace}/")
}

fn attention_input_key(workspace: WorkspaceId, session: SessionId, input: InputId) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "{}input/{session}/{:020}",
            attention_workspace_prefix(workspace),
            input.0
        ),
    )
}

fn attention_write_key(workspace: WorkspaceId, call: CallRef) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "{}write/{}/{:020}",
            attention_workspace_prefix(workspace),
            call.runtime,
            call.id
        ),
    )
}

fn session_from_input_key(key: &str) -> Result<SessionId, StoreError> {
    let mut parts = key.split('/');
    if parts.next() != Some("input") {
        return Err(StoreError::Corrupt {
            message: "input document has an invalid key",
        });
    }
    let session = parts.next().ok_or(StoreError::Corrupt {
        message: "input document key has no session",
    })?;
    let parsed = uuid::Uuid::parse_str(session).map_err(|_| StoreError::Corrupt {
        message: "input document key has an invalid session",
    })?;
    Ok(SessionId::from_uuid(parsed))
}

fn write_view(workspace: WorkspaceId, attempt: &WriteAttempt) -> UnresolvedWriteView {
    let (status, outcome) = match &attempt.state {
        WriteState::Pending => (UnresolvedWriteStatus::Pending, None),
        WriteState::Finished(outcome) => (UnresolvedWriteStatus::Finished, Some(outcome.clone())),
    };
    UnresolvedWriteView {
        workspace,
        session: attempt.session,
        call: attempt.call,
        job: attempt.job,
        status,
        tool: attempt.tool.clone(),
        arguments: attempt.arguments.clone(),
        outcome,
    }
}

fn acceptance_prefix(result: ResultRef) -> String {
    format!("acceptance/{}/{:020}/", result.session, result.version.0)
}

fn acceptance_key(record: &AcceptanceRecord) -> DocumentKey {
    DocumentKey::new(
        NAMESPACE,
        format!(
            "{}{:020}/{}",
            acceptance_prefix(record.result),
            record.saved_at.0,
            record.id
        ),
    )
}

fn journal_key(session: SessionId) -> JournalKey {
    JournalKey::new(format!("app/session/{session}"))
}

fn session_config_key(session: SessionId) -> DocumentKey {
    DocumentKey::new(NAMESPACE, format!("config/session/{session}"))
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
    use crate::{ActivityKind, JobOwner, ModelSelection, ProfileId};
    use bone_adapters::tools::BashOutput;

    #[test]
    fn public_history_preserves_call_arguments_for_presentation() {
        let runtime = RuntimeId::new();
        let job = bone_core::JobId(3);
        let call = bone_core::CallId(7);
        let spec = bone_core::JobSpec::new("inspect", "one file", "report findings");
        let created = Record {
            seq: bone_core::Seq(1),
            origin: bone_core::Origin::Kernel,
            body: bone_core::RecordBody::JobCreated {
                allowed_tools: std::collections::BTreeSet::from(["bash".into()]),
                job,
                spec,
                owner: bone_core::Owner::User,
            },
        };
        assert!(matches!(
            public_agent_event(runtime, &created),
            Some(SessionEvent::JobCreated {
                job: JobRef { id: 3, .. },
                owner: JobOwner::User,
                ..
            })
        ));

        let started = Record {
            seq: bone_core::Seq(2),
            origin: bone_core::Origin::Call(call),
            body: bone_core::RecordBody::CallStarted {
                call,
                kind: bone_core::CallKind::Tool,
                job: Some(job),
                tool: Some(Arc::new(bone_core::ToolCall::new(
                    "bash",
                    serde_json::json!({"command": "secret argument"}),
                ))),
            },
        };
        assert_eq!(
            public_agent_event(runtime, &started),
            Some(SessionEvent::CallStarted {
                call: CallRef { runtime, id: 7 },
                job: Some(JobRef { runtime, id: 3 }),
                kind: ActivityKind::Tool {
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "secret argument"}),
                },
            })
        );

        let arguments = serde_json::json!({"command": "echo 中文", "cwd":"src"});
        let tool_finished = Record {
            seq: bone_core::Seq(3),
            origin: bone_core::Origin::Call(call),
            body: bone_core::RecordBody::ToolFinished {
                call,
                job,
                request: Arc::new(bone_core::ToolCall::new("bash", arguments.clone())),
                outcome: Arc::new(ToolOutcome::value(
                    serde_json::json!({"stdout":"中文\n","stderr":"","exit_code":0,"timed_out":false,"truncated":false}),
                )),
            },
        };
        let Some(SessionEvent::ToolFinished {
            tool,
            arguments: saved,
            outcome,
            ..
        }) = public_agent_event(runtime, &tool_finished)
        else {
            panic!("tool completion must be projected");
        };
        assert_eq!(saved, arguments);
        let detail = crate::tool_details(&tool, &saved, &outcome);
        assert!(
            detail
                .sections
                .iter()
                .any(|section| section.text == "中文\n")
        );

        let finished = Record {
            seq: bone_core::Seq(3),
            origin: bone_core::Origin::Call(call),
            body: bone_core::RecordBody::CallFinished {
                call,
                error: None,
                external_effect: ExternalEffect::Applied,
            },
        };
        assert_eq!(
            public_agent_event(runtime, &finished),
            Some(SessionEvent::CallFinished {
                call: CallRef { runtime, id: 7 },
                error: None,
                external_effect: ExternalEffect::Applied,
            })
        );
    }

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

    fn delete_session_summary_projection(store: &DataStore, session: SessionId) {
        let projection = store
            .store
            .document::<SessionSummaryProjection>(session_summary_projection_key(session));
        store
            .store
            .transaction(|transaction| {
                let snapshot = transaction.read(&projection)?;
                transaction.delete(&projection, snapshot.revision)
            })
            .unwrap();
    }

    #[test]
    fn workspace_overview_backfills_exact_navigation_summaries_for_legacy_sessions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let store = DataStore::open(temporary.path().join("data")).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let active = store.create_session(workspace.id, "active".into()).unwrap();
        let empty = store.create_session(workspace.id, "empty".into()).unwrap();

        for session in [active.info.id, empty.info.id] {
            delete_session_summary_projection(&store, session);
        }

        let journal = store
            .store
            .journal::<StoredEvent>(journal_key(active.info.id));
        let runtime = RuntimeId::new();
        journal
            .append(&StoredEvent::App(SessionEvent::InputSubmitted {
                input: InputId(1),
                request_id: RequestId::new(),
                text: "question".into(),
                reply_to: None,
            }))
            .unwrap();
        journal
            .append(&StoredEvent::Agent {
                runtime,
                record: Record {
                    seq: bone_core::Seq(1),
                    origin: bone_core::Origin::Kernel,
                    body: bone_core::RecordBody::Reply {
                        inputs: vec![bone_core::InputId(1)],
                        text: "older reply".into(),
                    },
                },
            })
            .unwrap();
        let latest = "界".repeat(400);
        journal
            .append(&StoredEvent::Agent {
                runtime,
                record: Record {
                    seq: bone_core::Seq(2),
                    origin: bone_core::Origin::Kernel,
                    body: bone_core::RecordBody::Reply {
                        inputs: vec![bone_core::InputId(1)],
                        text: latest.clone(),
                    },
                },
            })
            .unwrap();

        let overview = store.workspace_overview(workspace.clone()).unwrap();
        let active_summary = overview
            .sessions
            .iter()
            .find(|summary| summary.session.id == active.info.id)
            .unwrap();
        assert!(active_summary.created_at > 0);
        assert_eq!(active_summary.message_count, 3);
        assert!(!active_summary.projection_pending);
        assert_eq!(
            active_summary.latest_reply_preview.as_deref(),
            Some(bounded_reply_preview(&latest).as_str())
        );
        assert!(
            active_summary.latest_reply_preview.as_ref().unwrap().len()
                <= LATEST_REPLY_PREVIEW_BYTES
        );

        let empty_summary = overview
            .sessions
            .iter()
            .find(|summary| summary.session.id == empty.info.id)
            .unwrap();
        assert!(empty_summary.created_at > 0);
        assert_eq!(empty_summary.message_count, 0);
        assert_eq!(empty_summary.latest_reply_preview, None);
        assert!(!empty_summary.projection_pending);

        let persisted = store
            .store
            .document::<SessionSummaryProjection>(session_summary_projection_key(active.info.id))
            .read()
            .unwrap()
            .value
            .unwrap();
        assert_eq!(persisted.through, SessionSeq(3));
        assert_eq!(persisted.message_count, 3);

        drop(store);
        let reopened = DataStore::open(temporary.path().join("data")).unwrap();
        let second = reopened.workspace_overview(workspace).unwrap();
        assert_eq!(overview.sessions, second.sessions);
    }

    #[test]
    fn session_summary_backfill_is_globally_record_bounded_and_resumable() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let data = temporary.path().join("data");
        let store = DataStore::open(&data).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let sessions = [
            store.create_session(workspace.id, "first".into()).unwrap(),
            store.create_session(workspace.id, "second".into()).unwrap(),
        ];
        for session in &sessions {
            delete_session_summary_projection(&store, session.info.id);
            let journal = store
                .store
                .journal::<StoredEvent>(journal_key(session.info.id));
            for input in 1..=40 {
                journal
                    .append(&StoredEvent::App(SessionEvent::InputSubmitted {
                        input: InputId(input),
                        request_id: RequestId::new(),
                        text: format!("message {input}"),
                        reply_to: None,
                    }))
                    .unwrap();
            }
        }

        let first = store.workspace_overview(workspace.clone()).unwrap();
        assert_eq!(
            first
                .sessions
                .iter()
                .map(|summary| summary.message_count)
                .sum::<u64>(),
            SESSION_SUMMARY_BACKFILL_RECORDS as u64
        );
        assert!(
            first
                .sessions
                .iter()
                .any(|summary| summary.projection_pending)
        );

        drop(store);
        let reopened = DataStore::open(&data).unwrap();
        let second = reopened.workspace_overview(workspace.clone()).unwrap();
        assert_eq!(
            second
                .sessions
                .iter()
                .map(|summary| summary.message_count)
                .sum::<u64>(),
            80
        );
        assert!(
            second
                .sessions
                .iter()
                .all(|summary| !summary.projection_pending)
        );

        let stable = reopened.workspace_overview(workspace).unwrap();
        assert_eq!(second.sessions, stable.sessions);
    }

    #[test]
    fn stale_summary_projection_does_not_expand_append_work_and_obeys_byte_budget() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let data = temporary.path().join("data");
        let store = DataStore::open(&data).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let session = store
            .create_session(workspace.id, "large legacy history".into())
            .unwrap();
        delete_session_summary_projection(&store, session.info.id);

        let journal = store
            .store
            .journal::<StoredEvent>(journal_key(session.info.id));
        let large_body = "x".repeat(1024 * 1024);
        let runtime = RuntimeId::new();
        let mut encoded_sizes = Vec::new();
        let mut latest_reply = String::new();
        for sequence in 1..=12 {
            latest_reply = format!("reply {sequence} {large_body}");
            let event = StoredEvent::Agent {
                runtime,
                record: Record {
                    seq: bone_core::Seq(sequence),
                    origin: bone_core::Origin::Kernel,
                    body: bone_core::RecordBody::Reply {
                        inputs: vec![bone_core::InputId(1)],
                        text: latest_reply.clone(),
                    },
                },
            };
            encoded_sizes.push(serde_json::to_vec(&event).unwrap().len());
            journal.append(&event).unwrap();
        }

        store
            .accept_input(session.info.id, &SubmitInput::new("new message"))
            .unwrap();
        assert!(
            store
                .store
                .document::<SessionSummaryProjection>(session_summary_projection_key(
                    session.info.id
                ))
                .read()
                .unwrap()
                .value
                .is_none(),
            "an ordinary append must not migrate stale history while holding the writer"
        );

        let first = store.workspace_overview(workspace.clone()).unwrap();
        let summary = first
            .sessions
            .iter()
            .find(|summary| summary.session.id == session.info.id)
            .unwrap();
        assert!(summary.projection_pending);
        let projection = store
            .store
            .document::<SessionSummaryProjection>(session_summary_projection_key(session.info.id))
            .read()
            .unwrap()
            .value
            .unwrap();
        let processed = projection.through.0 as usize;
        assert!(processed > 0 && processed < encoded_sizes.len());
        let materialized = encoded_sizes[..processed].iter().sum::<usize>();
        assert!(materialized <= SESSION_SUMMARY_BACKFILL_BYTES);
        assert!(
            materialized + encoded_sizes[processed] > SESSION_SUMMARY_BACKFILL_BYTES,
            "the next payload, rather than a record-count boundary, must stop this slice"
        );

        drop(store);
        let reopened = DataStore::open(&data).unwrap();
        let mut overview = first;
        for _ in 0..4 {
            if overview
                .sessions
                .iter()
                .all(|summary| !summary.projection_pending)
            {
                break;
            }
            overview = reopened.workspace_overview(workspace.clone()).unwrap();
        }
        let summary = overview
            .sessions
            .iter()
            .find(|summary| summary.session.id == session.info.id)
            .unwrap();
        assert!(!summary.projection_pending);
        assert_eq!(summary.message_count, 13);
        assert_eq!(
            summary.latest_reply_preview.as_deref(),
            Some(bounded_reply_preview(&latest_reply).as_str())
        );
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
    fn model_config_change_replaces_both_roles_at_every_scope() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let store = DataStore::open(temporary.path().join("data")).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let session = store
            .create_session(workspace.id, "session".into())
            .unwrap();

        for scope in [
            ConfigScope::User,
            ConfigScope::Workspace(workspace.id),
            ConfigScope::Session(session.info.id),
        ] {
            store
                .update_config(scope, ConfigChange::Worker(Some(selection("old-worker"))))
                .unwrap();
            store
                .update_config(
                    scope,
                    ConfigChange::Coordinator(Some(selection("old-coordinator"))),
                )
                .unwrap();

            let selected = selection("selected");
            let saved = store
                .update_config(scope, ConfigChange::Model(Some(selected.clone())))
                .unwrap();
            assert_eq!(saved.worker, Some(selected.clone()));
            assert_eq!(saved.coordinator, Some(selected.clone()));
            assert_eq!(store.config(scope).unwrap(), saved);

            let cleared = store
                .update_config(scope, ConfigChange::Model(None))
                .unwrap();
            assert_eq!(cleared.worker, None);
            assert_eq!(cleared.coordinator, None);
            assert_eq!(store.config(scope).unwrap(), cleared);
        }
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

    #[test]
    fn project_config_loads_on_open_and_reload() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        let project_dir = root.join(".bone");
        std::fs::create_dir_all(&project_dir).unwrap();
        let config_path = project_dir.join("config.toml");
        std::fs::write(
            &config_path,
            "schema_version = 1\n[overrides.worker]\nprofile = \"chatgpt\"\nmodel = \"gpt-5.6-terra\"\n",
        )
        .unwrap();
        let data = temporary.path().join("data");

        let store = DataStore::open(&data).unwrap();
        let workspace = store.workspace(&root).unwrap();
        assert_eq!(
            store
                .config(ConfigScope::Workspace(workspace.id))
                .unwrap()
                .worker
                .unwrap()
                .model,
            "gpt-5.6-terra"
        );
        drop(store);

        let reopened = DataStore::open(&data).unwrap();
        let same_workspace = reopened.workspace(&root).unwrap();
        assert_eq!(same_workspace.id, workspace.id);
        assert_eq!(
            reopened
                .config(ConfigScope::Workspace(workspace.id))
                .unwrap()
                .worker
                .unwrap()
                .model,
            "gpt-5.6-terra"
        );

        std::fs::write(&config_path, "schema_version = 1\n[overrides]\n").unwrap();
        reopened.reload_workspace_config(workspace.id).unwrap();
        assert!(
            reopened
                .config(ConfigScope::Workspace(workspace.id))
                .unwrap()
                .worker
                .is_none()
        );
    }

    #[test]
    fn legacy_sqlite_user_workspace_and_profile_documents_are_ignored() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let store = DataStore::open(temporary.path().join("data")).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let selection = ModelSelection::new(ProfileId::chatgpt(), "legacy-model").unwrap();
        let settings = RuntimeSettings {
            worker: Some(selection.clone()),
            ..RuntimeSettings::default()
        };
        let overrides = RuntimeOverrides {
            worker: Some(selection),
            ..RuntimeOverrides::default()
        };
        let old_user = store
            .store
            .document::<RuntimeSettings>(DocumentKey::new(NAMESPACE, "config/user"));
        let old_workspace = store.store.document::<RuntimeOverrides>(DocumentKey::new(
            NAMESPACE,
            format!("config/workspace/{}", workspace.id),
        ));
        let old_profiles = store
            .store
            .document::<Vec<Profile>>(DocumentKey::new(NAMESPACE, "profiles"));
        store
            .store
            .transaction(|transaction| {
                let user = transaction.read(&old_user)?;
                let project = transaction.read(&old_workspace)?;
                let profiles = transaction.read(&old_profiles)?;
                transaction.replace(&old_user, &settings, user.revision)?;
                transaction.replace(&old_workspace, &overrides, project.revision)?;
                transaction.replace(
                    &old_profiles,
                    &vec![
                        Profile::new(
                            ProfileId::new("legacy").unwrap(),
                            "Legacy",
                            bone_adapters::llm::EndpointConfig::OpenAiResponses { base_url: None },
                        )
                        .unwrap(),
                    ],
                    profiles.revision,
                )?;
                Ok(())
            })
            .unwrap();

        assert!(store.config(ConfigScope::User).unwrap().worker.is_none());
        assert!(
            store
                .config(ConfigScope::Workspace(workspace.id))
                .unwrap()
                .worker
                .is_none()
        );
        assert_eq!(store.profiles().unwrap(), vec![Profile::chatgpt()]);
    }
}
