#[test]
fn tui_manifest_has_no_product_backend_dependencies_besides_bone_app() {
    let manifest = include_str!("../Cargo.toml");
    for forbidden in [
        "bone-core",
        "bone-adapters",
        "rusqlite",
        "reqwest",
        "keyring",
    ] {
        assert!(
            !manifest.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with(forbidden)
                    && line[forbidden.len()..].starts_with(|character: char| {
                        character.is_whitespace() || character == '='
                    })
            }),
            "bone-tui must reach {forbidden} through bone-app, never depend on it directly"
        );
    }
}

#[test]
fn credential_effect_debug_output_is_always_redacted() {
    let raw = "sk-this-value-must-never-appear";
    let mut secret = bone_tui::state::SecretText::default();
    secret.extend(raw.chars());
    let effect = bone_tui::state::Effect::SetApiKey {
        session: bone_app::SessionId::new(),
        generation: 7,
        profile: bone_app::ProfileId::new("openai").unwrap(),
        key: secret,
    };
    let debug = format!("{effect:?}");
    assert!(!debug.contains(raw));
    assert!(debug.contains("<redacted>"));
}
