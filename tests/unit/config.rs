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
