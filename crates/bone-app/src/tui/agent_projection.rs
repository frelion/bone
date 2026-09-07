use std::collections::{BTreeMap, BTreeSet};

use bone_agent::{
    ExternalEffect, JobErrorKind, JobId, JobOutput, JobProgress, JobRequest, Notice, RecordEntry,
    RecordKind, Snapshot, ToolCall,
};
use ratatui::widgets::{Paragraph, Wrap};

/// Whether an Agent record represents the terminal completion notice that
/// should draw attention even when progress rows are hidden.
pub(super) fn is_finished_notice(entry: &RecordEntry) -> bool {
    matches!(entry.kind, RecordKind::Notice(Notice::Finished { .. }))
}

/// Pure projection of Agent runtime records into terminal-facing timeline and
/// activity state. It deliberately owns no composer, cursor, or session
/// lifecycle state; those remain in `app`.
#[derive(Debug, PartialEq)]
pub(super) struct Projection {
    pub(super) timeline: Vec<TimelineItem>,
    pub(super) active: BTreeMap<JobId, ActiveJob>,
    pub(super) status: SessionStatus,
    show_progress: bool,
    tool_calls: BTreeMap<JobId, ToolCall>,
    unknown_tools: BTreeSet<JobId>,
}

impl Projection {
    pub(super) fn empty(show_progress: bool) -> Self {
        Self {
            timeline: Vec::new(),
            active: BTreeMap::new(),
            status: SessionStatus::Ready,
            show_progress,
            tool_calls: BTreeMap::new(),
            unknown_tools: BTreeSet::new(),
        }
    }

    pub(super) fn from_snapshot(snapshot: &Snapshot, show_progress: bool) -> Self {
        let mut projection = Self::empty(show_progress);
        projection.apply_all(&snapshot.record);
        if projection.active.is_empty()
            && projection.status == SessionStatus::Ready
            && snapshot.autonomous
        {
            projection.status = SessionStatus::Waiting;
        }
        projection
    }

    pub(super) fn apply_all(&mut self, records: &[RecordEntry]) {
        for entry in records {
            self.apply(entry);
        }
    }

    pub(super) fn apply(&mut self, entry: &RecordEntry) {
        match &entry.kind {
            RecordKind::UserMessage(message) => {
                self.status = SessionStatus::Working;
                self.timeline.push(TimelineItem::message(
                    entry.cursor,
                    Speaker::User,
                    message.text.clone(),
                    false,
                ));
            }
            RecordKind::CancellationRequested { job } => {
                if let Some(active) = self.active.get_mut(job) {
                    active.stopping = true;
                }
            }
            RecordKind::Notice(notice) => self.apply_notice(entry.cursor, notice),
            _ => {}
        }
    }

    fn apply_notice(&mut self, cursor: u64, notice: &Notice) {
        match notice {
            Notice::Reply { text, .. } => self.timeline.push(TimelineItem::message(
                cursor,
                Speaker::Bone,
                text.clone(),
                true,
            )),
            Notice::JobStarted { id, request } => {
                let kind = match request {
                    JobRequest::Work { .. } => ActiveKind::Work,
                    JobRequest::ReviewInput { .. } => ActiveKind::Review,
                    JobRequest::Tool(call) => {
                        let active = active_tool(call);
                        self.tool_calls.insert(*id, call.clone());
                        ActiveKind::Tool(active)
                    }
                };
                self.active.insert(*id, ActiveJob::new(kind));
                self.status = SessionStatus::Working;
            }
            Notice::JobProgress { id, progress } => {
                if let Some(active) = self.active.get_mut(id) {
                    active.progress = Some(progress.clone());
                }
            }
            Notice::JobFinished { id, outcome } => {
                self.active.remove(id);
                if self.active.is_empty() && self.status == SessionStatus::Working {
                    self.status = SessionStatus::Waiting;
                }
                let Some(call) = self.tool_calls.get(id).cloned() else {
                    return;
                };
                let resolved = self.unknown_tools.remove(id);
                if outcome.external_effect == ExternalEffect::Unknown {
                    self.unknown_tools.insert(*id);
                } else {
                    self.tool_calls.remove(id);
                }
                if !resolved
                    && outcome.external_effect == ExternalEffect::None
                    && matches!(
                        &outcome.result,
                        Err(error) if error.kind == JobErrorKind::Cancelled
                    )
                {
                    return;
                }
                let (text, tone, attention) = tool_result(&call, outcome, resolved);
                if self.show_progress || attention {
                    self.timeline.push(TimelineItem {
                        cursor,
                        kind: TimelineKind::Tool { text, tone },
                        attention,
                    });
                }
            }
            Notice::Error { message } => {
                self.timeline.push(TimelineItem {
                    cursor,
                    kind: TimelineKind::Error(message.clone()),
                    attention: true,
                });
            }
            Notice::Paused => {
                self.status = SessionStatus::Waiting;
                self.timeline.push(TimelineItem::status(
                    cursor,
                    "— waiting for you",
                    Tone::Warning,
                    true,
                ));
            }
            Notice::Stopped => {
                self.status = SessionStatus::Stopped;
                self.timeline.push(TimelineItem::status(
                    cursor,
                    "— work stopped",
                    Tone::Warning,
                    true,
                ));
            }
            Notice::Finished { .. } => {
                self.status = SessionStatus::Complete;
            }
        }
    }

    pub(super) fn activity(&self) -> Option<String> {
        if !self.show_progress {
            return None;
        }
        let stopping = self.active.values().filter(|job| job.stopping).count();
        if stopping > 0 {
            return Some(format!(
                "stopping · {} {} resolving",
                self.active.len(),
                if self.active.len() == 1 {
                    "job"
                } else {
                    "jobs"
                }
            ));
        }

        if self.active.is_empty() {
            return None;
        }
        if self.active.len() == 1 {
            return self.active.values().next().map(ActiveJob::detail);
        }

        let mut parts = self
            .active
            .values()
            .take(2)
            .map(ActiveJob::summary)
            .collect::<Vec<_>>();
        let more = self.active.len().saturating_sub(parts.len());
        if more > 0 {
            parts.push(format!("+{more}"));
        }
        Some(format!(
            "{} active · {}",
            self.active.len(),
            parts.join(" · ")
        ))
    }

    pub(super) fn has_unknown_effect(&self) -> bool {
        !self.unknown_tools.is_empty()
    }

    pub(super) fn show_progress(&self) -> bool {
        self.show_progress
    }

    pub(super) fn title(&self) -> Option<&str> {
        self.timeline.iter().find_map(|item| match &item.kind {
            TimelineKind::Message {
                speaker: Speaker::User,
                text,
            } => text.lines().find(|line| !line.trim().is_empty()),
            _ => None,
        })
    }
}

fn tool_result(
    call: &ToolCall,
    outcome: &bone_agent::JobOutcome,
    resolved: bool,
) -> (String, Tone, bool) {
    if outcome.external_effect == ExternalEffect::Unknown {
        let detail = match &outcome.result {
            Err(error) => format!(": {}", error.message),
            Ok(_) => String::new(),
        };
        return (
            format!("! {} · outcome unknown{detail}", tool_subject(call)),
            Tone::Warning,
            true,
        );
    }

    let resolution = resolved.then_some("outcome resolved");
    match &outcome.result {
        Ok(_) => (
            match resolution {
                Some(detail) => format!("✓ {} · {detail}", completed_tool(call, outcome)),
                None => format!("✓ {}", completed_tool(call, outcome)),
            },
            Tone::Success,
            resolved,
        ),
        Err(error) => {
            let detail = match error.kind {
                JobErrorKind::Cancelled => "cancelled",
                JobErrorKind::TimedOut => "timed out",
                JobErrorKind::Failed | JobErrorKind::Panicked => error.message.as_str(),
            };
            let detail = match resolution {
                Some(resolution) => format!("{resolution}: {detail}"),
                None => detail.to_owned(),
            };
            let symbol = if error.kind == JobErrorKind::Cancelled {
                "—"
            } else {
                "×"
            };
            (
                format!("{symbol} {} · {detail}", tool_subject(call)),
                if error.kind == JobErrorKind::Cancelled {
                    Tone::Warning
                } else {
                    Tone::Error
                },
                resolved || error.kind != JobErrorKind::Cancelled,
            )
        }
    }
}

fn tool_subject(call: &ToolCall) -> String {
    match call.name.as_str() {
        "read" => display_argument(call, "path").unwrap_or_else(|| "file".to_owned()),
        "grep" => match (
            display_argument(call, "pattern"),
            display_argument(call, "path"),
        ) {
            (Some(pattern), Some(path)) => format!("search {pattern:?} in {path}"),
            (Some(pattern), None) => format!("search {pattern:?}"),
            _ => "grep".to_owned(),
        },
        "glob" => match (
            display_argument(call, "pattern"),
            display_argument(call, "path"),
        ) {
            (Some(pattern), Some(path)) => format!("find {pattern} in {path}"),
            (Some(pattern), None) => format!("find {pattern}"),
            _ => "find files".to_owned(),
        },
        name => humanized_tool_name(name),
    }
}

fn active_tool(call: &ToolCall) -> String {
    let subject = tool_subject(call);
    match call.name.as_str() {
        "read" => format!("Reading {subject}"),
        "grep" => format!(
            "Searching {}",
            subject.strip_prefix("search ").unwrap_or(&subject)
        ),
        "glob" => format!(
            "Finding {}",
            subject.strip_prefix("find ").unwrap_or(&subject)
        ),
        _ => subject,
    }
}

fn completed_tool(call: &ToolCall, outcome: &bone_agent::JobOutcome) -> String {
    let artifact = match &outcome.result {
        Ok(JobOutput::Artifact(value)) => Some(value),
        _ => None,
    };
    match call.name.as_str() {
        "read" => {
            let path = display_argument(call, "path").unwrap_or_else(|| "file".to_owned());
            match artifact.and_then(|value| {
                Some((
                    value.get("start_line")?.as_u64()?,
                    value.get("end_line")?.as_u64()?,
                ))
            }) {
                Some((start, end)) => format!("Read {path} · lines {start}–{end}"),
                None => format!("Read {path}"),
            }
        }
        "grep" => {
            let subject = tool_subject(call);
            match artifact
                .and_then(|value| value.get("match_count"))
                .and_then(|value| value.as_u64())
            {
                Some(count) => format!(
                    "Searched {} · {count} matches",
                    subject.trim_start_matches("search ")
                ),
                None => format!("Searched {}", subject.trim_start_matches("search ")),
            }
        }
        "glob" => {
            let subject = tool_subject(call);
            match artifact
                .and_then(|value| value.get("paths"))
                .and_then(|value| value.as_array())
            {
                Some(paths) => format!(
                    "Found {} · {} paths",
                    subject.trim_start_matches("find "),
                    paths.len()
                ),
                None => format!("Found {}", subject.trim_start_matches("find ")),
            }
        }
        _ => tool_subject(call),
    }
}

fn display_argument(call: &ToolCall, key: &str) -> Option<String> {
    let value = call.arguments.get(key)?.as_str()?;
    let value = normalize_and_shorten(value);
    (!value.is_empty()).then_some(value)
}

fn humanized_tool_name(name: &str) -> String {
    let name = name.replace(['_', '-'], " ");
    let name = normalize_and_shorten(&name);
    if name.is_empty() {
        "tool".to_owned()
    } else {
        name
    }
}

fn normalize_and_shorten(text: &str) -> String {
    const LIMIT: usize = 48;
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let short = chars.by_ref().take(LIMIT).collect::<String>();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TimelineItem {
    pub(super) cursor: u64,
    pub(super) kind: TimelineKind,
    pub(super) attention: bool,
}

impl TimelineItem {
    pub(super) fn message(cursor: u64, speaker: Speaker, text: String, attention: bool) -> Self {
        Self {
            cursor,
            kind: TimelineKind::Message { speaker, text },
            attention,
        }
    }

    pub(super) fn status(
        cursor: u64,
        text: impl Into<String>,
        tone: Tone,
        attention: bool,
    ) -> Self {
        Self {
            cursor,
            kind: TimelineKind::Status {
                text: text.into(),
                tone,
            },
            attention,
        }
    }

    pub(super) fn line_count(&self, width: u16) -> usize {
        Paragraph::new(self.text())
            .wrap(Wrap { trim: false })
            .line_count(width.saturating_sub(2).max(1))
            .max(1)
    }

    fn text(&self) -> &str {
        match &self.kind {
            TimelineKind::Message { text, .. }
            | TimelineKind::Tool { text, .. }
            | TimelineKind::Error(text)
            | TimelineKind::Status { text, .. } => text,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TimelineKind {
    Message { speaker: Speaker, text: String },
    Tool { text: String, tone: Tone },
    Error(String),
    Status { text: String, tone: Tone },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Speaker {
    User,
    Bone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Success,
    Warning,
    Error,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ActiveJob {
    kind: ActiveKind,
    progress: Option<JobProgress>,
    stopping: bool,
}

impl ActiveJob {
    fn new(kind: ActiveKind) -> Self {
        Self {
            kind,
            progress: None,
            stopping: false,
        }
    }

    fn summary(&self) -> String {
        let label = match &self.kind {
            ActiveKind::Work => "Thinking".to_owned(),
            ActiveKind::Review => "Reading your update".to_owned(),
            ActiveKind::Tool(label) => label.clone(),
        };
        match self.progress.as_ref().and_then(|progress| progress.percent) {
            Some(percent) => format!("{label} {percent}%"),
            None => label,
        }
    }

    fn detail(&self) -> String {
        let summary = self.summary();
        let Some(progress) = &self.progress else {
            return summary;
        };
        let message = progress
            .message
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if message.is_empty() {
            summary
        } else {
            format!("{summary} · {message}")
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ActiveKind {
    Work,
    Review,
    Tool(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionStatus {
    Ready,
    Working,
    Waiting,
    Stopped,
    Complete,
}

#[cfg(test)]
mod tests {
    use bone_agent::ToolCall;

    use super::active_tool;

    #[test]
    fn tool_labels_normalize_and_bound_displayed_arguments() {
        let grep = ToolCall::new(
            "grep",
            serde_json::json!({
                "pattern": "  agent\n   loop  ",
                "path": "  crates/ bone-agent  "
            }),
        );
        assert_eq!(
            active_tool(&grep),
            "Searching \"agent loop\" in crates/ bone-agent"
        );

        let read = ToolCall::new(
            "read",
            serde_json::json!({"path": format!("  src/{}  ", "x".repeat(80))}),
        );
        let label = active_tool(&read);
        assert!(!label.contains("  "));
        assert!(label.ends_with('…'));
        assert_eq!(label.chars().count(), "Reading ".chars().count() + 49);

        let extension = ToolCall::new("future_tool-name", serde_json::json!({}));
        assert_eq!(active_tool(&extension), "future tool name");
    }
}
