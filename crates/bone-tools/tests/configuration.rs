use std::time::Duration;

use bone_config::{ConfigError, ConfigManager, ConfigSection};
use bone_tools::{ReadArgs, Tool, ToolEnvironment, ToolLimits};
use serde_json::{Value, json};

fn config_manager() -> (tempfile::TempDir, ConfigManager) {
    let directory = tempfile::tempdir().unwrap();
    let manager = ConfigManager::builder()
        .register::<ToolLimits>()
        .unwrap()
        .build(directory.path().join("config.json"))
        .unwrap();
    (directory, manager)
}

#[test]
fn omitted_settings_use_defaults_and_deadlines_round_trip_as_integer_seconds() {
    let (directory, manager) = config_manager();
    let defaults: ToolLimits = serde_json::from_value(json!({})).unwrap();
    assert_eq!(defaults, ToolLimits::default());

    let initial = manager.snapshot().unwrap();
    manager
        .set_value(
            ToolLimits::KEY,
            json!({
                "max_read_lines": 2,
                "default_bash_timeout_seconds": 3,
                "max_bash_timeout_seconds": 10
            }),
            initial.revision(),
        )
        .unwrap();
    let snapshot = manager.snapshot().unwrap();
    let limits = snapshot.get::<ToolLimits>().unwrap().unwrap();
    assert_eq!(
        limits,
        ToolLimits {
            max_read_lines: 2,
            default_bash_timeout: Duration::from_secs(3),
            max_bash_timeout: Duration::from_secs(10),
            ..ToolLimits::default()
        }
    );

    manager.set(&limits, snapshot.revision()).unwrap();
    let document: Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("config.json")).unwrap())
            .unwrap();
    let stored = &document[ToolLimits::KEY];
    assert_eq!(stored["default_bash_timeout_seconds"], 3);
    assert_eq!(stored["max_bash_timeout_seconds"], 10);
    assert!(stored.get("default_bash_timeout").is_none());
    assert!(stored.get("max_bash_timeout").is_none());
    assert_eq!(
        manager.snapshot().unwrap().get::<ToolLimits>().unwrap(),
        Some(limits)
    );
}

#[test]
fn schema_describes_optional_defaults_and_integer_second_deadlines() {
    let schema = ToolLimits::schema();
    let defaults = serde_json::to_value(ToolLimits::default()).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert!(schema.get("required").is_none() || schema["required"].as_array().unwrap().is_empty());
    let properties = schema["properties"].as_object().unwrap();
    assert_eq!(properties.len(), defaults.as_object().unwrap().len());
    for (field, default) in defaults.as_object().unwrap() {
        assert_eq!(properties[field]["type"], "integer", "{field}");
        assert_eq!(properties[field]["minimum"], 1, "{field}");
        assert_eq!(&properties[field]["default"], default, "{field}");
    }
}

#[test]
fn invalid_limits_and_unknown_fields_cannot_change_stored_configuration() {
    let (directory, manager) = config_manager();
    let revision = manager.snapshot().unwrap().revision().clone();
    for invalid in [
        json!({"max_output_bytes": 0}),
        json!({"max_read_lines": 0}),
        json!({"max_search_file_bytes": 0}),
        json!({"max_patch_total_bytes": 0}),
        json!({"default_bash_timeout_seconds": 0}),
        json!({"max_bash_timeout_seconds": 0}),
        json!({"default_bash_timeout_seconds": 10, "max_bash_timeout_seconds": 9}),
        json!({"max_read_lienes": 10}),
        json!({"default_bash_timeout": {"secs": 1, "nanos": 0}}),
        json!({"default_bash_timeout_seconds": {"secs": 1, "nanos": 0}}),
        json!({"default_bash_timeout_seconds": 1.5}),
        json!({"max_bash_timeout_seconds": -1}),
    ] {
        assert!(matches!(
            manager.set_value(ToolLimits::KEY, invalid, &revision),
            Err(ConfigError::InvalidSection { .. })
        ));
    }
    assert_eq!(manager.snapshot().unwrap().revision(), &revision);
    assert!(!directory.path().join("config.json").exists());
}

#[test]
fn subsecond_rust_deadlines_are_never_silently_truncated_when_persisted() {
    let (directory, manager) = config_manager();
    let revision = manager.snapshot().unwrap().revision().clone();
    for limits in [
        ToolLimits {
            default_bash_timeout: Duration::from_millis(500),
            ..ToolLimits::default()
        },
        ToolLimits {
            max_bash_timeout: Duration::from_millis(600_500),
            ..ToolLimits::default()
        },
    ] {
        assert!(ToolEnvironment::with_limits(directory.path(), limits.clone()).is_ok());
        assert!(ConfigSection::validate(&limits).is_err());
        assert!(serde_json::to_value(&limits).is_err());
        assert!(manager.set(&limits, &revision).is_err());
    }
    assert_eq!(manager.snapshot().unwrap().revision(), &revision);
    assert!(!directory.path().join("config.json").exists());
}

#[tokio::test]
async fn persisted_limits_affect_new_tools_while_existing_tools_keep_their_limits() {
    let (directory, manager) = config_manager();
    tokio::fs::write(directory.path().join("sample.txt"), "one\ntwo\nthree\n")
        .await
        .unwrap();
    let initial = manager.snapshot().unwrap();
    manager
        .set_value(
            ToolLimits::KEY,
            json!({"max_read_lines": 1}),
            initial.revision(),
        )
        .unwrap();
    let snapshot = manager.snapshot().unwrap();
    let original = ToolEnvironment::with_limits(
        directory.path(),
        snapshot.get::<ToolLimits>().unwrap().unwrap(),
    )
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

    manager
        .set_value(
            ToolLimits::KEY,
            json!({"max_read_lines": 2}),
            snapshot.revision(),
        )
        .unwrap();
    let replacement = ToolEnvironment::with_limits(
        directory.path(),
        manager
            .snapshot()
            .unwrap()
            .get::<ToolLimits>()
            .unwrap()
            .unwrap(),
    )
    .unwrap()
    .read();
    let updated = replacement.call(args.clone()).await.unwrap();
    assert_eq!(updated.end_line, Some(2));
    assert_eq!(updated.next_offset, Some(3));
    assert_eq!(original.call(args).await.unwrap(), first);

    assert!(
        replacement
            .call(ReadArgs {
                path: "sample.txt".to_owned(),
                offset: None,
                limit: Some(3),
            })
            .await
            .is_err()
    );
}
