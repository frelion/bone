use super::*;
use rig_core::providers::{
    openai::{OpenAIConfig, Route},
    registry::ProviderConfig,
};

#[test]
fn native_config_roundtrip_preserves_route_endpoint_and_omits_key() {
    let config = ProviderConfig::OpenAi(
        OpenAIConfig::new("test-secret")
            .with_base_url("http://127.0.0.1:1234/v1")
            .with_route(Route::Responses),
    );
    let profile = Profile {
        model: ModelReference::Registry(ProviderRef::configured(config, "fixture").unwrap()),
        credential_env: Some("BONE_FIXTURE_KEY".into()),
        reuse_codex_login: false,
        additional_params: Some(serde_json::json!({"reasoning":{"effort":"low"}})),
        max_tokens: Some(100),
    };
    let config = Config {
        default_profile: "fixture".into(),
        profiles: BTreeMap::from([("fixture".into(), profile)]),
    };
    let text = toml::to_string(&config).unwrap();
    assert!(!text.contains("test-secret"));
    let restored: Config = toml::from_str(&text).unwrap();
    restored.validate().unwrap();
    let ModelReference::Registry(reference) = &restored.profile(None).unwrap().model else {
        panic!("registry recipe")
    };
    let ProviderConfig::OpenAi(native) = reference.config("") else {
        panic!("OpenAI config")
    };
    assert_eq!(native.base_url, "http://127.0.0.1:1234/v1");
    assert_eq!(native.route, Some(Route::Responses));
}

#[test]
fn unsafe_profile_names_and_empty_companion_models_are_rejected() {
    for name in ["", "..", "../other", "a/b", "a\\b"] {
        assert!(validate_profile_name(name).is_err());
    }
    let profile = Profile {
        model: ModelReference::Bedrock { bedrock: "".into() },
        credential_env: None,
        reuse_codex_login: false,
        additional_params: None,
        max_tokens: None,
    };
    assert!(profile.validate().is_err());
}

#[test]
fn human_connection_names_allow_languages_and_spaces_without_accepting_path_or_toml_syntax() {
    for name in [
        "团队开发 API",
        "Café API",
        "機械学習_GPU2",
        "فريق٢",
        "dev-team_v2",
    ] {
        validate_profile_name(name).unwrap();
    }
    validate_profile_name(&"a".repeat(80)).unwrap();
    validate_profile_name(&"团".repeat(26)).unwrap();
    assert!(validate_profile_name(&"a".repeat(81)).is_err());
    assert!(validate_profile_name(&"团".repeat(27)).is_err());
    for name in [
        "",
        " ",
        " leading",
        "trailing ",
        ".",
        "..",
        "团队/开发",
        "团队\\开发",
        "团队.开发",
        "团队\0开发",
        "团队\n开发",
        "团队\t开发",
        "团队\r开发",
        "[团队]",
        "团队=开发",
        "团队\"开发",
        "团队'开发",
        "团队#开发",
        "团队:开发",
        "团队$开发",
        "团队`开发",
        "团队;开发",
        "团队\u{00a0}开发",
        "团队\u{200b}开发",
        "团队\u{202e}开发",
        "团队／开发",
    ] {
        assert!(validate_profile_name(name).is_err(), "{name:?}");
    }
}

#[test]
fn unicode_connection_names_survive_real_toml_save_as_exact_profile_keys() {
    let data = tempfile::tempdir().unwrap();
    let (_, revision) = Config::load_with_revision(data.path()).unwrap();
    let first = "团队开发 API";
    let second = "Café Production";
    let first_profile = Profile::from_model("openai:future-native-model").unwrap();
    let second_profile = Profile::from_model("anthropic:another-native-model").unwrap();
    let config = Config {
        default_profile: first.into(),
        profiles: BTreeMap::from([
            (first.into(), first_profile),
            (second.into(), second_profile),
        ]),
    };
    config.save_checked(data.path(), &revision).unwrap();
    let text = std::fs::read_to_string(data.path().join("config.toml")).unwrap();
    let document: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(document["default_profile"].as_str(), Some(first));
    let profiles = document["profiles"].as_table().unwrap();
    assert_eq!(profiles.len(), 2);
    assert!(profiles.contains_key(first));
    assert!(profiles.contains_key(second));
    let restored = Config::load(data.path()).unwrap();
    assert_eq!(restored.default_profile, first);
    assert_eq!(
        serde_json::to_value(restored.profile(None).unwrap()).unwrap(),
        serde_json::to_value(config.profile(Some(first)).unwrap()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(restored.profile(Some(second)).unwrap()).unwrap(),
        serde_json::to_value(config.profile(Some(second)).unwrap()).unwrap()
    );
}

#[test]
fn unicode_credentials_stay_in_distinct_profile_directories_and_invalid_names_cannot_write() {
    let data = tempfile::tempdir().unwrap();
    let profile = Profile::from_model("openai:fixture-model").unwrap();
    let first = "团队开发 API";
    let second = "Café Production";
    crate::model::save_api_key(data.path(), first, &profile, "synthetic-first-key").unwrap();
    crate::model::save_api_key(data.path(), second, &profile, "synthetic-second-key").unwrap();
    let first_path = crate::model::api_key_file(data.path(), first).unwrap();
    let second_path = crate::model::api_key_file(data.path(), second).unwrap();
    assert_eq!(
        first_path,
        data.path().join("profiles").join(first).join("api-key")
    );
    assert_eq!(
        second_path,
        data.path().join("profiles").join(second).join("api-key")
    );
    assert_ne!(first_path, second_path);
    let first_bytes = std::fs::read(&first_path).unwrap();
    let second_bytes = std::fs::read(&second_path).unwrap();
    assert_ne!(first_bytes, second_bytes);
    assert!(crate::model::has_api_key(data.path(), first).unwrap());
    assert!(crate::model::has_api_key(data.path(), second).unwrap());
    let subscription = Profile::from_model("chatgpt:fixture-model").unwrap();
    let auth = crate::model::auth_file(data.path(), first).unwrap();
    assert_eq!(auth, first_path.with_file_name("auth.json"));
    assert!(!crate::model::has_login(&subscription, data.path(), first).unwrap());
    std::fs::write(&auth, br#"{"synthetic_cache":true}"#).unwrap();
    assert!(crate::model::has_login(&subscription, data.path(), first).unwrap());
    assert!(!crate::model::has_login(&subscription, data.path(), second).unwrap());
    for name in [
        "../outside",
        "团队/开发",
        "团队\\开发",
        "团队\n开发",
        "[profiles]",
    ] {
        assert!(
            crate::model::save_api_key(data.path(), name, &profile, "synthetic-invalid-key")
                .is_err()
        );
        assert!(crate::model::auth_file(data.path(), name).is_err());
    }
    assert_eq!(std::fs::read(&first_path).unwrap(), first_bytes);
    assert_eq!(std::fs::read(&second_path).unwrap(), second_bytes);
    crate::model::remove_api_key(data.path(), first).unwrap();
    assert!(!crate::model::has_api_key(data.path(), first).unwrap());
    assert!(crate::model::has_api_key(data.path(), second).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&second_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(second_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[cfg(unix)]
#[test]
fn unicode_profile_names_preserve_existing_credential_symlink_protection() {
    let data = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let name = "团队开发 API";
    std::fs::create_dir(data.path().join("profiles")).unwrap();
    std::os::unix::fs::symlink(outside.path(), data.path().join("profiles").join(name)).unwrap();
    let profile = Profile::from_model("openai:fixture-model").unwrap();
    assert!(crate::model::save_api_key(data.path(), name, &profile, "synthetic-key").is_err());
    assert!(crate::model::has_api_key(data.path(), name).is_err());
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn configuration_save_is_atomic_private_and_rejects_external_revisions() {
    let data = tempfile::tempdir().unwrap();
    let (mut config, revision) = Config::load_with_revision(data.path()).unwrap();
    config.profiles.insert(
        "alternate".into(),
        Profile::from_model("anthropic:any-future-model").unwrap(),
    );
    config.default_profile = "alternate".into();
    let saved = config.save_checked(data.path(), &revision).unwrap();
    assert_eq!(
        Config::load(data.path()).unwrap().default_profile,
        "alternate"
    );
    assert!(config.save_checked(data.path(), &revision).is_err());
    let path = data.path().join("config.toml");
    let external = format!(
        "{}\n# external edit\n",
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, &external).unwrap();
    assert!(config.save_checked(data.path(), &saved).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), external);
    assert!(std::fs::read_dir(data.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bone-save-")
    }));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(data.path().join("config.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn model_changes_keep_native_recipe_only_within_the_same_provider() {
    let mut profile = Profile::from_model("openai:first-model")
        .unwrap()
        .with_endpoint("http://127.0.0.1:9123/v1")
        .unwrap();
    profile.credential_env = Some("EXPLICIT_PROFILE_VARIABLE".into());
    profile.additional_params = Some(serde_json::json!({"reasoning":{"effort":"low"}}));
    let next = profile
        .with_model("openai:new-model-not-in-any-list")
        .unwrap();
    assert_eq!(next.endpoint(), profile.endpoint());
    assert_eq!(next.credential_env, profile.credential_env);
    assert_eq!(next.additional_params, profile.additional_params);
    let ModelReference::Registry(reference) = &next.model else {
        panic!("native reference");
    };
    assert_eq!(reference.model(), "new-model-not-in-any-list");
    let other = next.with_model("anthropic:another-future-model").unwrap();
    assert!(other.credential_env.is_none());
    assert_ne!(other.endpoint(), next.endpoint());
    assert!(other.additional_params.is_none());
    assert!(next.with_endpoint("relative/path").is_err());
}

#[test]
fn endpoint_updates_preserve_native_route_and_provider_options() {
    let config = ProviderConfig::OpenAi(OpenAIConfig::new("").with_route(Route::Responses));
    let mut profile = Profile::from_model("openai:fixture").unwrap();
    profile.model = ModelReference::Registry(ProviderRef::configured(config, "fixture").unwrap());
    let next = profile.with_endpoint("http://127.0.0.1:1234/v1").unwrap();
    let ModelReference::Registry(reference) = next.model else {
        panic!("native reference");
    };
    let ProviderConfig::OpenAi(native) = reference.config("") else {
        panic!("native config");
    };
    assert_eq!(native.base_url, "http://127.0.0.1:1234/v1");
    assert_eq!(native.route, Some(Route::Responses));
}
