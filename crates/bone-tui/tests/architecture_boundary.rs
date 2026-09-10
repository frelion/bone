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
