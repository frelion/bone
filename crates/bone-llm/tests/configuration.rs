#![cfg(not(target_arch = "wasm32"))]

use bone_config::{ConfigManager, ConfigSection};
use bone_llm::{LlmConfig, service::chatgpt_subscription};
use serde_json::json;

#[test]
fn settings_round_trip_through_the_shared_store() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let manager = ConfigManager::builder()
        .register::<LlmConfig>()
        .unwrap()
        .build(&path)
        .unwrap();

    manager
        .set_value(
            LlmConfig::KEY,
            json!({}),
            manager.snapshot().unwrap().revision(),
        )
        .unwrap();
    let snapshot = manager.snapshot().unwrap();
    assert_eq!(
        snapshot.get::<LlmConfig>().unwrap(),
        Some(LlmConfig::default())
    );

    let credential_root = directory.path().join("independent-credentials");
    let config = LlmConfig {
        credential_root: Some(credential_root.clone()),
    };
    manager.set(&config, snapshot.revision()).unwrap();
    let reopened = ConfigManager::builder()
        .register::<LlmConfig>()
        .unwrap()
        .build(&path)
        .unwrap();
    let old_snapshot = reopened.snapshot().unwrap();
    let old_config = old_snapshot.get::<LlmConfig>().unwrap().unwrap();
    assert_eq!(old_config, config);
    assert_eq!(
        old_config.resolve_credential_root().unwrap(),
        credential_root
    );

    let next_root = directory.path().join("next-credentials");
    manager
        .set(
            &LlmConfig {
                credential_root: Some(next_root.clone()),
            },
            old_snapshot.revision(),
        )
        .unwrap();
    let next_config = reopened
        .snapshot()
        .unwrap()
        .get::<LlmConfig>()
        .unwrap()
        .unwrap();
    assert_eq!(next_config.resolve_credential_root().unwrap(), next_root);
    assert_eq!(
        old_snapshot
            .get::<LlmConfig>()
            .unwrap()
            .unwrap()
            .resolve_credential_root()
            .unwrap(),
        credential_root
    );
    assert!(!credential_root.exists());
    assert!(!next_root.exists());
    assert_eq!(
        manager.schema(LlmConfig::KEY).unwrap()["additionalProperties"],
        false
    );
}

#[test]
fn invalid_settings_are_rejected_without_changing_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let manager = ConfigManager::builder()
        .register::<LlmConfig>()
        .unwrap()
        .build(&path)
        .unwrap();
    manager
        .set(
            &LlmConfig::default(),
            manager.snapshot().unwrap().revision(),
        )
        .unwrap();
    let snapshot = manager.snapshot().unwrap();
    let before = std::fs::read(&path).unwrap();
    for value in [
        json!({"credential_root": "relative/credentials"}),
        json!({"credential_root": ""}),
        json!({"credential_root": directory.path().join("bad\0path")}),
        json!({"credential_root": 1}),
        json!({"credential_rooot": "/misspelled"}),
    ] {
        assert!(
            manager
                .set_value(LlmConfig::KEY, value, snapshot.revision())
                .is_err()
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn credential_root_resolution_does_not_touch_storage() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("not-created");
    let config = LlmConfig {
        credential_root: Some(root.clone()),
    };
    assert_eq!(config.resolve_credential_root().unwrap(), root);
    assert!(!root.exists());

    let relative = LlmConfig {
        credential_root: Some("relative/credentials".into()),
    };
    assert_eq!(
        relative.resolve_credential_root(),
        Err(chatgpt_subscription::Error::CredentialStoreUnavailable)
    );
    assert_eq!(
        LlmConfig::default().resolve_credential_root(),
        chatgpt_subscription::default_credential_root()
    );
}
