use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    CallId, CallKind, CallProgress, CallView, Input, InputId, JobId, JobSpec, JobStatus,
    MonoTimeView, Owner, ReadQuery, ReportDraft, Seq, ToolCall, ToolOutcome, ToolSpec, WaitView,
    job::JobState,
    kernel::{Kernel, Requester},
};

pub(crate) const DIRECTORY_PAGE: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    User(InputId),
    Job { job: JobId, revision: u64 },
    Call(CallId),
    Kernel,
}

/// Immutable session fact. Large bodies live here once; jobs retain only `Seq`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub seq: Seq,
    pub origin: Origin,
    pub body: RecordBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RecordBody {
    Input(Input),
    RoutingStarted {
        inputs: Vec<InputId>,
        source: Option<JobId>,
    },
    RoutingFinished {
        routing: Seq,
    },
    RoutingHandoff {
        job: JobId,
        inputs: Vec<InputId>,
        text: String,
        #[serde(deserialize_with = "serde::Deserialize::deserialize")]
        previous: Option<Seq>,
    },
    JobCreated {
        job: JobId,
        spec: JobSpec,
        owner: Owner,
    },
    JobChanged {
        job: JobId,
        spec: JobSpec,
        revision: u64,
    },
    /// The job's local pause flag changed. An unpaused child may still be
    /// effectively paused by one of its owners.
    JobControlChanged {
        job: JobId,
        paused: bool,
    },
    ConstraintsChanged {
        source: InputId,
        revision: u64,
        constraints: String,
    },
    Note {
        job: JobId,
        text: String,
    },
    Report {
        job: JobId,
        report: ReportDraft,
    },
    ToolFinished {
        job: JobId,
        call: CallId,
        request: Arc<ToolCall>,
        outcome: Arc<ToolOutcome>,
    },
    Published {
        job: JobId,
        result: ReportDraft,
    },
    Outcome {
        job: JobId,
        outcome: Arc<crate::JobOutcome>,
    },
    Inquiry {
        requester: DeliveryTarget,
        target: JobId,
        question: String,
    },
    InquirySettled {
        inquiry: Seq,
        result: InquiryResult,
    },
    Delivery {
        to: DeliveryTarget,
        source: Seq,
        kind: DeliveryKind,
    },
    ReadResult {
        requester: DeliveryTarget,
        query: ReadQuery,
        next_job: Option<JobId>,
        jobs: Vec<JobCard>,
        record: Option<RecordRange>,
    },
    ImportedMemory {
        source_job: JobId,
        source_revision: u64,
        summary: String,
        record_refs: Vec<Seq>,
    },
    SessionCheckpoint {
        checkpoint: Arc<SessionCheckpoint>,
    },
    Checkpoint {
        job: JobId,
        checkpoint: Arc<Checkpoint>,
    },
    Reply {
        job: JobId,
        inputs: Vec<InputId>,
        text: String,
    },
    Clarification {
        inputs: Vec<InputId>,
        question: String,
    },
    InputRoutingFailed {
        inputs: Vec<InputId>,
        message: String,
    },
    InputFinished {
        input: InputId,
        outcome: crate::InputOutcome,
    },
    CallStarted {
        call: CallId,
        kind: CallKind,
        job: Option<JobId>,
        tool: Option<Arc<ToolCall>>,
    },
    CallProgress {
        call: CallId,
        job: Option<JobId>,
        progress: CallProgress,
    },
    CallFinished {
        call: CallId,
        error: Option<crate::CallError>,
        external_effect: crate::ExternalEffect,
    },
    Audit {
        message: String,
    },
    /// A side-effect-free refusal delivered back to the same Job for correction.
    WorkRejected {
        job: JobId,
        call: CallId,
        message: String,
        budget: crate::WorkRejections,
    },
    Delegated {
        job: JobId,
        call: CallId,
        children: Vec<JobId>,
        continuation: crate::AfterDelegation,
    },
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InquiryResult {
    Answer(ReportDraft),
    NeedsWork(String),
    Unavailable(String),
    Finished(Seq),
    TimedOut,
    Cancelled,
    Changed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryTarget {
    Job(JobId),
    Routing(Seq),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryKind {
    Input,
    ChildCreated,
    Result,
    Outcome,
    Inquiry,
    InquiryResult,
    Read,
    Coordination,
    DependencyChanged,
    Memory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRange {
    pub source: Seq,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub job: JobId,
    pub revision: u64,
    pub through: Seq,
    pub summary: String,
    pub evidence: Vec<Seq>,
}

/// A derived summary of the public session prefix. Raw records remain authoritative.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub through: Seq,
    pub summary: String,
    pub evidence: Vec<Seq>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactScope {
    Session,
    Job { job: JobId, revision: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointDraft {
    pub summary: String,
    pub evidence: Vec<Seq>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordView {
    pub source: Seq,
    pub origin: Origin,
    /// UTF-8 byte offset in the authoritative serialized [`RecordBody`].
    pub offset: usize,
    /// The offset for [`ReadQuery::Record`], or `None` when this view reaches
    /// the end of the authoritative body.
    pub next_offset: Option<usize>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCard {
    pub id: JobId,
    pub spec: JobSpec,
    pub status: JobStatus,
    pub report: Option<ReportDraft>,
    /// Read this record, then its `previous` links, for assigned inputs and intent.
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub latest_handoff: Option<Seq>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InquiryView {
    pub id: Seq,
    pub requester: DeliveryTarget,
    pub question: String,
    pub deadline: MonoTimeView,
}

/// Read-only projection of the public session summary and record tail.
/// Current input delivery and constraints remain separate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContext {
    pub entries: Vec<BackgroundEntry>,
}

/// One labeled historical item in [`SessionContext`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundEntry {
    pub label: String,
    pub content: String,
}

impl BackgroundEntry {
    pub fn new(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoordinateInput {
    pub routing: Seq,
    pub inputs: Vec<Input>,
    pub source: Option<JobId>,
    pub request: Option<String>,
    pub constraints: String,
    pub background: Arc<SessionContext>,
    pub jobs: Vec<JobCard>,
    /// Pass this value as `ReadQuery::Jobs.after` to continue the root directory.
    pub next_job: Option<JobId>,
    pub records: Vec<RecordView>,
}

/// The authority carried by one worker call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerRole {
    User,
    Delegated,
    Investigation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkInput {
    pub job: JobId,
    pub revision: u64,
    pub role: WorkerRole,
    /// Whether `AskUser` is available for this particular user-owned turn.
    pub can_ask_user: bool,
    /// Effective remaining delegation capacity, including ancestor ceilings.
    pub delegation: crate::DelegationLimits,
    pub spec: JobSpec,
    /// Input IDs this job may pass to a delegated child.
    pub inputs: Vec<InputId>,
    pub constraints: String,
    pub constraints_revision: u64,
    pub background: Arc<SessionContext>,
    pub waiting: Option<WaitView>,
    pub checkpoint: Option<Arc<Checkpoint>>,
    pub children: Vec<JobCard>,
    pub inquiries: Vec<InquiryView>,
    pub calls: Vec<CallView>,
    pub records: Vec<RecordView>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactInput {
    pub scope: CompactScope,
    pub previous: Option<Arc<SessionCheckpoint>>,
    pub output_bytes: usize,
    pub through: Seq,
    pub records: Vec<RecordView>,
}

pub(crate) enum PreparedWork {
    Work {
        input: Box<WorkInput>,
        seen_through: Seq,
    },
    Compact(CompactInput),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ContextError {
    #[error("job {0} does not exist")]
    MissingJob(JobId),
    #[error("routing {0} does not exist")]
    MissingRouting(Seq),
    #[error("record {0} is unavailable")]
    MissingRecord(Seq),
    #[error("record offset is not a UTF-8 boundary")]
    InvalidOffset,
    #[error("required context exceeds the configured model input budget")]
    TooLarge,
    #[error("there is no read prefix to compact")]
    NothingToCompact,
}

pub(crate) fn prepare_work(kernel: &Kernel, job_id: JobId) -> Result<PreparedWork, ContextError> {
    let mut page_bytes = initial_page_bytes(kernel);
    let (input, seen_through) = work_input(kernel, job_id, page_bytes)?;
    if encoded_len(&input) <= kernel.limits.context_bytes {
        return Ok(PreparedWork::Work {
            input: Box::new(input),
            seen_through,
        });
    }
    // A newly required record cannot be compacted before a worker has seen it.
    // Tool outcomes and explicit record reads are pageable, so reduce their
    // projection until the complete DTO fits. The authoritative record remains
    // unchanged and the next byte offset is carried by RecordView.
    while page_bytes > 1 {
        page_bytes = (page_bytes / 2).max(1);
        let (input, seen_through) = work_input(kernel, job_id, page_bytes)?;
        if encoded_len(&input) <= kernel.limits.context_bytes {
            return Ok(PreparedWork::Work {
                input: Box::new(input),
                seen_through,
            });
        }
    }
    // Compact only after shrinking pageable observations. Prefer the larger scope.
    let (input, _) = work_input(kernel, job_id, 1)?;
    let background_bytes = encoded_len(&input.background);
    let mut without_background = input.clone();
    without_background.background = Arc::new(SessionContext::default());
    let background_can_help = encoded_len(&without_background) <= kernel.limits.context_bytes;
    if background_can_help
        && background_bytes > kernel.limits.context_bytes / 3
        && let Ok(compact) = prepare_session_compact(kernel)
    {
        return Ok(PreparedWork::Compact(compact));
    }
    match prepare_compact(kernel, job_id) {
        Ok(input) => return Ok(PreparedWork::Compact(input)),
        Err(ContextError::NothingToCompact) => {}
        Err(error) => return Err(error),
    }

    if background_can_help && let Ok(compact) = prepare_session_compact(kernel) {
        return Ok(PreparedWork::Compact(compact));
    }
    Err(ContextError::TooLarge)
}

fn work_input(
    kernel: &Kernel,
    job_id: JobId,
    page_bytes: usize,
) -> Result<(WorkInput, Seq), ContextError> {
    let job = kernel
        .jobs
        .get(&job_id)
        .ok_or(ContextError::MissingJob(job_id))?;
    let checkpoint_through = job
        .context
        .checkpoint
        .as_ref()
        .map_or(Seq::ZERO, |checkpoint| checkpoint.through);
    let local = job
        .context
        .records
        .iter()
        .copied()
        .filter(|seq| *seq > checkpoint_through)
        .collect::<Vec<_>>();
    let records = expand_records(kernel, &local, page_bytes)?;
    let seen_through = local.last().copied().unwrap_or(job.context.read_through);
    let role = if kernel.is_investigation(job_id) {
        WorkerRole::Investigation
    } else if matches!(job.owner, Owner::User) {
        WorkerRole::User
    } else {
        WorkerRole::Delegated
    };
    let input = WorkInput {
        job: job_id,
        revision: job.revision,
        role,
        delegation: kernel.delegation_capacity(job_id),
        can_ask_user: role == WorkerRole::User
            && job
                .inputs
                .iter()
                .any(|input| kernel.inputs[input].finished.is_none()),
        spec: job.spec.clone(),
        inputs: job.inputs.clone(),
        constraints: kernel.constraints.clone(),
        constraints_revision: kernel.constraints_revision,
        background: session_background(
            kernel,
            &records
                .iter()
                .map(|record| record.source)
                .collect::<Vec<_>>(),
        ),
        waiting: match &job.state {
            JobState::Waiting(wait) => Some(wait.view()),
            _ => None,
        },
        checkpoint: job.context.checkpoint.clone(),
        children: kernel
            .children_of(job_id)
            .filter(|id| !matches!(kernel.jobs[id].state, JobState::Finished(_)))
            .map(|id| card(kernel, id))
            .collect(),
        inquiries: kernel
            .inquiries
            .iter()
            .filter(|(_, inquiry)| inquiry.target == job_id)
            .map(|(id, inquiry)| InquiryView {
                id: *id,
                requester: inquiry.requester,
                question: match &kernel.records[id].body {
                    RecordBody::Inquiry { question, .. } => question.clone(),
                    _ => unreachable!("an inquiry points to its record"),
                },
                deadline: inquiry.deadline.into(),
            })
            .collect(),
        calls: kernel.calls_for(job_id),
        records,
        tools: kernel
            .tools
            .values()
            .filter(|tool| {
                role != WorkerRole::Investigation || tool.effect == crate::ToolEffect::ReadOnly
            })
            .cloned()
            .collect(),
    };
    Ok((input, seen_through))
}

pub(crate) fn prepare_coordinate(
    kernel: &Kernel,
    routing_id: Seq,
) -> Result<CoordinateInput, ContextError> {
    let (mut input, has_read_result) = coordinate_input(kernel, routing_id, None)?;
    if encoded_len(&input) > kernel.limits.context_bytes {
        return Err(ContextError::TooLarge);
    }
    if has_read_result {
        return Ok(input);
    }

    let roots = kernel
        .jobs
        .keys()
        .copied()
        .filter(|id| {
            matches!(kernel.jobs[id].owner, Owner::User)
                && !matches!(kernel.jobs[id].state, JobState::Finished(_))
        })
        .collect::<Vec<_>>();
    for (index, id) in roots.iter().take(DIRECTORY_PAGE).enumerate() {
        let mut candidate = input.clone();
        candidate.jobs.push(card(kernel, *id));
        candidate.next_job = (index + 1 < roots.len()).then_some(*id);
        if encoded_len(&candidate) > kernel.limits.context_bytes {
            break;
        }
        input = candidate;
    }
    if input.jobs.is_empty() && !roots.is_empty() {
        return Err(ContextError::TooLarge);
    }
    Ok(input)
}

pub(crate) fn session_can_help_coordinate(kernel: &Kernel, routing: Seq) -> bool {
    coordinate_input(kernel, routing, None).is_ok_and(|(mut input, _)| {
        input.background = Arc::new(SessionContext::default());
        encoded_len(&input) <= kernel.limits.context_bytes
    })
}

pub(crate) fn coordinate_read_fits(
    kernel: &Kernel,
    routing_id: Seq,
    candidate: &Record,
) -> Result<bool, ContextError> {
    let (input, _) = coordinate_input(kernel, routing_id, Some(candidate))?;
    Ok(encoded_len(&input) <= kernel.limits.context_bytes)
}

fn coordinate_input(
    kernel: &Kernel,
    routing_id: Seq,
    read_override: Option<&Record>,
) -> Result<(CoordinateInput, bool), ContextError> {
    let routing = kernel
        .routings
        .get(&routing_id)
        .ok_or(ContextError::MissingRouting(routing_id))?;
    let latest_read = read_override.is_none().then(|| {
        routing.records.iter().rev().find(|seq| {
            **seq > routing_id
                && kernel
                    .records
                    .get(seq)
                    .is_some_and(|record| matches!(record.body, RecordBody::ReadResult { .. }))
        })
    });
    let latest_read = latest_read.flatten().copied();
    let projected = routing
        .records
        .iter()
        .copied()
        .filter(|seq| {
            !kernel
                .records
                .get(seq)
                .is_some_and(|record| matches!(record.body, RecordBody::ReadResult { .. }))
                || Some(*seq) == latest_read
        })
        .collect::<Vec<_>>();
    let mut positions = BTreeMap::new();
    let mut records = Vec::new();
    expand_into(
        kernel,
        &projected,
        kernel.limits.item_bytes,
        &mut positions,
        &mut records,
    )?;
    if let Some(candidate) = read_override {
        push_record_view(candidate, 0, usize::MAX, &mut positions, &mut records)?;
        if let RecordBody::ReadResult {
            record: Some(range),
            ..
        } = &candidate.body
        {
            push_view(
                kernel,
                range.source,
                range.offset,
                kernel.limits.item_bytes,
                &mut positions,
                &mut records,
            )?;
        }
    }
    let input = CoordinateInput {
        routing: routing_id,
        inputs: routing
            .inputs
            .iter()
            .filter_map(|id| {
                kernel
                    .inputs
                    .get(id)
                    .map(|entry| kernel.input(entry.accepted_at).clone())
            })
            .collect(),
        source: match routing.requester {
            Requester::Inputs => None,
            Requester::Job { job, .. } => Some(job),
        },
        request: routing.request.clone(),
        constraints: kernel.constraints.clone(),
        background: session_background(
            kernel,
            &routing
                .inputs
                .iter()
                .filter_map(|id| kernel.inputs.get(id).map(|input| input.accepted_at))
                .chain(records.iter().map(|record| record.source))
                .collect::<Vec<_>>(),
        ),
        jobs: Vec::new(),
        next_job: None,
        records,
    };
    Ok((input, read_override.is_some() || latest_read.is_some()))
}

pub(crate) fn prepare_compact(
    kernel: &Kernel,
    job_id: JobId,
) -> Result<CompactInput, ContextError> {
    let job = kernel
        .jobs
        .get(&job_id)
        .ok_or(ContextError::MissingJob(job_id))?;
    let after = job
        .context
        .checkpoint
        .as_ref()
        .map_or(Seq::ZERO, |checkpoint| checkpoint.through);
    let candidates = job
        .context
        .records
        .iter()
        .copied()
        .filter(|seq| *seq > after && *seq <= job.context.read_through);
    let mut records = Vec::new();
    let mut positions = BTreeMap::new();
    let mut through = after;
    'records: for seq in candidates {
        let next_through = seq;
        let mut page_bytes = initial_page_bytes(kernel);
        loop {
            let mut trial = records.clone();
            let mut trial_positions = positions.clone();
            expand_into(kernel, &[seq], page_bytes, &mut trial_positions, &mut trial)?;
            let input = CompactInput {
                scope: CompactScope::Job {
                    job: job_id,
                    revision: job.revision,
                },
                previous: job.context.checkpoint.as_ref().map(|cp| {
                    Arc::new(SessionCheckpoint {
                        through: cp.through,
                        summary: cp.summary.clone(),
                        evidence: cp.evidence.clone(),
                    })
                }),
                output_bytes: compact_output_bytes(kernel),
                through: next_through,
                records: trial.clone(),
            };
            if encoded_len(&input) <= kernel.limits.context_bytes {
                records = trial;
                positions = trial_positions;
                through = next_through;
                break;
            }
            if through != after || page_bytes == 1 || !pageable(kernel, seq)? {
                break 'records;
            }
            page_bytes = (page_bytes / 2).max(1);
        }
    }
    if through == after {
        return Err(ContextError::NothingToCompact);
    }
    Ok(CompactInput {
        scope: CompactScope::Job {
            job: job_id,
            revision: job.revision,
        },
        previous: job.context.checkpoint.as_ref().map(|cp| {
            Arc::new(SessionCheckpoint {
                through: cp.through,
                summary: cp.summary.clone(),
                evidence: cp.evidence.clone(),
            })
        }),
        output_bytes: compact_output_bytes(kernel),
        through,
        records,
    })
}

pub(crate) fn session_public(kernel: &Kernel, record: &Record) -> bool {
    match &record.body {
        RecordBody::Input(_) | RecordBody::Reply { .. } | RecordBody::Clarification { .. } => true,
        RecordBody::Published { job, .. } | RecordBody::Outcome { job, .. } => kernel
            .jobs
            .get(job)
            .is_some_and(|job| job.owner == Owner::User),
        _ => false,
    }
}

pub(crate) fn session_checkpoint(kernel: &Kernel) -> Option<Arc<SessionCheckpoint>> {
    kernel
        .records
        .values()
        .rev()
        .find_map(|record| match &record.body {
            RecordBody::SessionCheckpoint { checkpoint } => Some(checkpoint.clone()),
            _ => None,
        })
}

fn session_background(kernel: &Kernel, excluded: &[Seq]) -> Arc<SessionContext> {
    let mut background = SessionContext::default();
    let checkpoint = session_checkpoint(kernel);
    let through = checkpoint
        .as_ref()
        .map_or(Seq::ZERO, |checkpoint| checkpoint.through);
    if let Some(checkpoint) = checkpoint {
        background.entries.push(BackgroundEntry::new(
            format!(
                "Session summary through {}; read source records for exact details",
                checkpoint.through
            ),
            checkpoint.summary.clone(),
        ));
    }
    for record in kernel.records.values().filter(|record| {
        record.seq > through && !excluded.contains(&record.seq) && session_public(kernel, record)
    }) {
        background.entries.push(BackgroundEntry::new(
            format!("Session record {}", record.seq),
            serde_json::to_string(&record.body).expect("record is serializable"),
        ));
    }
    Arc::new(background)
}

pub(crate) fn compact_output_bytes(kernel: &Kernel) -> usize {
    (kernel.limits.context_bytes / 4).min(kernel.limits.item_bytes)
}

pub(crate) fn prepare_session_compact(kernel: &Kernel) -> Result<CompactInput, ContextError> {
    let previous = session_checkpoint(kernel);
    let after = previous
        .as_ref()
        .map_or(Seq::ZERO, |checkpoint| checkpoint.through);
    let mut input = CompactInput {
        scope: CompactScope::Session,
        previous,
        output_bytes: compact_output_bytes(kernel),
        through: after,
        records: Vec::new(),
    };
    let latest_public = kernel
        .records
        .values()
        .rev()
        .find(|record| session_public(kernel, record))
        .map(|record| record.seq);
    for record in kernel.records.values().filter(|record| {
        record.seq > after && Some(record.seq) != latest_public && session_public(kernel, record)
    }) {
        let candidate = view(record, 0, usize::MAX)?;
        input.records.push(candidate);
        let old = input.through;
        input.through = record.seq;
        if encoded_len(&input) > kernel.limits.context_bytes {
            input.records.pop();
            input.through = old;
            break;
        }
    }
    if input.through == after {
        return Err(ContextError::NothingToCompact);
    }
    Ok(input)
}

fn pageable(kernel: &Kernel, seq: Seq) -> Result<bool, ContextError> {
    let record = kernel
        .records
        .get(&seq)
        .ok_or(ContextError::MissingRecord(seq))?;
    Ok(match &record.body {
        RecordBody::ToolFinished { .. }
        | RecordBody::ReadResult {
            record: Some(_), ..
        } => true,
        RecordBody::Delivery { source, .. } => kernel
            .records
            .get(source)
            .is_some_and(|record| matches!(record.body, RecordBody::ToolFinished { .. })),
        _ => false,
    })
}

pub(crate) fn card(kernel: &Kernel, id: JobId) -> JobCard {
    let job = &kernel.jobs[&id];
    JobCard {
        id,
        spec: job.spec.clone(),
        status: kernel.job_status(id),
        report: job.report.and_then(|seq| match &kernel.records[&seq].body {
            RecordBody::Report { report, .. } => Some(report.clone()),
            _ => None,
        }),
        latest_handoff: latest_handoff(kernel, id),
    }
}

pub(crate) fn latest_handoff(kernel: &Kernel, job: JobId) -> Option<Seq> {
    kernel.records.values().rev().find_map(|record| {
        matches!(record.body, RecordBody::RoutingHandoff { job: owner, .. } if owner == job)
            .then_some(record.seq)
    })
}

fn expand_records(
    kernel: &Kernel,
    ids: &[Seq],
    page_bytes: usize,
) -> Result<Vec<RecordView>, ContextError> {
    let mut positions = BTreeMap::new();
    let mut views = Vec::new();
    expand_into(kernel, ids, page_bytes, &mut positions, &mut views)?;
    Ok(views)
}

fn expand_into(
    kernel: &Kernel,
    ids: &[Seq],
    page_bytes: usize,
    positions: &mut BTreeMap<(Seq, usize), usize>,
    views: &mut Vec<RecordView>,
) -> Result<(), ContextError> {
    for id in ids {
        let record = kernel
            .records
            .get(id)
            .ok_or(ContextError::MissingRecord(*id))?;
        // A tool result is an immutable authority that may be larger than one
        // model context. A worker consumes this bounded notification page; a
        // non-null cursor means the body remains available through explicit
        // ReadQuery::Record calls. The job's Seq read watermark tracks the
        // notification, not full consumption of every referenced byte.
        let limit = if matches!(record.body, RecordBody::ToolFinished { .. }) {
            page_bytes
        } else {
            usize::MAX
        };
        push_record_view(record, 0, limit, positions, views)?;
        match &record.body {
            RecordBody::Delivery { source, .. } => {
                let source_record = kernel
                    .records
                    .get(source)
                    .ok_or(ContextError::MissingRecord(*source))?;
                let limit = if matches!(source_record.body, RecordBody::ToolFinished { .. }) {
                    page_bytes
                } else {
                    usize::MAX
                };
                push_record_view(source_record, 0, limit, positions, views)?;
            }
            RecordBody::ReadResult {
                record: Some(range),
                ..
            } => {
                push_view(
                    kernel,
                    range.source,
                    range.offset,
                    page_bytes,
                    positions,
                    views,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn push_view(
    kernel: &Kernel,
    id: Seq,
    offset: usize,
    limit: usize,
    positions: &mut BTreeMap<(Seq, usize), usize>,
    views: &mut Vec<RecordView>,
) -> Result<(), ContextError> {
    let record = kernel
        .records
        .get(&id)
        .ok_or(ContextError::MissingRecord(id))?;
    push_record_view(record, offset, limit, positions, views)
}

fn push_record_view(
    record: &Record,
    offset: usize,
    limit: usize,
    positions: &mut BTreeMap<(Seq, usize), usize>,
    views: &mut Vec<RecordView>,
) -> Result<(), ContextError> {
    let next = view(record, offset, limit)?;
    if let Some(position) = positions.get(&(record.seq, offset)).copied() {
        if views[position].next_offset.is_some() && next.next_offset.is_none() {
            views[position] = next;
        }
        return Ok(());
    }
    positions.insert((record.seq, offset), views.len());
    views.push(next);
    Ok(())
}

pub(crate) fn view(
    record: &Record,
    offset: usize,
    limit: usize,
) -> Result<RecordView, ContextError> {
    let content = serde_json::to_string(&record.body).map_err(|_| ContextError::TooLarge)?;
    if offset > content.len() || !content.is_char_boundary(offset) {
        return Err(ContextError::InvalidOffset);
    }
    let mut end = offset.saturating_add(limit).min(content.len());
    while end > offset && !content.is_char_boundary(end) {
        end -= 1;
    }
    if end == offset && offset < content.len() {
        end += content[offset..]
            .chars()
            .next()
            .expect("a non-empty string has a first character")
            .len_utf8();
    }
    Ok(RecordView {
        source: record.seq,
        origin: record.origin.clone(),
        offset,
        next_offset: (end < content.len()).then_some(end),
        content: content[offset..end].to_owned(),
    })
}

fn encoded_len(value: &impl Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn initial_page_bytes(kernel: &Kernel) -> usize {
    kernel
        .limits
        .item_bytes
        .min((kernel.limits.context_bytes / 4).max(1))
}
