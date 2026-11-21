//! Independent delivery preserves each declared contract; behavior modules also
//! match the aggregate fixture's public surface.
use std::{path::Path, time::Duration};

use proteus_contracts::{
    contracts::{
        ExecutionAttribution, ProcessComponentExportRef, ProcessModuleComposition,
        ProcessToolInvokeInput, session_storage::*,
    },
    domain::{
        ToolCall, new_call_id, new_execution_id, new_record_id, new_session_id, new_thread_id,
    },
    model_standard::{CanonicalMessage, MessageRole},
    storage::journal::{HistoryMutated, HistoryMutationKind, JournalEntry},
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
    let catalog: Value =
        serde_json::from_str(include_str!("../../../proteus-catalog.json")).unwrap();
    let expected: std::collections::BTreeSet<_> = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    let actual = packages.keys().map(String::as_str).collect();
    assert_eq!(
        expected, actual,
        "every catalog package needs a built process fixture"
    );
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
        let manifest = worker
            .manifest()
            .unwrap_or_else(|error| panic!("{identity}: {error:#}"));
        if identity == "session-storage" {
            let declared = manifest
                .exports
                .iter()
                .map(|export| (export.slot.as_str(), export.module_id.as_str()))
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                declared,
                std::collections::BTreeSet::from([
                    ("session_storage", "jsonl"),
                    ("session_storage", "sqlite")
                ])
            );
            for export in &manifest.exports {
                assert_eq!(
                    export.contract_version,
                    PROCESS_SESSION_STORAGE_CONTRACT_VERSION
                );
                assert_eq!(export.composition, ProcessModuleComposition::SelectOne);
                assert!(
                    export.module_features.is_empty(),
                    "storage has no execution callbacks"
                );
                let authority = current_process_contract_authority(&export.slot).unwrap();
                assert!(authority.host_methods.is_empty());
                check_storage_boundary(
                    &worker,
                    &ProcessComponentExportRef {
                        slot: export.slot.clone(),
                        module_id: export.module_id.clone(),
                    },
                    workspace.path(),
                )
                .await;
            }
            assert!(
                connect(reference, workspace.path(), &exports).is_err(),
                "aggregate intentionally contains no storage implementation"
            );
            continue;
        }
        let baseline = connect(reference, workspace.path(), &exports).unwrap();
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

async fn storage_request(
    worker: &ComponentBroker,
    target: &ProcessComponentExportRef,
    request: SessionStorageRequest,
) -> SessionStorageResponse {
    request.validate().unwrap();
    let result = invoke(
        worker,
        target,
        PROCESS_SESSION_STORAGE_EXECUTE_METHOD,
        serde_json::to_value(&request).unwrap(),
    )
    .await;
    let response: SessionStorageResponse = serde_json::from_value(result).unwrap();
    request.validate_response(&response).unwrap();
    response
}

async fn check_storage_boundary(
    worker: &ComponentBroker,
    target: &ProcessComponentExportRef,
    workspace: &Path,
) {
    let root = tempfile::tempdir().unwrap();
    let SessionStorageResponse::Session { descriptor } = storage_request(
        worker,
        target,
        SessionStorageRequest::Create {
            config_root: root.path().into(),
            workspace_path: workspace.into(),
            session_id: new_session_id(),
            parent_session_id: None,
        },
    )
    .await
    else {
        panic!()
    };
    assert!(
        !descriptor.local_path.as_ref().unwrap().exists(),
        "read-only reservation must not create a session folder"
    );
    let SessionStorageResponse::Writer { lease, revision } = storage_request(
        worker,
        target,
        SessionStorageRequest::AcquireWriter {
            session: descriptor.session.clone(),
        },
    )
    .await
    else {
        panic!()
    };
    assert_eq!(revision, StorageRevision::default());
    let message = CanonicalMessage::text(MessageRole::User, "independent storage delivery");
    storage_request(
        worker,
        target,
        SessionStorageRequest::Commit {
            commit: StorageCommit {
                lease: lease.clone(),
                operation_id: new_record_id(),
                expected: revision,
                timestamp_ms: 1,
                attribution: StorageRecordAttribution {
                    execution_id: None,
                    thread_id: Some(new_thread_id()),
                    turn_id: None,
                },
                entry: JournalEntry::HistoryMutated(HistoryMutated {
                    previous_revision: 0,
                    new_revision: 1,
                    mutation: HistoryMutationKind::Append,
                    messages: vec![message.clone()],
                    compaction: None,
                    tool_results: vec![],
                }),
            },
        },
    )
    .await;
    storage_request(
        worker,
        target,
        SessionStorageRequest::ReleaseWriter { lease },
    )
    .await;
    let SessionStorageResponse::Snapshot { snapshot } = storage_request(
        worker,
        target,
        SessionStorageRequest::OpenSnapshot {
            session: descriptor.session,
        },
    )
    .await
    else {
        panic!()
    };
    let SessionStorageResponse::Conversation { conversation } = storage_request(
        worker,
        target,
        SessionStorageRequest::ReadConversation {
            snapshot: snapshot.clone(),
            point: StorageHistoryPoint::Current,
        },
    )
    .await
    else {
        panic!()
    };
    assert_eq!(conversation.messages, [message]);
    assert_eq!(
        conversation.revision,
        StorageRevision {
            session_seq: 1,
            history_revision: 1
        }
    );
    storage_request(
        worker,
        target,
        SessionStorageRequest::ReleaseSnapshot { snapshot },
    )
    .await;
}
