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
    pub numbered: Vec<NumberedText>,
    pub layout_cache: std::cell::RefCell<Option<ReaderLayout>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NumberedText {
    pub range: std::ops::Range<usize>,
    pub first_line: usize,
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
    pub gutter: u16,
    numbers: Vec<(usize, usize)>,
}
impl ReaderRows {
    pub fn len(&self) -> usize {
        self.starts.len()
    }
    pub fn range(&self, row: usize) -> std::ops::Range<usize> {
        crate::ui::selection::row_range(&self.text, &self.starts, row)
    }
    pub fn line_number(&self, row: usize) -> Option<usize> {
        self.numbers
            .binary_search_by_key(&row, |(row, _)| *row)
            .ok()
            .map(|index| self.numbers[index].1)
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = String> + '_ {
        (0..self.len()).map(|row| crate::ui::selection::display_text(&self.text[self.range(row)]))
    }
    pub fn allocated_bytes(&self) -> usize {
        std::mem::size_of_val(&*self.starts) + std::mem::size_of_val(self.numbers.as_slice())
    }
}

impl PartialEq for ReaderContent {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.source == other.source
            && self.title == other.title
            && self.text == other.text
            && self.numbered == other.numbered
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
        let mut numbered_starts = Vec::new();
        for section in &self.numbered {
            let mut offset = section.range.start;
            for (index, line) in self.text[section.range.clone()]
                .split_inclusive('\n')
                .enumerate()
            {
                numbered_starts.push((offset, section.first_line + index));
                offset += line.len();
            }
        }
        let gutter = numbered_starts
            .iter()
            .map(|(_, number)| number.to_string().len() + 1)
            .max()
            .unwrap_or(0);
        let gutter = if gutter < width { gutter } else { 0 };
        let starts = crate::ui::selection::source_starts(&self.text, width.saturating_sub(gutter))
            .into_boxed_slice();
        let numbers = if numbered_starts.is_empty() {
            Vec::new()
        } else {
            starts
                .iter()
                .enumerate()
                .filter_map(|(row, start)| {
                    numbered_starts
                        .binary_search_by_key(&(*start as usize), |(byte, _)| *byte)
                        .ok()
                        .map(|index| (row, numbered_starts[index].1))
                })
                .collect()
        };
        let rows = std::sync::Arc::new(ReaderRows {
            text: self.text.clone(),
            starts,
            gutter: gutter as u16,
            numbers,
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
            SessionEvent::ConversationFailed {
                runtime, message, ..
            } => (
                "Conversation failed".into(),
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
                tool,
                arguments,
                outcome,
                ..
            } => {
                let summary = bone_app::tool_summary(tool, arguments, Some(outcome));
                let details = bone_app::tool_details(tool, arguments, outcome);
                let mut text = String::new();
                let mut numbered = Vec::new();
                for section in details.sections {
                    if !text.is_empty() {
                        if !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push('\n');
                    }
                    if let Some(heading) = section.heading {
                        text.push_str(&heading);
                        text.push('\n');
                    }
                    let start = text.len();
                    text.push_str(&section.text);
                    if let Some(first_line) = section.first_line {
                        numbered.push(NumberedText {
                            range: start..text.len(),
                            first_line,
                        });
                    }
                }
                return Some(Self {
                    session,
                    source: ReaderSource::History(entry.sequence),
                    title: format!("{tool} · {}", summary.subject),
                    text: text.into(),
                    numbered,
                    layout_cache: Default::default(),
                });
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
            numbered: Vec::new(),
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
            JobOwner::Job(owner) => format!("Job {} / {}", owner.runtime, owner.id),
        };
        let mut text = format!(
            "Goal\n{}\n\nState\n{state}\n\nDone when\n{}\n\nScope\n{}",
            job.goal, job.done_when, job.scope,
        );
        let tools = if job.allowed_tools.is_empty() {
            "None".to_owned()
        } else {
            job.allowed_tools
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        text.push_str(&format!("\n\nAllowed tools\n{tools}"));
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
            numbered: Vec::new(),
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
    fn line_numbers_disappear_when_the_complete_gutter_does_not_fit() {
        let content = ReaderContent {
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "source".into(),
            text: "界abc\nnext\n".into(),
            numbered: vec![NumberedText {
                range: 0..12,
                first_line: 10000,
            }],
            layout_cache: Default::default(),
        };
        for width in 1..=6 {
            assert_eq!(content.wrapped_rows(width).gutter, 0);
        }
        let rows = content.wrapped_rows(8);
        assert_eq!(rows.gutter, 6);
        assert_eq!(rows.line_number(0), Some(10000));
        assert_eq!(
            rows.line_number(1),
            None,
            "soft wraps do not repeat line numbers"
        );
        assert_eq!(
            rows.numbers
                .iter()
                .map(|(_, number)| *number)
                .collect::<Vec<_>>(),
            [10000, 10001]
        );
    }

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
                arguments: serde_json::json!({}),
                outcome,
            },
        };
        let content = ReaderContent::from_history(SessionId::new(), &entry).unwrap();
        assert!(content.text.len() < encoded.len() + 512);
        let json = content.text.split_once("Result\n").unwrap().1;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(json).unwrap(),
            value
        );
        assert!(content.text.contains("汉字"));
        assert!(!content.text.contains('\u{1b}'));
        assert!(content.text.contains("\\u001b[31m"));
    }
}
