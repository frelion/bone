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
