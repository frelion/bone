use std::{
    fs,
    path::{Component, Path},
};

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
fn terminal_state_changes_stay_inside_the_terminal_module() {
    const TERMINAL_COMMANDS: &[&str] = &[
        "EnterAlternateScreen",
        "LeaveAlternateScreen",
        "EnableMouseCapture",
        "DisableMouseCapture",
        "EnableBracketedPaste",
        "DisableBracketedPaste",
        "PushKeyboardEnhancementFlags",
        "PopKeyboardEnhancementFlags",
        "enable_raw_mode",
        "disable_raw_mode",
    ];
    const HOST_COSMETIC_MUTATIONS: &[&str] = &["SetCursorStyle", "SetTitle", "SetSize"];
    const STANDARD_STREAM_CALLS: &[&str] = &["stdout()", "stderr()"];

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    visit_rust_sources(&source_root, &mut |path| {
        let relative = path.strip_prefix(&source_root).unwrap();
        let source = fs::read_to_string(path).unwrap();
        let production = production_before_test_modules(&source);
        let first = relative.components().next();
        let terminal_owned = matches!(
            first,
            Some(Component::Normal(name)) if name == "terminal" || name == "terminal.rs"
        );

        if !terminal_owned {
            for command in TERMINAL_COMMANDS {
                assert!(
                    !production.contains(command),
                    "{} changes terminal state through {command}; route it through terminal",
                    relative.display()
                );
            }
            for stream in STANDARD_STREAM_CALLS {
                assert!(
                    !production.contains(stream),
                    "{} opens {stream} directly; route terminal output through terminal",
                    relative.display()
                );
            }
            assert!(
                !contains_raw_terminal_escape(production),
                "{} embeds a raw ESC/CSI sequence; route terminal output through terminal",
                relative.display()
            );
        }

        for command in HOST_COSMETIC_MUTATIONS {
            assert!(
                !production.contains(command),
                "{} mutates cosmetic host state through {command}",
                relative.display()
            );
        }
    });
}

#[test]
fn native_windows_terminal_state_has_an_exact_snapshot_boundary() {
    let source = include_str!("../src/terminal/modes.rs");
    for required in [
        "ConsoleMode",
        "Handle::input_handle",
        "Handle::output_handle",
        "ENABLE_VIRTUAL_TERMINAL_PROCESSING",
        "RestoreAction::HostConsole",
    ] {
        assert!(
            source.contains(required),
            "native Windows terminal ownership must include {required}"
        );
    }
}

fn production_before_test_modules(source: &str) -> &str {
    source
        .match_indices("#[cfg(test)]")
        .find_map(|(offset, attribute)| {
            source[offset + attribute.len()..]
                .trim_start()
                .starts_with("mod ")
                .then_some(&source[..offset])
        })
        .unwrap_or(source)
}

fn contains_raw_terminal_escape(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    source.contains('\u{1b}')
        || source.contains('\u{9b}')
        || source.contains(r"\x1b")
        || source.contains(r"\x9b")
        || contains_unicode_escape(&source, 0x1b)
        || contains_unicode_escape(&source, 0x9b)
}

fn contains_unicode_escape(source: &str, scalar: u32) -> bool {
    source.match_indices(r"\u{").any(|(offset, prefix)| {
        let digits = &source[offset + prefix.len()..];
        digits.find('}').is_some_and(|end| {
            let digits = digits[..end].replace('_', "");
            u32::from_str_radix(&digits, 16) == Ok(scalar)
        })
    })
}

#[test]
fn view_components_use_the_shared_typography_and_color_system() {
    let view_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/view");
    visit_rust_sources(&view_root, &mut |path| {
        let source = fs::read_to_string(path).unwrap();
        let production = source
            .split_once("#[cfg(test)]")
            .map_or(source.as_str(), |(production, _)| production);
        for forbidden in ["Color::Rgb(", ".bold()", "Modifier::BOLD"] {
            assert!(
                !production.contains(forbidden),
                "{} contains component-local styling through {forbidden}; use ui::theme",
                path.strip_prefix(&view_root).unwrap().display()
            );
        }
    });
}

fn visit_rust_sources(root: &Path, visit: &mut impl FnMut(&Path)) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit_rust_sources(&path, visit);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            visit(&path);
        }
    }
}
