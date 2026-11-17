//! Exercise production component framing and dispatch, without model/provider IO.
use super::*;
use proteus_contracts::{
    contracts::{
        MODEL_INPUT_ESTIMATE_FEATURE, ModelInputEstimate, ModelInputEstimateRequest,
        ModelPartTokenEstimate, PROCESS_MODEL_CONTRACT_VERSION, PROCESS_MODEL_ESTIMATE_METHOD,
        ProcessComponentInitialize, ProcessModelDescriptor, ProcessModelInput, ProcessModelOutput,
    },
    domain::ModelRef,
    model_standard::{CanonicalPart, ContentPart, PartProvenance, PartScope},
    process_module::{ModelModule, ModelModuleHost, ProcessModuleResult},
};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

struct Probe {
    supported: bool,
    invalid: bool,
    calls: Arc<AtomicUsize>,
}
impl ModelModule for Probe {
    fn catalog(
        &self,
        _: &dyn ModelModuleHost,
    ) -> ProcessModuleResult<Option<proteus_contracts::contracts::ModelCatalog>> {
        panic!("estimate must not query catalog")
    }
    fn quota(
        &self,
        _: &dyn ModelModuleHost,
    ) -> ProcessModuleResult<Option<proteus_contracts::contracts::ModelQuotaSnapshot>> {
        panic!("estimate must not query quota")
    }
    fn describe(&self, _: ModelRef) -> ProcessModuleResult<ProcessModelDescriptor> {
        panic!("estimate must not describe or generate")
    }
    fn supports_input_estimate(&self) -> bool {
        self.supported
    }
    fn estimate_input_json(&self, input: String) -> ProcessModuleResult<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let input: ModelInputEstimateRequest = serde_json::from_str(&input).unwrap();
        let mut parts = vec![ModelPartTokenEstimate {
            part_id: input.parts[0].part_id,
            tokens: 0,
        }];
        if self.invalid {
            parts.push(parts[0].clone());
        }
        Ok(serde_json::to_string(&Some(ModelInputEstimate { parts })).unwrap())
    }
    fn stream(
        &self,
        _: ProcessModelInput,
        _: &dyn ModelModuleHost,
    ) -> ProcessModuleResult<ProcessModelOutput> {
        panic!("estimate must not stream")
    }
}

fn roundtrip(supported: bool, invalid: bool) -> (Value, Value, usize) {
    let (host, worker) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut writer = host.try_clone().unwrap();
    let mut reader = BufReader::new(host);
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = calls.clone();
    let server = thread::spawn(move || {
        let out = worker.try_clone().unwrap();
        proteus_module_sdk::serve(BufReader::new(worker), out, move |binding| {
            let mut modules = CollectedModules::default();
            modules.models.insert(
                binding.module_id.clone(),
                Box::new(Probe {
                    supported,
                    invalid,
                    calls: worker_calls.clone(),
                }),
            );
            Ok(Box::new(ModuleExport::new(binding, modules, None)))
        })
    });
    fn exchange(
        writer: &mut UnixStream,
        reader: &mut BufReader<UnixStream>,
        frame: Value,
    ) -> Value {
        writeln!(writer, "{frame}").unwrap();
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        serde_json::from_str(&line).unwrap()
    }
    let binding = ProcessComponentExportInitialize::new(
        "model",
        "estimate",
        PROCESS_MODEL_CONTRACT_VERSION,
        proteus_contracts::contracts::ProcessModuleComposition::SelectOne,
        json!({}),
        vec![MODEL_INPUT_ESTIMATE_FEATURE.to_owned()],
    );
    let initialized = exchange(
        &mut writer,
        &mut reader,
        json!({
            "jsonrpc":"2.0","id":"h:1:0","method":"initialize",
            "params":ProcessComponentInitialize::new("estimate-wire",[binding]),
        }),
    );
    let request = ModelInputEstimateRequest {
        model: ModelRef::new("estimate", "fixture"),
        parts: vec![CanonicalPart::new(
            PartProvenance::Model,
            PartScope::Conversation,
            ContentPart::Opaque {
                format: "provider/opaque".into(),
                data: json!({"unchanged":true}),
            },
        )],
    };
    let response = exchange(
        &mut writer,
        &mut reader,
        json!({
            "jsonrpc":"2.0","id":"h:1:1","method":PROCESS_MODEL_ESTIMATE_METHOD,
            "params":{"export":{"slot":"model","module_id":"estimate"},
                "lineage":{"root_invocation_id":"h:1:1","parent_invocation_id":null,"depth":0},
                "params":request},
        }),
    );
    writer.shutdown(std::net::Shutdown::Both).unwrap();
    drop(reader);
    drop(writer);
    server.join().unwrap().unwrap();
    (initialized, response, calls.load(Ordering::SeqCst))
}

#[test]
fn negotiated_local_estimate_crosses_actual_sdk_framing_and_keeps_zero() {
    let (initialized, response, calls) = roundtrip(true, false);
    assert_eq!(
        initialized["result"]["exports"][0]["module_features"],
        json!([MODEL_INPUT_ESTIMATE_FEATURE])
    );
    assert_eq!(response["result"]["parts"][0]["tokens"], 0);
    assert_eq!(calls, 1);
}

#[test]
fn unsupported_estimate_is_not_advertised_or_dispatched() {
    let (initialized, response, calls) = roundtrip(false, false);
    assert_eq!(
        initialized["result"]["exports"][0]["module_features"],
        json!([])
    );
    assert!(response.get("error").is_some(), "{response}");
    assert_eq!(calls, 0);
}

#[test]
fn invalid_provider_map_cannot_cross_the_dispatch_boundary() {
    let (_, response, calls) = roundtrip(true, true);
    assert!(response.get("error").is_some(), "{response}");
    assert_eq!(calls, 1);
}
