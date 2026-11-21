use std::fs;

use super::CodexPatchConfig;

#[test]
fn freeform_binding_accepts_raw_provider_input_and_rejects_function_arguments() {
    use proteus_contracts::{
        domain::{ToolCall, ToolCallSurface, ToolSpec, ToolSurface, validate_tool_call_args},
        process_module::ToolModule,
    };
    let tool = super::tool::ApplyPatchTool {
        config: CodexPatchConfig {
            freeform: true,
            ..Default::default()
        },
    };
    let spec: ToolSpec = serde_json::from_str(&tool.spec_json()).unwrap();
    assert_eq!(
        spec.surface,
        ToolSurface::freeform_lark(include_str!("upstream/apply_patch.lark"))
    );
    let input = patch("*** Add File: f\n+hello");
    let call = ToolCall::new("test", "apply_patch", serde_json::json!({"input":input}))
        .with_surface(ToolCallSurface::Freeform);
    assert_eq!(validate_tool_call_args(&call, &spec), None);
    let wrong = ToolCall::new("test", "apply_patch", serde_json::json!({"patch":input}));
    assert!(validate_tool_call_args(&wrong, &spec).is_some());
}

fn apply_patch(input: &str, workspace: &std::path::Path) -> Result<String, String> {
    super::apply_patch(input, workspace, &CodexPatchConfig::default())
}

#[test]
fn self_move_guard_rejects_normalized_alias_before_any_write() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), "old\n").unwrap();
    let input = patch(
        "*** Add File: new\n+created\n*** Update File: f\n*** Move to: ./f\n@@\n-old\n+updated",
    );
    let error = super::apply_patch(
        &input,
        dir.path(),
        &CodexPatchConfig {
            reject_self_move: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("self-move rejected"), "{error}");
    assert_eq!(fs::read_to_string(dir.path().join("f")).unwrap(), "old\n");
    assert!(!dir.path().join("new").exists());

    apply_patch(&input, dir.path()).unwrap();
    assert!(
        !dir.path().join("f").exists(),
        "unconfigured mode follows pinned Codex"
    );
}

#[test]
fn patch_config_rejects_unknown_fields_and_wrong_flag_types() {
    assert!(
        serde_json::from_value::<CodexPatchConfig>(serde_json::json!({"unknown":true})).is_err()
    );
    assert!(
        serde_json::from_value::<CodexPatchConfig>(serde_json::json!({"reject_self_move":"true"}))
            .is_err()
    );
}

fn patch(body: &str) -> String {
    format!("*** Begin Patch\n{body}\n*** End Patch")
}

#[test]
fn context_and_eof_select_the_last_matching_block_and_keep_a_newline() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("f"),
        "fn first() {\nold\nfn last() {\nold\n",
    )
    .unwrap();
    let result = apply_patch(
        &patch("*** Update File: f\n@@ fn last() {\n-old\n+new\n*** End of File"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("f")).unwrap(),
        "fn first() {\nold\nfn last() {\nnew\n"
    );
    assert_eq!(result, "Success. Updated the following files:\nM f\n");

    let error = apply_patch(
        &patch("*** Update File: f\n@@\n-old\n+bad\n*** End of File"),
        dir.path(),
    )
    .unwrap_err();
    assert!(error.contains("Failed to find expected lines"), "{error}");
    assert!(
        fs::read_to_string(dir.path().join("f"))
            .unwrap()
            .ends_with("new\n")
    );
}

#[test]
fn matching_uses_exact_before_whitespace_and_unicode_passes() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), "  same  \nsame\n  “hello”—world\n").unwrap();
    apply_patch(
        &patch("*** Update File: f\n@@\n-same\n+exact\n@@\n-\"hello\"-world\n+normalized"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("f")).unwrap(),
        "  same  \nexact\nnormalized\n"
    );
}

#[test]
fn unicode_fallback_compares_whole_lines_and_every_line_in_the_block() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    let prefixes = "α—\nα—βx\nα—β\na b extra\n";
    fs::write(&file, format!("{prefixes}  α—β  \na\u{a0}b\n")).unwrap();

    apply_patch(
        &patch("*** Update File: f\n@@\n-α-β\n-a\u{2009}b\n+updated"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(file).unwrap(),
        format!("{prefixes}updated\n")
    );
}

#[test]
fn bare_empty_context_and_missing_final_newline_follow_pinned_default() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), "one\n\ntwo").unwrap();
    apply_patch(
        &patch("*** Update File: f\n one\n\n-two\n+three\n "),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("f")).unwrap(),
        "one\n\nthree\n"
    );
}

#[test]
fn pure_insertions_and_multiple_chunks_use_original_indices() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), "a\nb\nc\n").unwrap();
    apply_patch(
        &patch("*** Update File: f\n@@\n+tail\n@@\n-b\n+B\n+extra\n@@\n-c\n+C"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("f")).unwrap(),
        "a\nB\nextra\nC\ntail\n"
    );
}

#[test]
fn eof_deletion_can_shorten_an_earlier_replacement_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    fs::write(&path, "keep\nfirst\nlast\n").unwrap();
    apply_patch(
        &patch("*** Update File: f\n@@\n-first\n-last\n+new\n@@\n-last\n*** End of File"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "keep\nnew\n");
}

#[test]
fn equal_position_insertions_preserve_patch_order_before_final_blank_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    fs::write(&path, "keep\n\n").unwrap();
    apply_patch(
        &patch("*** Update File: f\n@@\n+first\n+second\n@@\n+third\n+fourth"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "keep\nfirst\nsecond\nthird\nfourth\n"
    );
}

#[test]
fn add_and_move_overwrite_destinations_and_summary_groups_operation_kinds() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("source"), "old\n").unwrap();
    fs::write(dir.path().join("dest"), [0xff]).unwrap();
    fs::write(dir.path().join("added"), [0xff]).unwrap();
    fs::write(dir.path().join("delete"), "gone").unwrap();
    let result = apply_patch(&patch("*** Delete File: delete\n*** Update File: source\n*** Move to: dest\n@@\n-old\n+new\n*** Add File: added\n+replacement\n*** Add File: nested/empty"), dir.path()).unwrap();
    assert_eq!(
        result,
        "Success. Updated the following files:\nA added\nA nested/empty\nM dest\nD delete\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("dest")).unwrap(),
        "new\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("added")).unwrap(),
        "replacement\n"
    );
    assert_eq!(fs::read(dir.path().join("nested/empty")).unwrap(), b"");
    assert!(!dir.path().join("source").exists());
    assert!(!dir.path().join("delete").exists());
}

#[test]
fn verification_rejects_bad_later_update_duplicate_targets_and_move_only() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), "original\n").unwrap();
    for body in [
        "*** Add File: added\n+ok\n*** Update File: f\n@@\n-missing\n+bad",
        "*** Add File: added\n+ok\n*** Add File: ./added\n+again",
        "*** Add File: added\n+ok\n*** Update File: f\n*** Move to: dest",
    ] {
        let error = apply_patch(&patch(body), dir.path()).unwrap_err();
        assert!(
            error.starts_with("apply_patch verification failed:"),
            "{error}"
        );
        assert!(!dir.path().join("added").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("f")).unwrap(),
            "original\n"
        );
    }
}

#[test]
fn application_failure_keeps_completed_writes() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("directory")).unwrap();
    let error = apply_patch(
        &patch("*** Add File: written\n+kept\n*** Add File: directory\n+cannot write"),
        dir.path(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        format!(
            "Failed to write file {}",
            dir.path().join("directory").display()
        )
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("written")).unwrap(),
        "kept\n"
    );
}

#[test]
fn application_rereads_after_move_instead_of_using_stale_verified_contents() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a"), "A\n").unwrap();
    fs::write(dir.path().join("b"), "B\n").unwrap();
    let error = apply_patch(&patch("*** Update File: a\n*** Move to: b\n@@\n-A\n+moved\n*** Update File: b\n@@\n-B\n+stale"), dir.path()).unwrap_err();
    assert!(
        error.starts_with("Failed to find expected lines"),
        "{error}"
    );
    assert!(!dir.path().join("a").exists());
    assert_eq!(fs::read_to_string(dir.path().join("b")).unwrap(), "moved\n");
}

#[test]
fn current_lenient_wrapper_empty_patch_and_environment_boundary_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    apply_patch(
        &format!("<<'EOF'\n{}\nEOF", patch("*** Add File: f\n+ok")),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        apply_patch("*** Begin Patch\n*** End Patch", dir.path()).unwrap_err(),
        "No files were modified."
    );
    assert!(
        apply_patch(
            &patch("*** Environment ID: remote\n*** Add File: g\n+bad"),
            dir.path()
        )
        .unwrap_err()
        .contains("environment selection is unavailable")
    );
    assert!(!dir.path().join("g").exists());
}

#[test]
fn workspace_path_policy_still_applies_before_any_writes() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        "../outside".to_owned(),
        dir.path()
            .parent()
            .unwrap()
            .join("outside")
            .display()
            .to_string(),
        format!("{}-other/outside", dir.path().display()),
        dir.path().join("nested/../outside").display().to_string(),
    ] {
        assert!(
            apply_patch(
                &patch(&format!(
                    "*** Add File: first\n+ok\n*** Add File: {path}\n+bad"
                )),
                dir.path()
            )
            .is_err()
        );
        assert!(!dir.path().join("first").exists());
    }
    #[cfg(unix)]
    {
        fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();
        assert!(
            apply_patch(&patch("*** Add File: link/f\n+bad"), dir.path())
                .unwrap_err()
                .contains("symlink")
        );
        assert!(!dir.path().join("real/f").exists());
    }
}

#[test]
fn absolute_paths_support_all_operations_inside_the_selected_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("source"), "old\n").unwrap();
    fs::write(root.join("delete"), "gone\n").unwrap();
    apply_patch(
        &patch(&format!(
            "*** Add File: {}/added\n+first\n*** Update File: {}/source\n*** Move to: {}/moved\n@@\n-old\n+new\n*** Delete File: {}/delete",
            root.display(), root.display(), root.display(), root.display()
        )),
        &root,
    ).unwrap();
    apply_patch(
        &patch(&format!(
            "*** Update File: {}/added\n@@\n-first\n+second",
            root.display()
        )),
        &root,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(root.join("added")).unwrap(), "second\n");
    assert_eq!(fs::read_to_string(root.join("moved")).unwrap(), "new\n");
    assert!(!root.join("source").exists());
    assert!(!root.join("delete").exists());
}

#[test]
fn absolute_aliases_keep_duplicate_and_self_move_verification_before_writes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("f"), "old\n").unwrap();
    for (source, destination) in [
        ("f".to_owned(), root.join("f").display().to_string()),
        (root.join("f").display().to_string(), "./f".to_owned()),
    ] {
        let duplicate = patch(&format!(
            "*** Add File: first\n+ok\n*** Update File: {source}\n@@\n-old\n+new\n*** Delete File: {destination}"
        ));
        assert!(
            apply_patch(&duplicate, &root)
                .unwrap_err()
                .contains("multiple operations")
        );
        let self_move = patch(&format!(
            "*** Add File: first\n+ok\n*** Update File: {source}\n*** Move to: {destination}\n@@\n-old\n+new"
        ));
        assert!(
            super::apply_patch(
                &self_move,
                &root,
                &CodexPatchConfig {
                    reject_self_move: true,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .contains("self-move rejected")
        );
        assert_eq!(fs::read_to_string(root.join("f")).unwrap(), "old\n");
        assert!(!root.join("first").exists());
    }
}

#[cfg(unix)]
#[test]
fn absolute_symlinks_and_paths_outside_nested_workdir_remain_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::create_dir(root.join("real")).unwrap();
    fs::write(root.join("real/file"), "old\n").unwrap();
    std::os::unix::fs::symlink(root.join("real"), nested.join("link")).unwrap();
    std::os::unix::fs::symlink(root.join("real/file"), nested.join("leaf")).unwrap();
    for path in [
        nested.join("link/new"),
        nested.join("leaf"),
        root.join("outside"),
    ] {
        assert!(
            apply_patch(
                &patch(&format!(
                    "*** Add File: first\n+ok\n*** Add File: {}\n+bad",
                    path.display()
                )),
                &nested
            )
            .is_err()
        );
        assert!(!nested.join("first").exists());
    }
    assert_eq!(fs::read_to_string(root.join("real/file")).unwrap(), "old\n");
    assert!(!root.join("real/new").exists());
}
