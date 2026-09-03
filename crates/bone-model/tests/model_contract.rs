use bone_model::{
    ConfigError, Protocol,
    protocol::openai_responses,
    rig::{providers::openai as rig_openai, test_utils::RecordingHttpClient},
};

#[test]
fn separates_endpoint_protocol_and_model_identity() {
    let client_a = rig_openai::Client::builder()
        .api_key("secret-a")
        .http_client(RecordingHttpClient::default())
        .build()
        .expect("test client should build");
    let client_b = rig_openai::Client::builder()
        .api_key("secret-b")
        .http_client(RecordingHttpClient::default())
        .build()
        .expect("test client should build");
    let endpoint_a = openai_responses::from_client("gateway-a", client_a).unwrap();
    let endpoint_b = openai_responses::from_client("gateway-b", client_b).unwrap();
    let model_a = endpoint_a.model("shared-model").unwrap();
    let model_b = endpoint_b.model("shared-model").unwrap();

    assert_eq!(model_a.endpoint_id(), "gateway-a");
    assert_eq!(model_b.endpoint_id(), "gateway-b");
    assert_eq!(model_a.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model_b.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model_a.id(), model_b.id());
    assert_eq!(Protocol::OpenAiResponses.as_str(), "openai-responses");
}

#[test]
fn endpoint_debug_and_configuration_errors_do_not_expose_credentials() {
    let credential = "do-not-print-this-secret";
    let endpoint = openai_responses::official("openai-primary", credential).unwrap();

    let debug = format!("{endpoint:?}");
    assert!(debug.contains("openai-primary"));
    assert!(debug.contains("OpenAiResponses"));
    assert!(!debug.contains(credential));

    let invalid_credential = "secret\nheader";
    let error = openai_responses::official("openai-primary", invalid_credential).unwrap_err();
    assert_eq!(error, ConfigError::InvalidApiKey);
    assert!(!error.to_string().contains(invalid_credential));
}
