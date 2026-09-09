use std::time::Duration;

use bone_adapters::tools::{ReadArgs, Tool, ToolEnvironment, ToolLimits, ToolLimitsError};
use serde_json::json;

#[test]
fn defaults_and_integer_second_deadlines_round_trip_through_the_plain_domain() {
    let defaults: ToolLimits = serde_json::from_value(json!({})).unwrap();
    assert_eq!(defaults, ToolLimits::default());

    let limits: ToolLimits = serde_json::from_value(json!({
        "max_read_lines": 2,
        "default_bash_timeout_seconds": 3,
        "max_bash_timeout_seconds": 10
    }))
    .unwrap();
    assert_eq!(
        limits,
        ToolLimits {
            max_read_lines: 2,
            default_bash_timeout: Duration::from_secs(3),
            max_bash_timeout: Duration::from_secs(10),
            ..ToolLimits::default()
        }
    );
    limits.validate().unwrap();

    let stored = serde_json::to_value(&limits).unwrap();
    assert_eq!(stored["default_bash_timeout_seconds"], 3);
    assert_eq!(stored["max_bash_timeout_seconds"], 10);
    assert!(stored.get("default_bash_timeout").is_none());
    assert!(stored.get("max_bash_timeout").is_none());
}

#[test]
fn serde_shape_and_domain_validation_reject_invalid_limits() {
    for invalid in [
        json!({"max_output_bytes": 0}),
        json!({"max_read_lines": 0}),
        json!({"max_search_file_bytes": 0}),
        json!({"max_patch_total_bytes": 0}),
        json!({"default_bash_timeout_seconds": 0}),
        json!({"max_bash_timeout_seconds": 0}),
        json!({"default_bash_timeout_seconds": 10, "max_bash_timeout_seconds": 9}),
    ] {
        let limits: ToolLimits = serde_json::from_value(invalid).unwrap();
        assert!(limits.validate().is_err());
    }
    for malformed in [
        json!({"max_read_lienes": 10}),
        json!({"default_bash_timeout": {"secs": 1, "nanos": 0}}),
        json!({"default_bash_timeout_seconds": {"secs": 1, "nanos": 0}}),
        json!({"default_bash_timeout_seconds": 1.5}),
        json!({"max_bash_timeout_seconds": -1}),
    ] {
        assert!(serde_json::from_value::<ToolLimits>(malformed).is_err());
    }

    assert_eq!(
        ToolLimits {
            max_ignore_total_bytes: 0,
            ..ToolLimits::default()
        }
        .validate(),
        Err(ToolLimitsError::NonPositive {
            field: "max_ignore_total_bytes"
        })
    );
}

#[tokio::test]
async fn environments_capture_validated_limits_by_value() {
    let directory = tempfile::tempdir().unwrap();
    tokio::fs::write(directory.path().join("sample.txt"), "one\ntwo\nthree\n")
        .await
        .unwrap();

    let original_limits = ToolLimits {
        max_read_lines: 1,
        ..ToolLimits::default()
    };
    let replacement_limits = ToolLimits {
        max_read_lines: 2,
        ..ToolLimits::default()
    };
    let original = ToolEnvironment::with_limits(directory.path(), original_limits)
        .unwrap()
        .read();
    let replacement = ToolEnvironment::with_limits(directory.path(), replacement_limits)
        .unwrap()
        .read();
    let args = ReadArgs {
        path: "sample.txt".to_owned(),
        offset: None,
        limit: None,
    };
    let first = original.call(args.clone()).await.unwrap();
    assert_eq!(first.end_line, Some(1));
    assert_eq!(first.next_offset, Some(2));

    let updated = replacement.call(args.clone()).await.unwrap();
    assert_eq!(updated.end_line, Some(2));
    assert_eq!(updated.next_offset, Some(3));
    assert_eq!(original.call(args).await.unwrap(), first);
}
