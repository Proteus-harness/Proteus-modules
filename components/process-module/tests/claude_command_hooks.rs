//! Exercise the foreign command adapter through the real Rust host and worker.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use proteus_contracts::{
    contracts::{
        ApprovalRequest, ApprovalResponse, ApprovalTransport, CancellationToken, HookEvent,
    },
    domain::{ToolCall, new_call_id},
};
use proteus_core::core::{
    AgentRuntime, AppConfig, JournalEntry, ModuleCatalog, SessionStore, TurnSettlementStatus,
};
use serde_json::json;

fn harness(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("PROTEUS_TEST_HARNESS_ROOT").expect("use scripts/test.py"))
        .join(path)
}

fn modules(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

struct Approval {
    approved: bool,
    calls: AtomicUsize,
}
#[async_trait]
impl ApprovalTransport for Approval {
    fn can_request_approval(&self) -> bool {
        true
    }
    async fn request_approval(&self, _: ApprovalRequest) -> anyhow::Result<ApprovalResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if self.approved {
            ApprovalResponse::approve()
        } else {
            ApprovalResponse::deny("fixture refused")
        })
    }
}

fn attach(config: &mut AppConfig, workspace: &Path, event: &str, mode: &str, value: Option<&str>) {
    let foreign = workspace.join("foreign-hooks.json");
    let mut args = vec![
        json!(modules(
            "hook/claude-command-hooks/tests/fixtures/command.mjs"
        )),
        json!(mode),
    ];
    args.extend(value.map(|value| json!(value)));
    std::fs::write(
        &foreign,
        serde_json::to_vec(&json!({"hooks":{(event):[{
            "matcher":"Write", "hooks":[{"type":"command","command":"node","args":args}]
        }]}}))
        .unwrap(),
    )
    .unwrap();
    config.modules.hooks = vec!["claude-command-hooks".into()];
    config.components.insert(
        "command-hooks".into(),
        serde_json::from_value(json!({
            "command":"node", "args":[harness("examples/modules/hook-process/worker.mjs")],
            "exports":{"hook":{"claude-command-hooks":{"timeout_ms":10000}}}
        }))
        .unwrap(),
    );
    config.module_config.entry("hook".into()).or_default().insert("claude-command-hooks".into(), json!({
        "entry":modules("hook/claude-command-hooks/src/entry.mjs"),
        "settings":{"mode":"strict_subset","allow_execution":true,"config_path":foreign,
            "sdk_root":harness("examples/modules/hook-process"),"events":[event],
            "tool_mappings":{"write_file":{"name":"Write","arguments":{"path":"file_path"}}}}
    }));
}

fn tool_config() -> AppConfig {
    let mut config = AppConfig::default();
    config.active_provider = None;
    config.providers.clear();
    config.modules.policy = Some("ask_write".into());
    config.tools.enabled = vec!["write_file".into()];
    config.components.insert(
        "reference".into(),
        serde_json::from_value(json!({
            "command":env!("CARGO_BIN_EXE_proteus-reference-module"),
            "exports":{"tool":{"reference.tools":{}},"policy":{"ask_write":{}}}
        }))
        .unwrap(),
    );
    config
        .module_config
        .entry("policy".into())
        .or_default()
        .insert("ask_write".into(), json!({"ask_before":["write_file"]}));
    config
}

async fn runtime(config: AppConfig, workspace: &Path, approval: Arc<Approval>) -> AgentRuntime {
    let catalog = ModuleCatalog::from_config(&config).unwrap();
    AgentRuntime::builder(config, workspace.into())
        .with_module_catalog(catalog)
        .with_config_path(Some(&workspace.join("config.toml")))
        .with_approval(approval)
        .build_async()
        .await
        .unwrap()
}

fn write_call() -> ToolCall {
    ToolCall::new(
        new_call_id(),
        "write_file",
        json!({"path":"original.txt","content":"payload"}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_veto_and_failure_prevent_the_real_file_effect() {
    for (mode, value) in [
        ("deny", Some("fixture denied")),
        ("error", None),
        ("raw", Some("{\"continue\":false}")),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let mut config = tool_config();
        attach(&mut config, workspace.path(), "PreToolUse", mode, value);
        let approval = Arc::new(Approval {
            approved: true,
            calls: AtomicUsize::new(0),
        });
        let runtime = runtime(config, workspace.path(), approval.clone()).await;
        let result = runtime
            .execute_tool(write_call(), CancellationToken::new())
            .await;
        if mode == "deny" {
            assert!(!result.unwrap().ok);
        } else {
            assert!(result.is_err());
        }
        assert!(!workspace.path().join("original.txt").exists());
        assert_eq!(approval.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn argument_mapping_is_revalidated_and_allow_still_requires_host_approval() {
    for approved in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let mut config = tool_config();
        attach(
            &mut config,
            workspace.path(),
            "PreToolUse",
            "rewrite",
            Some("{\"file_path\":\"rewritten.txt\",\"content\":\"mapped payload\"}"),
        );
        let approval = Arc::new(Approval {
            approved,
            calls: AtomicUsize::new(0),
        });
        let runtime = runtime(config, workspace.path(), approval.clone()).await;
        let result = runtime
            .execute_tool(write_call(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.ok, approved);
        assert_eq!(approval.calls.load(Ordering::SeqCst), 1);
        assert!(!workspace.path().join("original.txt").exists());
        let target = workspace.path().join("rewritten.txt");
        if approved {
            assert_eq!(std::fs::read_to_string(target).unwrap(), "mapped payload");
        } else {
            assert!(!target.exists());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_continues_the_same_turn_then_persists_its_success() {
    let workspace = tempfile::tempdir().unwrap();
    let mut config = AppConfig::load(Some(&harness(
        "examples/configs/proteus.project-check.example.toml",
    )))
    .await
    .unwrap();
    config.components.clear();
    config.components.insert(
        "controller".into(),
        serde_json::from_value(json!({
            "command":env!("CARGO_BIN_EXE_proteus-reference-module"),
            "exports":{"workflow":{"coding.project_check":{}},"policy":{"ask_write":{}}}
        }))
        .unwrap(),
    );
    config.components.insert("project-check-tools".into(), serde_json::from_value(json!({
        "command":"python3", "args":[Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_check_tools.py")],
        "env":{"PYTHONDONTWRITEBYTECODE":"1","PYTHONPATH":harness("examples/modules")},
        "exports":{"tool":{"project-check-fixture-tools":{}}}
    })).unwrap());
    attach(&mut config, workspace.path(), "Stop", "stop-once", None);
    let runtime = runtime(
        config,
        workspace.path(),
        Arc::new(Approval {
            approved: true,
            calls: AtomicUsize::new(0),
        }),
    )
    .await;
    runtime.run("check the project".into()).await.unwrap();
    let projection = SessionStore::open(runtime.session_dir().unwrap().to_path_buf())
        .unwrap()
        .load_projection()
        .unwrap();
    let attempts = projection
        .records
        .iter()
        .filter_map(|record| match &record.entry {
            JournalEntry::HookInvoked(trace) => match trace.input.event {
                HookEvent::BeforeStop { attempt, .. } => Some(attempt),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(attempts, [0, 1]);
    assert_eq!(
        projection
            .records
            .iter()
            .filter(|record| matches!(record.entry, JournalEntry::TurnOpened(_)))
            .count(),
        1
    );
    assert!(
        projection
            .records
            .iter()
            .any(|record| matches!(&record.entry,
        JournalEntry::TurnSettled(settlement) if settlement.status==TurnSettlementStatus::Success))
    );
    assert!(projection.unsettled_turns.is_empty());
}
