//! Real standalone workers must preserve the aggregate fixture's public surface.
use std::{path::Path, time::Duration};

use proteus_contracts::{
    contracts::{ExecutionAttribution, ProcessComponentExportRef, ProcessToolInvokeInput},
    domain::{ToolCall, new_call_id, new_execution_id},
};
use proteus_module_protocol::{
    ProcessComponentBinding, ProcessExportBinding, current_process_contract_authority,
    v3::{ComponentBroker, ComponentBrokerOptions, InvocationTerminal},
};
use proteus_process_host::ProcessSpec;
use serde_json::{Value, json};

#[path = "independent_modules/source_capture.rs"]
mod source_capture;

fn connect(
    binary: &Path,
    workspace: &Path,
    exports: &[(String, String)],
) -> anyhow::Result<ComponentBroker> {
    let exports = exports.iter().map(|(slot, id)| {
        let config = match (slot.as_str(), id.as_str()) {
            ("model", _) => json!({"implementation": if id == "local_alias" { "fake" } else { id },
                                  "auth_file": workspace.join("auth.json")}),
            ("hook", "hook.instructions") => {
                json!({"text":"Verify assumptions", "placement":"append"})
            }
            ("hook", "hook.output_budget") => json!({"max_bytes":7,"head_bytes":3}),
            _ => json!({}),
        };
        ProcessExportBinding::new(
            slot,
            id,
            current_process_contract_authority(slot)
                .unwrap()
                .contract_version,
            config,
        )
        .unwrap()
    });
    ComponentBroker::connect(
        ProcessSpec::new(binary.to_string_lossy().into_owned()).cwd(workspace),
        ProcessComponentBinding::new("independent-module-check", exports).unwrap(),
        ComponentBrokerOptions::default(),
    )
}

async fn invoke(
    worker: &ComponentBroker,
    target: &ProcessComponentExportRef,
    method: &str,
    params: Value,
) -> Value {
    match worker
        .invoke(target, method, params, Duration::from_secs(15))
        .await
        .unwrap()
    {
        InvocationTerminal::Success(value) => value,
        terminal => panic!("{target:?}/{method}: {terminal:?}"),
    }
}

#[tokio::test]
async fn every_independent_package_preserves_manifests_and_tool_catalogs() {
    let index = std::env::var_os("PROTEUS_TEST_MODULE_ARTIFACTS")
        .expect("run python3 scripts/test.py full to prepare independent workers");
    let index: Value = serde_json::from_slice(&std::fs::read(index).unwrap()).unwrap();
    let packages = index["packages"].as_object().unwrap();
    assert_eq!(packages.len(), 18);
    let workspace = tempfile::tempdir().unwrap();
    let reference = Path::new(env!("CARGO_BIN_EXE_proteus-reference-module"));
    for (identity, package) in packages {
        let mut exports = vec![];
        for (slot, ids) in package["exports"].as_object().unwrap() {
            for id in ids.as_array().unwrap() {
                if id == "*" {
                    for model in [
                        "fake",
                        "openai",
                        "openai_compatible",
                        "openai_codex",
                        "anthropic",
                        "local_alias",
                    ] {
                        exports.push((slot.clone(), model.to_owned()));
                    }
                } else {
                    exports.push((slot.clone(), id.as_str().unwrap().to_owned()));
                }
            }
        }
        let worker = connect(
            Path::new(package["executable"].as_str().unwrap()),
            workspace.path(),
            &exports,
        )
        .unwrap_or_else(|error| panic!("{identity}: {error:#}"));
        let baseline = connect(reference, workspace.path(), &exports).unwrap();
        let manifest = worker
            .manifest()
            .unwrap_or_else(|error| panic!("{identity}: {error:#}"));
        assert_eq!(
            serde_json::to_value(&manifest).unwrap(),
            serde_json::to_value(baseline.manifest().unwrap()).unwrap(),
            "{identity}"
        );
        for export in &manifest.exports {
            let target = ProcessComponentExportRef {
                slot: export.slot.clone(),
                module_id: export.module_id.clone(),
            };
            if export.slot == "tool" {
                assert_eq!(
                    invoke(&worker, &target, "list", Value::Null).await,
                    invoke(&baseline, &target, "list", Value::Null).await,
                    "{identity}"
                );
            } else if export.slot == "model" {
                let params = json!({"model":{"provider":export.module_id,"model":"probe"}});
                assert_eq!(
                    invoke(&worker, &target, "describe", params.clone()).await,
                    invoke(&baseline, &target, "describe", params).await,
                    "{identity}"
                );
            }
            if export.module_id == "plan_tool" {
                let params = serde_json::to_value(ProcessToolInvokeInput {
                    call: ToolCall::new(
                        new_call_id(),
                        "update_plan",
                        json!({"plan":[{"step":"Independent delivery","status":"in_progress"}]}),
                    ),
                    cwd: workspace.path().into(),
                    attribution: ExecutionAttribution::detached(new_execution_id()),
                    skills: Default::default(),
                })
                .unwrap();
                assert_eq!(
                    invoke(&worker, &target, "invoke", params.clone()).await,
                    invoke(&baseline, &target, "invoke", params).await
                );
            }
        }
    }
}

#[test]
fn independent_packages_reject_exports_from_other_packages() {
    let index = std::env::var_os("PROTEUS_TEST_MODULE_ARTIFACTS")
        .expect("run python3 scripts/test.py full");
    let index: Value = serde_json::from_slice(&std::fs::read(index).unwrap()).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    for (identity, package) in index["packages"].as_object().unwrap() {
        let foreign = if identity == "plan-tool" {
            "file_tools"
        } else {
            "plan_tool"
        };
        let worker = connect(
            Path::new(package["executable"].as_str().unwrap()),
            workspace.path(),
            &[("tool".into(), foreign.into())],
        );
        assert!(worker.is_err(), "{identity} admitted {foreign}");
    }
}
