use std::{collections::BTreeSet, sync::Arc};

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
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCard {
    pub id: JobId,
    pub spec: JobSpec,
    pub status: JobStatus,
    pub report: Option<ReportDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InquiryView {
    pub id: Seq,
    pub requester: DeliveryTarget,
    pub question: String,
    pub deadline: MonoTimeView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoordinateInput {
    pub routing: Seq,
    pub inputs: Vec<Input>,
    pub source: Option<JobId>,
    pub request: Option<String>,
    pub constraints: String,
    pub jobs: Vec<JobCard>,
    /// Pass this value as `ReadQuery::Jobs.after` to continue the root directory.
    pub next_job: Option<JobId>,
    pub records: Vec<RecordView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkInput {
    pub job: JobId,
    pub revision: u64,
    pub spec: JobSpec,
    pub constraints: String,
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
    pub job: JobId,
    pub revision: u64,
    pub previous: Option<Arc<Checkpoint>>,
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
    let records = expand_records(kernel, &local)?;
    let seen_through = local.last().copied().unwrap_or(job.context.read_through);
    let input = WorkInput {
        job: job_id,
        revision: job.revision,
        spec: job.spec.clone(),
        constraints: kernel.constraints.clone(),
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
        tools: kernel.tools.values().cloned().collect(),
    };
    if encoded_len(&input) <= kernel.limits.context_bytes {
        return Ok(PreparedWork::Work {
            input: Box::new(input),
            seen_through,
        });
    }
    prepare_compact(kernel, job_id).map(PreparedWork::Compact)
}

pub(crate) fn prepare_coordinate(
    kernel: &Kernel,
    routing_id: Seq,
) -> Result<CoordinateInput, ContextError> {
    let routing = kernel
        .routings
        .get(&routing_id)
        .ok_or(ContextError::MissingRouting(routing_id))?;
    let records = expand_records(kernel, &routing.records)?;
    let mut input = CoordinateInput {
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
        jobs: Vec::new(),
        next_job: None,
        records,
    };
    if encoded_len(&input) > kernel.limits.context_bytes {
        return Err(ContextError::TooLarge);
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
    for id in roots.iter().take(DIRECTORY_PAGE) {
        input.jobs.push(card(kernel, *id));
        input.next_job = Some(*id);
        if encoded_len(&input) > kernel.limits.context_bytes {
            input.jobs.pop();
            break;
        }
    }
    if input.jobs.len() == roots.len() {
        input.next_job = None;
    } else if input.jobs.is_empty() {
        return Err(ContextError::TooLarge);
    } else {
        input.next_job = input.jobs.last().map(|job| job.id);
    }
    Ok(input)
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
    let mut through = after;
    for seq in candidates {
        let mut next = expand_records(kernel, &[seq])?;
        let next_through = seq;
        let mut trial = records.clone();
        trial.append(&mut next);
        let input = CompactInput {
            job: job_id,
            revision: job.revision,
            previous: job.context.checkpoint.clone(),
            through: next_through,
            records: trial.clone(),
        };
        if encoded_len(&input) > kernel.limits.context_bytes {
            break;
        }
        records = trial;
        through = next_through;
    }
    if through == after {
        return Err(ContextError::NothingToCompact);
    }
    Ok(CompactInput {
        job: job_id,
        revision: job.revision,
        previous: job.context.checkpoint.clone(),
        through,
        records,
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
    }
}

fn expand_records(kernel: &Kernel, ids: &[Seq]) -> Result<Vec<RecordView>, ContextError> {
    let mut seen = BTreeSet::new();
    let mut views = Vec::new();
    for id in ids {
        push_view(kernel, *id, 0, &mut seen, &mut views)?;
        let record = kernel
            .records
            .get(id)
            .ok_or(ContextError::MissingRecord(*id))?;
        match &record.body {
            RecordBody::Delivery { source, .. } => {
                push_view(kernel, *source, 0, &mut seen, &mut views)?;
            }
            RecordBody::ReadResult {
                record: Some(range),
                ..
            } => {
                push_view(kernel, range.source, range.offset, &mut seen, &mut views)?;
            }
            _ => {}
        }
    }
    Ok(views)
}

fn push_view(
    kernel: &Kernel,
    id: Seq,
    offset: usize,
    seen: &mut BTreeSet<(Seq, usize)>,
    views: &mut Vec<RecordView>,
) -> Result<(), ContextError> {
    if !seen.insert((id, offset)) {
        return Ok(());
    }
    let record = kernel
        .records
        .get(&id)
        .ok_or(ContextError::MissingRecord(id))?;
    views.push(view(record, offset, kernel.limits.item_bytes)?);
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
