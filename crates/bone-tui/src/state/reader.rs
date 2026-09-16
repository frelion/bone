//! Pure read-only projections of public App objects for a session reader.
//! No rendering, I/O, or inference from tool output text belongs here.
use bone_app::{
    HistoryEntry, JobOwner, JobRef, JobState, SessionEvent, SessionId, SessionSeq, SessionView,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderSource {
    History(SessionSeq),
    Job(JobRef),
}

/// The session is part of the identity: a sequence alone is not globally unique.
#[derive(Clone, Debug)]
pub(crate) struct ReaderContent {
    pub session: SessionId,
    pub source: ReaderSource,
    pub title: String,
    pub text: std::sync::Arc<str>,
    pub layout_cache: std::cell::RefCell<Option<ReaderLayout>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ReaderLayout {
    width: usize,
    text: std::sync::Arc<str>,
    rows: std::sync::Arc<ReaderRows>,
}

/// Original source plus packed row starts; only viewport text is materialized.
#[derive(Debug)]
pub(crate) struct ReaderRows {
    text: std::sync::Arc<str>,
    starts: Box<[u32]>,
}
impl ReaderRows {
    pub fn len(&self) -> usize {
        self.starts.len()
    }
    pub fn range(&self, row: usize) -> std::ops::Range<usize> {
        crate::ui::selection::row_range(&self.text, &self.starts, row)
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = String> + '_ {
        (0..self.len()).map(|row| crate::ui::selection::display_text(&self.text[self.range(row)]))
    }
    pub fn allocated_bytes(&self) -> usize {
        std::mem::size_of_val(&*self.starts)
    }
}

impl PartialEq for ReaderContent {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.source == other.source
            && self.title == other.title
            && self.text == other.text
    }
}
impl Eq for ReaderContent {}

impl ReaderContent {
    /// Exactly one object/width layout. Only viewport rows are consumed per frame.
    pub fn wrapped_rows(&self, width: usize) -> std::sync::Arc<ReaderRows> {
        let mut cache = self.layout_cache.borrow_mut();
        if let Some(layout) = cache.as_ref()
            && layout.width == width
            && std::sync::Arc::ptr_eq(&layout.text, &self.text)
        {
            return std::sync::Arc::clone(&layout.rows);
        }
        let starts = crate::ui::selection::source_starts(&self.text, width).into_boxed_slice();
        let rows = std::sync::Arc::new(ReaderRows {
            text: self.text.clone(),
            starts,
        });
        // Charge the shared source once and the packed source offsets.
        let bytes = rows
            .allocated_bytes()
            .saturating_add(self.text.len())
            .saturating_add(self.title.capacity())
            .saturating_add(
                std::mem::size_of::<ReaderContent>()
                    + std::mem::size_of::<ReaderLayout>()
                    + std::mem::size_of::<ReaderRows>()
                    + 4 * std::mem::size_of::<usize>(),
            );
        *cache = (bytes <= 8 * 1024 * 1024).then(|| ReaderLayout {
            width,
            text: std::sync::Arc::clone(&self.text),
            rows: std::sync::Arc::clone(&rows),
        });
        rows
    }

    /// Live Job details follow only the original session and full Job identity.
    /// Historical event projections deliberately remain immutable.
    pub fn refresh_job(&mut self, view: &SessionView) {
        let ReaderSource::Job(source) = self.source else {
            return;
        };
        if self.session != view.session.id {
            return;
        }
        if let Some(current) = Self::from_job(view, source) {
            if *self != current {
                *self = current;
            }
        } else {
            self.layout_cache.get_mut().take();
            self.text = format!(
                "This job is no longer in the current snapshot.\n\nJob: {} / {}",
                source.runtime, source.id,
            )
            .into();
        }
    }

    /// Only public, explicitly addressed objects are expandable. Ordinary replies
    /// do not become synthetic Jobs and a missing Job is not replaced by another.
    pub fn from_history(session: SessionId, entry: &HistoryEntry) -> Option<Self> {
        let (title, text) = match &entry.event {
            SessionEvent::RoutingFailed {
                runtime, message, ..
            } => (
                "Routing failed".into(),
                format!(
                    "{message}\n\nRuntime: {runtime}\nHistory: {}",
                    entry.sequence.0
                ),
            ),
            SessionEvent::InputRejected { input, message } => (
                "Input rejected".into(),
                format!(
                    "{message}\n\nInput: {}\nHistory: {}",
                    input.0, entry.sequence.0
                ),
            ),
            SessionEvent::ToolFinished {
                call,
                job,
                tool,
                outcome,
            } => {
                // Both ToolOutcome fields are preserved. A plain string result is
                // shown verbatim; structured values retain all their JSON fields.
                let result = match &outcome.result {
                    Ok(value) => match value.as_str() {
                        Some(text) => format!("Result: success\n\n{text}"),
                        // Preserve every field without allowing indentation to
                        // multiply a bounded App result into an oversized reader.
                        None => format!("Result: success\n\n{value}"),
                    },
                    Err(error) => {
                        format!("Result: error\nKind: {:?}\n\n{}", error.kind, error.message)
                    }
                };
                (
                    tool.clone(),
                    format!(
                        "Tool: {tool}\nExternal effect: {:?}\n\n{result}\n\nCall: {} / {}\nJob: {} / {}\nHistory: {}",
                        outcome.external_effect,
                        call.runtime,
                        call.id,
                        job.runtime,
                        job.id,
                        entry.sequence.0,
                    ),
                )
            }
            SessionEvent::JobFinished {
                job,
                outcome,
                summary,
                remaining,
            } => {
                let remaining = remaining
                    .iter()
                    .map(|item| format!("- {item}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                (
                    format!("Job {} result", job.id),
                    format!(
                        "Outcome: {outcome:?}\n\nSummary\n{summary}\n\nRemaining\n{remaining}\n\nJob: {} / {}\nHistory: {}",
                        job.runtime, job.id, entry.sequence.0,
                    ),
                )
            }
            _ => return None,
        };
        Some(Self {
            session,
            source: ReaderSource::History(entry.sequence),
            title,
            text: text.into(),
            layout_cache: Default::default(),
        })
    }

    pub fn from_job(view: &SessionView, source: JobRef) -> Option<Self> {
        let job = view.jobs.iter().find(|job| job.id == source)?;
        let state = match &job.state {
            JobState::Ready => "Ready".to_owned(),
            JobState::Running => "Running".to_owned(),
            JobState::Waiting(reason) => format!("Waiting: {reason:?}"),
            JobState::Paused => "Paused".to_owned(),
            JobState::Finished { outcome, summary } => format!("Finished: {outcome:?}\n{summary}"),
        };
        let owner = match job.owner {
            JobOwner::User => "User".to_owned(),
            JobOwner::Routing => "Routing".to_owned(),
            JobOwner::Job(owner) => format!("Job {} / {}", owner.runtime, owner.id),
        };
        let mut text = format!(
            "Goal\n{}\n\nState\n{state}\n\nDone when\n{}\n\nScope\n{}",
            job.goal, job.done_when, job.scope,
        );
        if let Some(report) = &job.report {
            text.push_str(&format!("\n\nReport\n{}", report.summary));
        }
        text.push_str(&format!(
            "\n\nOwner: {owner}\nJob: {} / {}",
            job.id.runtime, job.id.id,
        ));
        Some(Self {
            session: view.session.id,
            source: ReaderSource::Job(source),
            title: if job.goal.trim().is_empty() {
                format!("Job {}", source.id)
            } else {
                job.goal.clone()
            },
            text: text.into(),
            layout_cache: Default::default(),
        })
    }

    /// Input associations belong to the current public snapshot. Never duplicate
    /// an accumulating Vec in the reader projection or substitute another job.
    pub fn job_inputs<'a>(&self, view: Option<&'a SessionView>) -> Option<&'a [bone_app::InputId]> {
        let view = view.filter(|view| view.session.id == self.session)?;
        let ReaderSource::Job(source) = self.source else {
            return None;
        };
        view.jobs
            .iter()
            .find(|job| job.id == source)
            .map(|job| job.inputs.as_slice())
    }
}

#[cfg(test)]
mod projection_budget_tests {
    use super::*;

    #[test]
    fn bounded_nested_json_stays_compact_lossless_and_inert() {
        let mut items = vec![serde_json::json!("汉字"); 60_000];
        items.push(serde_json::json!("\u{1b}[31mUnicode🙂"));
        let mut value = serde_json::Value::Array(items);
        for _ in 0..72 {
            value = serde_json::json!({ "nested": value });
        }
        let outcome = bone_app::ToolOutcome {
            result: Ok(value.clone()),
            external_effect: bone_app::ExternalEffect::None,
        };
        let encoded = serde_json::to_vec(&outcome).unwrap();
        assert!(encoded.len() <= 1024 * 1024, "legal App tool output size");
        assert!(
            serde_json::to_string_pretty(&value).unwrap().len() > 8 * 1024 * 1024,
            "would exceed reader budget when pretty printed"
        );
        let runtime = bone_app::RuntimeId::new();
        let entry = HistoryEntry {
            sequence: SessionSeq(1),
            occurred_at: 0,
            event: SessionEvent::ToolFinished {
                call: bone_app::CallRef { runtime, id: 1 },
                job: JobRef { runtime, id: 2 },
                tool: "session_history".into(),
                outcome,
            },
        };
        let content = ReaderContent::from_history(SessionId::new(), &entry).unwrap();
        assert!(content.text.len() < encoded.len() + 512);
        let json = content
            .text
            .split_once("Result: success\n\n")
            .unwrap()
            .1
            .split_once("\n\nCall:")
            .unwrap()
            .0;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(json).unwrap(),
            value
        );
        assert!(content.text.contains("汉字"));
        assert!(!content.text.contains('\u{1b}'));
        assert!(content.text.contains("\\u001b[31m"));
    }
}
