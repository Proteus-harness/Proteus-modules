//! Existing runner supplies real independently packaged workers; no build/install
//! from tests, no replacement stand-in process and no invented SDK revision.
use super::*;
use proteus_contracts::contracts::{
    ProcessToolInvokeResponse, ProcessToolListResponse, TOOL_HOST_PUBLISH_TERMINAL_METHOD,
    ToolTerminalDescriptor,
};
use proteus_module_protocol::{
    ProcessModuleRpcError,
    v3::{AsyncHostRequestDispatcher, ComponentHostRequest, HostRequestFuture},
};
use std::sync::{Arc, Mutex};

// The broker offers terminal publication. This test host implements exactly
// that callback and retains its validated registration, rather than claiming
// capabilities backed by NoAsyncHostRequests.
struct TerminalHost {
    target: ProcessComponentExportRef,
    registrations: Mutex<Vec<ToolTerminalDescriptor>>,
}
impl AsyncHostRequestDispatcher for TerminalHost {
    fn dispatch(&self, request: ComponentHostRequest) -> HostRequestFuture {
        let result = (|| {
            if request.method != TOOL_HOST_PUBLISH_TERMINAL_METHOD {
                return Err(ProcessModuleRpcError::new(
                    -32601,
                    "unexpected host callback",
                ));
            }
            if request.invocation.target() != &self.target {
                return Err(ProcessModuleRpcError::new(
                    -32602,
                    "unexpected terminal export",
                ));
            }
            let descriptor: ToolTerminalDescriptor = serde_json::from_value(request.params)
                .map_err(|error| ProcessModuleRpcError::new(-32602, error.to_string()))?;
            let _: proteus_contracts::domain::ExecutionId =
                serde_json::from_value(json!(descriptor.handle))
                    .map_err(|error| ProcessModuleRpcError::new(-32602, error.to_string()))?;
            if descriptor.title.len() > 512
                || !(2..=1000).contains(&descriptor.columns)
                || !(1..=500).contains(&descriptor.rows)
            {
                return Err(ProcessModuleRpcError::new(
                    -32602,
                    "invalid terminal descriptor",
                ));
            }
            let mut registrations = self.registrations.lock().unwrap();
            if registrations.len() >= 32
                || registrations
                    .iter()
                    .any(|known| known.handle == descriptor.handle)
            {
                return Err(ProcessModuleRpcError::new(
                    -32602,
                    "duplicate or excess terminal",
                ));
            }
            registrations.push(descriptor);
            Ok(Value::Null)
        })();
        Box::pin(async move { result })
    }
}

fn shell_worker(binary: &Path, workspace: &Path, id: &str, config: Value) -> ComponentBroker {
    let export = ProcessExportBinding::new(
        "tool",
        id,
        current_process_contract_authority("tool")
            .unwrap()
            .contract_version,
        config,
    )
    .unwrap();
    ComponentBroker::connect(
        // sh -lc reads HOME's profile. Use this fixture's empty home so host
        // login scripts cannot add stderr bytes before the command's raw prefix.
        ProcessSpec::new(binary.to_string_lossy().into_owned())
            .cwd(workspace)
            .env("HOME", workspace.to_string_lossy().into_owned()),
        ProcessComponentBinding::new("source-capture-check", [export]).unwrap(),
        ComponentBrokerOptions::default(),
    )
    .unwrap()
}

async fn call(
    worker: &ComponentBroker,
    target: &ProcessComponentExportRef,
    workspace: &Path,
    attribution: ExecutionAttribution,
    name: &str,
    args: Value,
) -> ProcessToolInvokeResponse {
    let input = ProcessToolInvokeInput {
        call: ToolCall::new(new_call_id(), name, args),
        cwd: workspace.into(),
        attribution,
        skills: Default::default(),
    };
    let host = Arc::new(TerminalHost {
        target: target.clone(),
        registrations: Mutex::new(Vec::new()),
    });
    let terminal = worker
        .invoke_with_dispatcher(
            target,
            "invoke",
            serde_json::to_value(input).unwrap(),
            Duration::from_secs(15),
            host.clone(),
        )
        .await
        .unwrap();
    let InvocationTerminal::Success(value) = terminal else {
        panic!("{target:?}/invoke: {terminal:?}");
    };
    let registrations = host.registrations.lock().unwrap();
    assert_eq!(registrations.len(), usize::from(name == "exec_command"));
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn optin_capture_catalog_schema_and_raw_range_work_in_real_standalone_and_aggregate() {
    let index = std::env::var_os("PROTEUS_TEST_MODULE_ARTIFACTS")
        .expect("run python3 scripts/test.py full to prepare independent workers");
    let index: Value = serde_json::from_slice(&std::fs::read(index).unwrap()).unwrap();
    let standalone = Path::new(
        index["packages"]["shell-tool"]["executable"]
            .as_str()
            .unwrap(),
    );
    let reference = Path::new(env!("CARGO_BIN_EXE_proteus-reference-module"));
    let workspace = tempfile::tempdir().unwrap();
    let config = json!({"source_capture":{"mode":"ephemeral_file","directory":workspace.path(),
        "max_capture_bytes":8,"max_captures":2,"retention_seconds":300}});
    let mut expected_catalog: Option<Value> = None;
    let mut expected_manifest: Option<Value> = None;
    for (binary, id) in [
        (standalone, "shell_tools"),
        (reference, "shell_tools"),
        (reference, "reference.tools"),
    ] {
        let target = ProcessComponentExportRef {
            slot: "tool".into(),
            module_id: id.into(),
        };
        let disabled = shell_worker(binary, workspace.path(), id, json!({}));
        let old: ProcessToolListResponse =
            serde_json::from_value(invoke(&disabled, &target, "list", Value::Null).await).unwrap();
        assert!(
            !old.result
                .iter()
                .any(|definition| definition.spec.name == "read_exec_capture")
        );
        drop(disabled);

        let worker = shell_worker(binary, workspace.path(), id, config.clone());
        let catalog = invoke(&worker, &target, "list", Value::Null).await;
        let listed: ProcessToolListResponse = serde_json::from_value(catalog.clone()).unwrap();
        let read_spec = listed
            .result
            .iter()
            .find(|definition| definition.spec.name == "read_exec_capture")
            .unwrap();
        assert_eq!(
            read_spec.spec.safety,
            proteus_contracts::domain::ToolSafety::ReadOnly
        );
        let manifest = serde_json::to_value(worker.manifest().unwrap()).unwrap();
        assert_eq!(
            manifest["exports"][0]["config_schema"]["fields"][0]["key"],
            "source_capture"
        );
        if id == "shell_tools" {
            if let Some(expected) = &expected_catalog {
                assert_eq!(&catalog, expected);
            } else {
                expected_catalog = Some(catalog);
            }
            if let Some(expected) = &expected_manifest {
                assert_eq!(&manifest, expected);
            } else {
                expected_manifest = Some(manifest);
            }
        }
        let attribution = ExecutionAttribution::detached(new_execution_id());
        let executed = call(&worker, &target, workspace.path(), attribution.clone(), "exec_command",
            json!({"cmd":"printf 0123456789abcdef; exit 7","with_escalated_permissions":true,"yield_time_ms":1000})).await;
        assert!(executed.result.ok);
        assert_eq!(executed.result.metadata["exit_code"], 7);
        assert!(executed.result.metadata["session_id"].is_null());
        let handle = executed.result.metadata["source_capture"]["handle"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(executed.result.output.contains(&handle));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let result = call(
                &worker,
                &target,
                workspace.path(),
                attribution.clone(),
                "read_exec_capture",
                json!({"handle":handle,"format":"hex"}),
            )
            .await;
            let receipt = &result.result.metadata["source_capture"];
            let visible: Value = serde_json::from_str(&result.result.text_or_status()).unwrap();
            assert_eq!(
                &visible["source_capture"], receipt,
                "model must see the same receipt"
            );
            if receipt["storage_state"] == "sealed" {
                assert_eq!(
                    visible["bytes_hex"], "3031323334353637",
                    "exec preview: {}",
                    executed.result.output
                );
                assert_eq!(receipt["prefix_truncated"], true);
                assert_eq!(receipt["complete_stream"], false);
                assert_eq!(result.result.metadata["range_eof"], true);
                break;
            }
            assert_ne!(receipt["storage_state"], "failed");
            assert!(
                tokio::time::Instant::now() < deadline,
                "capture writer did not seal"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
