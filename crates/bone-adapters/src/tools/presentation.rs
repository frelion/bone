//! Pure, on-demand views of recorded tool calls. No execution environment is required.
use bone_core::{ExternalEffect, ToolOutcome};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSummary {
    pub subject: String,
    pub result: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDetails {
    pub sections: Vec<TextSection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSection {
    pub heading: Option<String>,
    pub text: String,
    pub first_line: Option<usize>,
}

pub fn summary(name: &str, args: &Value, outcome: Option<&ToolOutcome>) -> ToolSummary {
    let output = outcome.and_then(|outcome| outcome.result.as_ref().ok());
    let mut summary = match name {
        "read" => super::read::summary(args, output),
        "glob" => super::glob::summary(args, output),
        "grep" => super::grep::summary(args, output),
        "bash" => super::bash::summary(args, output),
        "apply_patch" => super::patch::summary(args, output),
        _ => None,
    }
    .unwrap_or_else(|| ToolSummary {
        subject: name.to_owned(),
        result: output.map(|_| "Result available".to_owned()),
    });
    if let Some(Err(error)) = outcome.map(|outcome| &outcome.result) {
        summary.result = Some(short(&error.message));
    }
    summary
}

pub fn details(name: &str, args: &Value, outcome: &ToolOutcome) -> ToolDetails {
    let mut sections = match &outcome.result {
        Err(error) => vec![
            section("Arguments", raw_text(args)),
            section("Error", &error.message),
        ],
        Ok(output) => {
            let known = match name {
                "read" => super::read::details(args, output),
                "glob" => super::glob::details(args, output),
                "grep" => super::grep::details(args, output),
                "bash" => super::bash::details(args, output),
                "apply_patch" => super::patch::details(args, output),
                _ => None,
            };
            known.unwrap_or_else(|| {
                vec![
                    section("Arguments", raw_text(args)),
                    section("Result", raw_text(output)),
                ]
            })
        }
    };
    match outcome.external_effect {
        ExternalEffect::None => {}
        ExternalEffect::Applied => {
            sections.push(section("External effect", "Changes were applied."))
        }
        ExternalEffect::Unknown => sections.push(section(
            "Warning",
            "External effects are unknown. Check the affected files or process before retrying.",
        )),
    }
    ToolDetails { sections }
}

pub(super) fn section(heading: impl Into<String>, text: impl Into<String>) -> TextSection {
    TextSection {
        heading: Some(heading.into()),
        text: text.into(),
        first_line: None,
    }
}

pub(super) fn short(text: &str) -> String {
    let mut chars = text.chars();
    let mut result: String = chars
        .by_ref()
        .take(160)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

pub(super) fn notices(output: &Value) -> Vec<String> {
    let mut notices = Vec::new();
    if output["truncated"].as_bool() == Some(true) {
        notices.push("Output truncated".to_owned());
    }
    if let Some(warnings) = output["warnings"].as_array() {
        notices.extend(warnings.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    notices
}

pub(super) fn with_notices(mut sections: Vec<TextSection>, output: &Value) -> Vec<TextSection> {
    let notices = notices(output);
    if !notices.is_empty() {
        sections.push(section("Notes", notices.join("\n")));
    }
    sections
}

pub(super) fn result(text: String, output: &Value) -> String {
    let mut text = text;
    if output["truncated"].as_bool() == Some(true) {
        text.push_str(" · truncated");
    }
    if let Some(count) = output["warnings"]
        .as_array()
        .map(Vec::len)
        .filter(|count| *count > 0)
    {
        text.push_str(&format!(" · {count} warnings"));
    }
    text
}

fn raw_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn grep_groups_paths_and_patch_reports_actual_file_changes() {
        let args = json!({"pattern":"needle", "path":"src", "glob":"*.rs"});
        let grep = ToolOutcome::value(
            json!({"match_count":1, "matches":[{"path":"src/a.rs", "line_number":7, "text":"needle", "kind":"match", "line_truncated":true}], "truncated":true, "warnings":[], "binary_files_skipped":2, "oversized_files_skipped":0}),
        );
        assert_eq!(
            summary("grep", &args, Some(&grep)).result.as_deref(),
            Some("1 matches · truncated")
        );
        let detail = details("grep", &args, &grep);
        assert!(
            detail
                .sections
                .iter()
                .any(|section| section.heading.as_deref() == Some("src/a.rs")
                    && section.text == "7: needle … [truncated]")
        );
        assert!(
            detail
                .sections
                .iter()
                .any(|section| section.text == "2 binary files skipped")
        );
        let patch = ToolOutcome::value(
            json!({"summary":"applied", "changes":[{"kind":"update", "path":"a.rs", "moved_to":"b.rs", "added_lines":3,"removed_lines":2}]}),
        );
        assert_eq!(
            summary("apply_patch", &json!({}), Some(&patch))
                .result
                .as_deref(),
            Some("1 files")
        );
        let detail = details("apply_patch", &json!({}), &patch);
        assert_eq!(detail.sections[0].heading.as_deref(), Some("a.rs → b.rs"));
        assert_eq!(detail.sections[0].text, "update · +3 −2 lines");
    }

    #[test]
    fn fallback_preserves_strings_and_reports_unknown_external_effects() {
        let outcome = ToolOutcome {
            result: Ok(json!("raw\ntext")),
            external_effect: ExternalEffect::Unknown,
        };
        let detail = details("extension", &json!({}), &outcome);
        assert_eq!(detail.sections[1].text, "raw\ntext");
        assert!(detail.sections[2].text.contains("unknown"));
        let nested = (0..80).fold(json!(0), |value, _| json!([value]));
        let detail = details("extension", &json!({}), &ToolOutcome::value(nested));
        assert_eq!(detail.sections[1].text.len(), 161);
    }

    #[test]
    fn read_summary_and_details_share_metadata_without_numbering_content() {
        let args = json!({"path":"src/lib.rs", "offset":20});
        let outcome = ToolOutcome::value(
            json!({"path":"src/lib.rs", "start_line":20, "end_line":21, "content":"hello\r\nworld", "truncated":true, "line_truncated":true, "next_offset":22}),
        );
        assert_eq!(
            summary("read", &args, None).result.as_deref(),
            Some("from line 20")
        );
        assert_eq!(
            summary("read", &args, Some(&outcome)).result.as_deref(),
            Some("lines 20–21 · truncated")
        );
        let details = details("read", &args, &outcome);
        assert_eq!(details.sections[0].first_line, Some(20));
        assert_eq!(details.sections[0].text, "hello\r\nworld");
        assert!(
            details
                .sections
                .iter()
                .any(|section| section.text.contains("22"))
        );
        assert!(
            details
                .sections
                .iter()
                .any(|section| section.text.contains("omitted"))
        );
    }

    #[test]
    fn search_exposes_partial_results_and_warnings() {
        let args = json!({"pattern":"*.rs"});
        let outcome = ToolOutcome::value(
            json!({"paths":["a.rs","b.rs"],"truncated":true,"warnings":["Cannot read private/"]}),
        );
        assert_eq!(
            summary("glob", &args, Some(&outcome)).result.as_deref(),
            Some("2 paths · truncated · 1 warnings")
        );
        let detail = details("glob", &args, &outcome);
        assert_eq!(detail.sections[1].text, "a.rs\nb.rs");
        assert!(detail.sections[2].text.contains("Cannot read private/"));
    }

    #[test]
    fn bash_exit_failure_and_timeout_are_not_presented_as_success() {
        let args = json!({"command":"cargo test", "cwd":"src"});
        for (code, timeout, expected) in [
            (json!(1), false, "exit 1"),
            (Value::Null, true, "timed out"),
        ] {
            let outcome = ToolOutcome::value(
                json!({"stdout":"", "stderr":"failed\n", "exit_code":code, "timed_out":timeout, "truncated":false}),
            );
            assert_eq!(
                summary("bash", &args, Some(&outcome)).result.as_deref(),
                Some(expected)
            );
            assert!(
                details("bash", &args, &outcome)
                    .sections
                    .iter()
                    .any(|section| section.text == "failed\n")
            );
        }
    }

    #[test]
    fn unknown_malformed_and_failed_calls_preserve_original_evidence() {
        let args = json!({"pattern":"needle"});
        for name in ["grep", "extension"] {
            let outcome = ToolOutcome::value(json!({"unexpected":"payload"}));
            assert_eq!(
                summary(name, &args, Some(&outcome)).result.as_deref(),
                Some("Result available")
            );
            let detail = details(name, &args, &outcome);
            assert!(detail.sections[0].text.contains("needle"));
            assert!(detail.sections[1].text.contains("payload"));
        }
        let failed = ToolOutcome::failed("permission denied");
        assert_eq!(
            summary("grep", &args, Some(&failed)).result.as_deref(),
            Some("permission denied")
        );
        assert_eq!(
            details("grep", &args, &failed).sections[1].text,
            "permission denied"
        );
    }

    #[test]
    fn large_outputs_do_not_expand_summary_and_commands_stay_one_line() {
        let args = json!({"command":format!("echo\n{}", "x".repeat(100_000))});
        let outcome = ToolOutcome::value(
            json!({"stdout":"x".repeat(1_000_000),"stderr":"","exit_code":0,"timed_out":false,"truncated":false}),
        );
        let summary = summary("bash", &args, Some(&outcome));
        assert_eq!(summary.subject.chars().count(), 161);
        assert!(!summary.subject.contains('\n'));
        assert_eq!(summary.result.as_deref(), Some("exit 0"));
    }
}
