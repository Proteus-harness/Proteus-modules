use super::*;
use proteus_contracts::{
    contracts::ExecutionAttribution, domain::new_execution_id, process_module::ToolModuleHost,
};

const _: () = assert!(TIMEOUT_MS >= 60_000);

fn invoke(tool_name: &str, cwd: &Path, args: Value) -> Value {
    let tool: &dyn ToolModule = if tool_name == "git_status" {
        &GitStatusTool
    } else {
        &GitDiffTool
    };
    let call = json!({"id": "call_test", "name": tool_name, "args": args});
    let context = ToolModuleInvocationContext {
        cwd: cwd.to_path_buf(),
        skills: Default::default(),
        attribution: ExecutionAttribution::detached(new_execution_id()),
        config: json!({}),
    };
    struct Host;
    impl ToolModuleHost for Host {
        fn is_cancelled(&self) -> Result<bool, ProcessModuleError> {
            Ok(false)
        }
    }
    let result = tool
        .invoke_json(
            call.to_string(),
            serde_json::to_string(&context).unwrap(),
            &mut Host,
        )
        .expect("tool result json");
    serde_json::from_str(&result).expect("result json")
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn git_status_reports_modified_file() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().expect("workspace");
    git(dir.path(), &["init"]);
    std::fs::write(dir.path().join("notes.txt"), "one\n").expect("write file");

    let result = invoke("git_status", dir.path(), json!({}));

    assert_eq!(result["ok"], true);
    assert!(result["output"].as_str().unwrap().contains("notes.txt"));
}

#[test]
fn git_diff_supports_path_filter() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().expect("workspace");
    git(dir.path(), &["init"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("a.txt"), "one\n").expect("write a");
    std::fs::write(dir.path().join("b.txt"), "one\n").expect("write b");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "initial"]);
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("modify a");
    std::fs::write(dir.path().join("b.txt"), "one\nthree\n").expect("modify b");

    let result = invoke("git_diff", dir.path(), json!({ "path": "a.txt" }));

    assert_eq!(result["ok"], true);
    let output = result["output"].as_str().unwrap();
    assert!(output.contains("a.txt"), "{output}");
    assert!(!output.contains("b.txt"), "{output}");

    let zero = invoke(
        "git_diff",
        dir.path(),
        json!({"path":"a.txt", "context_lines":0}),
    );
    assert_eq!(zero["ok"], true);
    let zero_output = zero["output"].as_str().unwrap();
    assert!(zero_output.contains("+two"), "{zero_output}");
    assert!(
        !zero_output.lines().any(|line| line == " one"),
        "{zero_output}"
    );
    assert!(
        output.lines().any(|line| line == " one"),
        "default context lost: {output}"
    );
    let call = ToolCallDto {
        id: "zero-bytes".into(),
        name: "git_diff".into(),
        args: json!({"max_bytes":0}),
    };
    assert!(
        invoke_impl(&call, dir.path(), GitCommand::Diff)
            .unwrap_err()
            .contains("greater than zero")
    );
}

#[test]
fn pathspec_rejects_parent_escape() {
    let error = validate_relative_pathspec("../outside.txt").expect_err("reject parent");
    assert!(error.contains("parent traversal"), "{error}");
}

#[cfg(unix)]
#[test]
fn readonly_git_invocations_refuse_matching_clean_and_process_filters() {
    for kind in ["clean", "process"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("tracked.txt"), "before\n").unwrap();
        git(root, &["add", "tracked.txt"]);
        std::fs::write(root.join("tracked.txt"), "different contents\n").unwrap();
        let marker = root.join("filter-ran");
        let script = root.join("filter.sh");
        std::fs::write(
            &script,
            format!("printf called > '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        git(
            root,
            &[
                "config",
                &format!("filter.probe.{kind}"),
                &format!("sh '{}'", script.display()),
            ],
        );
        git(root, &["config", "filter.probe.required", "true"]);
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();

        // An unrelated configured executable driver must not disable ordinary reads.
        std::fs::write(root.join(".gitattributes"), "tracked.txt filter=unknown\n").unwrap();
        for name in ["git_status", "git_diff"] {
            let result = invoke(name, root, json!({"path": "tracked.txt"}));
            assert_eq!(result["ok"], true, "{name}: {result}");
        }
        std::fs::write(root.join(".gitattributes"), "tracked.txt filter=probe\n").unwrap();
        for cwd in [root, nested.as_path()] {
            for name in ["git_status", "git_diff"] {
                // Unscoped tools can inspect repository content outside a nested cwd.
                let result = invoke(name, cwd, json!({}));
                assert_eq!(
                    result["ok"],
                    false,
                    "{kind}/{name}/{}: {result}",
                    cwd.display()
                );
                assert!(
                    result
                        .to_string()
                        .contains("read-only Git refuses executable filter 'probe'"),
                    "{result}"
                );
                assert!(!marker.exists(), "{kind}/{name} executed the filter");
            }
        }
        // Prove that the exact fixture reaches the driver without our consumer's guard.
        let _ = Command::new("git")
            .current_dir(root)
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                "tracked.txt",
            ])
            .output()
            .unwrap();
        assert!(
            marker.exists(),
            "{kind} fixture never exercised its executable driver"
        );
    }
}
