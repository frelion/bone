use super::*;

#[test]
fn changing_a_model_keeps_endpoint_and_does_not_move_credentials_between_providers() {
    let current: Profile = serde_json::from_value(serde_json::json!({
        "model":{"model":"old","config":{"openai":{"api_key":"","base_url":"http://127.0.0.1:1234/v1","dialect":"openai","route":"Responses","auth":"Bearer"}}},
        "credential_env":"LOCAL_FIXTURE_KEY","max_tokens":4000
    })).unwrap();
    let next = profile_with_model(&current, "openai:new-model").unwrap();
    let encoded = serde_json::to_value(&next).unwrap();
    assert_eq!(
        encoded["model"]["config"]["openai"]["base_url"],
        "http://127.0.0.1:1234/v1"
    );
    assert_eq!(encoded["model"]["model"], "new-model");
    assert_eq!(next.credential_env, current.credential_env);
    let changed = profile_with_model(&current, "ollama:arbitrary-native-name").unwrap();
    assert!(changed.credential_env.is_none());
    assert!(profile_with_model(&current, "missing-provider:model").is_err());
}

#[test]
fn write_preview_describes_proposed_content_without_fabricating_additions() {
    let preview = proposed_change_preview(
        "write_file",
        &serde_json::json!({"content":"existing replacement\nsecond line"}),
    );
    assert!(preview.contains("拟写入内容（可能覆盖现有文件）"));
    assert!(preview.contains("```text\nexisting replacement\nsecond line\n```"));
    assert!(!preview.contains("```diff"));
    assert!(!preview.contains("+ existing"));
}

#[test]
fn tool_projection_preserves_real_lines_and_failed_shell_status() {
    assert_eq!(
        readable_tool_output(r#"{"text":"第一行\n第二行","bytes":22}"#),
        "第一行\n第二行"
    );
    assert_eq!(
        readable_tool_output(r#"{"exit_code":2,"stdout":"out\n","stderr":"problem"}"#),
        "exit: 2\nout\n\nstderr:\nproblem"
    );
    assert!(
        readable_tool_output(r#"{"error":"cancelled","effect":"unknown"}"#).contains("需要核查")
    );
}
