//! Optional diagnostics cross the process boundary only after host negotiation.
use super::*;
use proteus_contracts::{
    contracts::{
        ContextPreparationInput, ContextPreparationOutput, ExecutionAttribution,
        HOOK_CONTEXT_PREPARATION_DIAGNOSTICS_FEATURE, HookEvent, HookInput, HookResponse,
        PROCESS_HOOK_CONTRACT_VERSION, PROCESS_HOOK_INVOKE_METHOD, ProcessComponentInitialize,
        ProcessModuleComposition,
    },
    model_standard::{CanonicalMessage, MessageRole},
    process_module::{HookModule, HookModuleHost, ProcessModuleResult},
};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    thread,
    time::Duration,
};

struct Probe;
impl HookModule for Probe {
    fn invoke_json(
        &self,
        input: String,
        _: &mut dyn HookModuleHost,
    ) -> ProcessModuleResult<String> {
        let input: HookInput = serde_json::from_str(&input).unwrap();
        let HookEvent::BeforeContext { mut output, .. } = input.event else {
            panic!()
        };
        output.token_estimate = Some(42);
        Ok(
            serde_json::to_string(&HookResponse::ContextPreparationDiagnostics {
                diagnostics: output.diagnostics(),
            })
            .unwrap(),
        )
    }
}

#[test]
fn diagnostics_are_negotiated_and_old_hosts_receive_full_output() {
    for negotiated in [false, true] {
        let (host, worker) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut writer = host.try_clone().unwrap();
        let mut reader = BufReader::new(host);
        let server = thread::spawn(move || {
            let out = worker.try_clone().unwrap();
            proteus_module_sdk::serve(BufReader::new(worker), out, move |binding| {
                let mut modules = CollectedModules::default();
                modules
                    .hooks
                    .insert(binding.module_id.clone(), Box::new(Probe));
                Ok(Box::new(ModuleExport::new(binding, modules, None)))
            })
        });
        let binding = ProcessComponentExportInitialize::new(
            "hook",
            "diagnostics",
            PROCESS_HOOK_CONTRACT_VERSION,
            ProcessModuleComposition::OrderedMany,
            json!({}),
            if negotiated {
                vec![HOOK_CONTEXT_PREPARATION_DIAGNOSTICS_FEATURE.to_owned()]
            } else {
                vec![]
            },
        );
        let mut exchange = |frame: Value| {
            writeln!(writer, "{frame}").unwrap();
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            serde_json::from_str::<Value>(&line).unwrap()
        };
        let initialized = exchange(json!({
            "jsonrpc":"2.0","id":"h:1:0","method":"initialize",
            "params":ProcessComponentInitialize::new("diagnostics-wire", [binding]),
        }));
        let features = if negotiated {
            json!([HOOK_CONTEXT_PREPARATION_DIAGNOSTICS_FEATURE])
        } else {
            json!([])
        };
        assert_eq!(
            initialized["result"]["exports"][0]["module_features"],
            features
        );
        let messages = vec![CanonicalMessage::text(MessageRole::User, "history payload")];
        let input = HookInput {
            event: HookEvent::BeforeContext {
                input: ContextPreparationInput::new(messages.clone(), None),
                output: ContextPreparationOutput::unchanged(messages.clone()),
                model_capabilities: None,
                target: None,
            },
            attribution: ExecutionAttribution::detached(
                proteus_contracts::domain::new_execution_id(),
            ),
            cwd: "/fixture".into(),
            conversation: None,
        };
        let response = exchange(json!({
            "jsonrpc":"2.0","id":"h:1:1","method":PROCESS_HOOK_INVOKE_METHOD,
            "params":{"export":{"slot":"hook","module_id":"diagnostics"},
                "lineage":{"root_invocation_id":"h:1:1","parent_invocation_id":null,"depth":0},
                "params":input},
        }));
        let response: HookResponse =
            serde_json::from_value(response["result"]["result"].clone()).unwrap();
        assert_eq!(
            matches!(response, HookResponse::ContextPreparationDiagnostics { .. }),
            negotiated
        );
        let HookEvent::BeforeContext { output, .. } =
            proteus_contracts::contracts::apply_hook_response(&input.event, &response).unwrap()
        else {
            panic!()
        };
        assert_eq!(output.messages, messages);
        assert_eq!(output.token_estimate, Some(42));
        writer.shutdown(std::net::Shutdown::Both).unwrap();
        drop(reader);
        drop(writer);
        server.join().unwrap().unwrap();
    }
}
