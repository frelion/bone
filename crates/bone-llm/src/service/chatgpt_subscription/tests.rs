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
