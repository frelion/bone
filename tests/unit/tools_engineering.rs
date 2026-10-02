use super::tests::assert_rejected;
use super::*;

fn matches(page: &Value) -> &[Value] {
    page["matches"].as_array().unwrap()
}

fn page(dir: &Path, query: &str, limit: usize, cursor: Option<&Value>) -> Value {
    let mut args = json!({"query":query,"limit":limit});
    if let Some(cursor) = cursor {
        args["cursor"] = cursor.clone();
    }
    search_files(dir, &args).unwrap()
}
fn edits(dir: &Path, source: &str, edits: Value) -> ToolOutcome {
    std::fs::write(dir.join("source"), source).unwrap();
    edit_file_prepared(
        dir,
        &json!({"path":"source","expected_sha256":sha256(source.as_bytes()),"edits":edits}),
        |_| Ok(()),
        |_| Ok(()),
    )
}

#[test]
fn search_native_schema_modes_and_final_bounds() {
    let readonly = definitions(true, true);
    let search = readonly
        .iter()
        .find(|tool| tool.name.as_str() == "search_files")
        .unwrap();
    assert_eq!(search.parameters["properties"]["limit"]["default"], 50);
    assert_eq!(search.parameters["properties"]["limit"]["maximum"], 200);
    assert!(
        !readonly
            .iter()
            .any(|tool| tool.name.as_str() == "edit_file")
    );
    assert!(is_external("search_files") && !is_write("search_files"));
    assert!(is_external("edit_file") && is_write("edit_file"));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), "q ".repeat(220)).unwrap();
    assert_eq!(
        search_files(dir.path(), &json!({"query":"q"})).unwrap()["matches"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
    assert!(
        page(dir.path(), "q", 200, None)["matches"]
            .as_array()
            .unwrap()
            .len()
            <= 200
    );
    for args in [
        json!({"query":""}),
        json!({"query":"q","limit":0}),
        json!({"query":"q","limit":201}),
        json!({"query":"q","limit":1.5}),
        json!({"query":"q","limit":"50"}),
    ] {
        assert!(search_files(dir.path(), &args).is_err());
    }
}

#[test]
fn short_line_search_returns_default50_max200_pages_without_unrelated_line_repetition() {
    let dir = tempfile::tempdir().unwrap();
    let source = "needle\r\n".repeat(240);
    std::fs::write(dir.path().join("many.txt"), &source).unwrap();
    let default = search_files(dir.path(), &json!({"query":"needle"})).unwrap();
    assert_eq!(matches(&default).len(), 50);
    let first = page(dir.path(), "needle", 200, None);
    assert_eq!(matches(&first).len(), 200);
    assert_eq!(first["truncated"], true);
    assert!(serde_json::to_vec(&first).unwrap().len() <= OUTPUT_LIMIT);
    for record in matches(&first) {
        assert_eq!(record["snippet"], "needle\r");
        assert_eq!(record["sha256"], sha256(source.as_bytes()));
    }
    let last = page(dir.path(), "needle", 200, Some(&first["next_cursor"]));
    assert_eq!(matches(&last).len(), 40);
    assert_eq!(last["matches"][0]["byte_offset"], 200 * 8);
    assert_eq!(last["matches"][0]["line"], 201);
    assert_eq!(last["truncated"], false);
    assert!(last["next_cursor"].is_null());
    assert_eq!(
        search_snippet("before needle\nAFTER", 7, "needle\n"),
        "before needle\n"
    );
    let source = format!(
        "{}命中\r\n下一行{}\n不要附带",
        "左".repeat(1600),
        "右".repeat(1600)
    );
    let offset = source.find("命中").unwrap();
    let snippet = search_snippet(&source, offset, "命中\r\n下一行");
    assert!(snippet.contains("命中\r\n下一行"));
    assert_eq!(snippet.chars().count(), 1024);
    assert!(!snippet.contains("不要附带"));
}

#[test]
fn unicode_crlf_paging_occurrences_and_whole_file_hash() {
    let dir = tempfile::tempdir().unwrap();
    let a = "é café café\r\n末 café\r\ntail not matched";
    std::fs::write(dir.path().join("a"), a).unwrap();
    std::fs::write(dir.path().join("z"), "café").unwrap();
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let current = page(dir.path(), "café", 1, cursor.as_ref());
        let records = matches(&current);
        assert_eq!(records.len(), 1);
        found.extend(records.iter().cloned());
        if current["next_cursor"].is_null() {
            assert_eq!(current["truncated"], false);
            break;
        }
        cursor = Some(current["next_cursor"].clone());
    }
    for (field, expected) in [
        ("path", json!(["a", "a", "a", "z"])),
        ("byte_offset", json!([3, 9, 20, 0])),
        ("line", json!([1, 1, 2, 1])),
    ] {
        assert_eq!(
            found
                .iter()
                .map(|value| value[field].clone())
                .collect::<Vec<_>>(),
            expected.as_array().unwrap().clone(),
            "{field}"
        );
    }
    for value in &found[..3] {
        assert_eq!(value["sha256"], sha256(a.as_bytes()));
        assert!(value["snippet"].as_str().unwrap().contains("café"));
    }
    let current = page(dir.path(), "café", 1, None);
    std::fs::write(dir.path().join("a"), format!("{a} altered tail")).unwrap();
    assert!(
        search_files(
            dir.path(),
            &json!({"query":"café","cursor":current["next_cursor"]})
        )
        .unwrap_err()
        .to_string()
        .contains("changed")
    );
}

#[test]
fn search_nonoverlap_and_empty_last_page() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), "aaaaa").unwrap();
    let first = page(dir.path(), "aa", 1, None);
    assert_eq!(first["matches"][0]["byte_offset"], 0);
    let last = page(dir.path(), "aa", 200, Some(&first["next_cursor"]));
    assert_eq!(last["matches"][0]["byte_offset"], 2);
    assert!(last["next_cursor"].is_null());
    assert_eq!(last["truncated"], false);
    assert!(
        page(dir.path(), "missing", 1, None)["matches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn search_output_ceiling_produces_contiguous_smaller_pages() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a"),
        format!("{}{}", "X".repeat(160), "é\\\n".repeat(1500)),
    )
    .unwrap();
    let mut cursor = None;
    let mut offsets = Vec::new();
    loop {
        let current = page(dir.path(), "X", 200, cursor.as_ref());
        assert!(serde_json::to_vec(&current).unwrap().len() <= OUTPUT_LIMIT);
        assert!(matches(&current).len() < 200);
        for value in matches(&current) {
            assert!(value["snippet"].as_str().unwrap().chars().count() <= 1024);
            offsets.push(value["byte_offset"].as_u64().unwrap());
        }
        if current["next_cursor"].is_null() {
            break;
        }
        cursor = Some(current["next_cursor"].clone());
    }
    assert_eq!(offsets, (0..160).collect::<Vec<u64>>());
    let query = "é".repeat(1200);
    std::fs::write(dir.path().join("long-query"), &query).unwrap();
    let result = search_files(dir.path(), &json!({"query":query,"path":"long-query"})).unwrap();
    assert_eq!(
        result["matches"][0]["snippet"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        1024
    );
}

#[test]
fn cursor_rejects_bad_checksum_version_position_request_removed_and_retyped_reference() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), "q q").unwrap();
    let first = page(dir.path(), "q", 1, None);
    let valid = first["next_cursor"].as_str().unwrap();
    for invalid in ["garbage".to_owned(), format!("{valid}0")] {
        assert!(search_files(dir.path(), &json!({"query":"q","cursor":invalid})).is_err());
    }
    for args in [
        json!({"query":"different","cursor":valid}),
        json!({"query":"q","path":"a","cursor":valid}),
    ] {
        assert!(search_files(dir.path(), &args).is_err());
    }
    let original = cursor_decode(valid).unwrap();
    for (version, byte_offset) in [(2, original.byte_offset), (1, 1)] {
        let mut cursor = cursor_decode(valid).unwrap();
        cursor.version = version;
        cursor.byte_offset = byte_offset;
        assert!(
            search_files(
                dir.path(),
                &json!({"query":"q","cursor":cursor_encode(&cursor).unwrap()})
            )
            .is_err()
        );
    }
    std::fs::remove_file(dir.path().join("a")).unwrap();
    assert!(search_files(dir.path(), &json!({"query":"q","cursor":valid})).is_err());
    std::fs::create_dir(dir.path().join("a")).unwrap();
    assert!(search_files(dir.path(), &json!({"query":"q","cursor":valid})).is_err());
}

#[test]
fn search_skips_binary_oversize_and_exclusions_without_touching_sources() {
    let dir = tempfile::tempdir().unwrap();
    for skip in SEARCH_EXCLUSIONS {
        std::fs::create_dir(dir.path().join(skip)).unwrap();
        std::fs::write(dir.path().join(skip).join("hidden"), "q").unwrap();
    }
    std::fs::write(dir.path().join("bad-utf8"), [0xff]).unwrap();
    std::fs::write(dir.path().join("binary"), b"q\0").unwrap();
    let huge = std::fs::File::create(dir.path().join("huge")).unwrap();
    huge.set_len(FILE_LIMIT + 1).unwrap();
    std::fs::write(dir.path().join("good"), "q").unwrap();
    let result = page(dir.path(), "q", 50, None);
    assert_eq!(matches(&result).len(), 1);
    assert_eq!(result["matches"][0]["path"], "good");
    assert_eq!(result["scanned_files"], 1);
    assert_eq!(result["skipped_files"], 3);
    assert_eq!(result["skipped_directories"], 4);
    let excluded = search_files(dir.path(), &json!({"query":"q","path":"target/hidden"})).unwrap();
    assert!(matches(&excluded).is_empty());
}

#[cfg(unix)]
#[test]
fn search_and_edit_reject_all_explicit_symlinks_and_bad_paths() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("source"), "q q").unwrap();
    std::fs::write(outside.path().join("file"), "q").unwrap();
    symlink(dir.path().join("source"), dir.path().join("inside-link")).unwrap();
    symlink(outside.path(), dir.path().join("dir-link")).unwrap();
    let result = page(dir.path(), "q", 50, None);
    assert_eq!(matches(&result).len(), 2);
    assert_eq!(result["skipped_files"], 2);
    for path in ["inside-link", "dir-link/file", "../source", "/tmp/absolute"] {
        assert!(search_files(dir.path(), &json!({"path":path,"query":"q"})).is_err());
        let outcome = edit_file_prepared(
            dir.path(),
            &json!({"path":path,"expected_sha256":sha256(b"q q"),"edits":[{"old_text":"q","new_text":"x"}]}),
            |_| Ok(()),
            |_| Ok(()),
        );
        assert_rejected(&outcome, "");
    }
    assert_eq!(std::fs::read(dir.path().join("source")).unwrap(), b"q q");
}

#[test]
fn edit_uses_original_spans_non_cascading_and_preserves_unicode_crlf() {
    let dir = tempfile::tempdir().unwrap();
    let source = "alpha\r\nβeta\r\n末\r\n";
    let outcome = edits(
        dir.path(),
        source,
        json!([{"old_text":"alpha","new_text":"βeta"},{"old_text":"βeta","new_text":"γamma"}]),
    );
    assert!(outcome.content.get("error").is_none());
    let expected = "βeta\r\nγamma\r\n末\r\n";
    assert_eq!(
        std::fs::read(dir.path().join("source")).unwrap(),
        expected.as_bytes()
    );
    assert_eq!(outcome.content["sha256"], sha256(expected.as_bytes()));
    assert_eq!(outcome.content["bytes"], expected.len());
    assert_eq!(outcome.content["edits_applied"], 2);
}

#[test]
fn edit_batch_validation_rejects_repeated_overlapping_absent_empty_and_stale_without_install() {
    let dir = tempfile::tempdir().unwrap();
    for (source, batch) in [
        ("aaa", json!([{"old_text":"aa","new_text":"x"}])),
        (
            "abc",
            json!([{"old_text":"ab","new_text":"x"},{"old_text":"bc","new_text":"y"}]),
        ),
        (
            "abc",
            json!([{"old_text":"ab","new_text":"x"},{"old_text":"ab","new_text":"y"}]),
        ),
        (
            "abc",
            json!([{"old_text":"ab","new_text":"x"},{"old_text":"missing","new_text":"y"}]),
        ),
        ("abc", json!([{"old_text":"","new_text":"x"}])),
        ("abc", json!([])),
    ] {
        let outcome = edits(dir.path(), source, batch);
        assert_rejected(&outcome, "");
        assert_eq!(
            std::fs::read(dir.path().join("source")).unwrap(),
            source.as_bytes()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    let outcome = edit_file_prepared(
        dir.path(),
        &json!({"path":"source","expected_sha256":"stale","edits":[{"old_text":"abc","new_text":"x"}]}),
        |_| Ok(()),
        |_| Ok(()),
    );
    assert!(outcome.content.get("error").is_some());
    assert_eq!(std::fs::read(dir.path().join("source")).unwrap(), b"abc");
}

#[cfg(unix)]
#[test]
fn edit_preserves_mode_and_reuses_final_identity_and_uncertain_durability() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source");
    std::fs::write(&path, "old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
    let args = json!({"path":"source","expected_sha256":sha256(b"old"),"edits":[{"old_text":"old","new_text":"new"}]});
    let outcome = edit_file_prepared(dir.path(), &args, |_| Ok(()), |_| Ok(()));
    assert!(outcome.content.get("error").is_none());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o751
    );
    std::fs::write(&path, "old").unwrap();
    let outcome = edit_file_prepared(
        dir.path(),
        &args,
        |_| Ok(()),
        |path| {
            std::fs::write(path, "raced")?;
            Ok(())
        },
    );
    assert_rejected(&outcome, "");
    assert_eq!(std::fs::read(&path).unwrap(), b"raced");
    std::fs::write(&path, "old").unwrap();
    let outcome = edit_file_prepared(
        dir.path(),
        &args,
        |_| Err(std::io::Error::other("durability fault")),
        |_| Ok(()),
    );
    assert!(outcome.uncertain);
    assert_eq!(outcome.content["effect"], "unknown");
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}

#[test]
fn edit_rejects_missing_directory_invalid_utf8_and_oversize_before_writes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("directory")).unwrap();
    std::fs::write(dir.path().join("invalid"), [0xff]).unwrap();
    std::fs::File::create(dir.path().join("huge"))
        .unwrap()
        .set_len(FILE_LIMIT + 1)
        .unwrap();
    for path in ["missing", "directory", "invalid", "huge"] {
        let outcome = edit_file_prepared(
            dir.path(),
            &json!({"path":path,"expected_sha256":"bad","edits":[{"old_text":"old","new_text":"new"}]}),
            |_| Ok(()),
            |_| Ok(()),
        );
        assert_rejected(&outcome, "");
    }
    assert!(!dir.path().join("missing").exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}
