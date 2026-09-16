use super::{DeviceCodePrompt, Error};

#[test]
fn service_errors_and_device_codes_are_redacted() {
    let error = Error::AuthorizationFailed;
    let rendered = format!("{error:?}: {error}");
    assert!(!rendered.contains("sentinel-secret-token"));
    assert!(!rendered.contains("Authorization: Bearer"));

    let prompt = DeviceCodePrompt {
        verification_uri: "https://auth.openai.com/codex/device".to_owned(),
        user_code: "SENTINEL-CODE".to_owned(),
    };
    let rendered = format!("{prompt:?}");
    assert!(rendered.contains("auth.openai.com"));
    assert!(!rendered.contains("SENTINEL-CODE"));
}

#[test]
fn cached_endpoint_construction_never_authorizes() {
    let directory = tempfile::tempdir().unwrap();
    let auth = directory.path().join("auth.json");
    let endpoint = super::connect_cached("chatgpt", &auth).unwrap();
    assert!(endpoint.model("offline-model").is_ok());
    assert!(!auth.exists());
    assert!(!auth.with_extension("lock").exists());
}
