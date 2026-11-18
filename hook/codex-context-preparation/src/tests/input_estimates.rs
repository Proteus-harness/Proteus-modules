use super::*;
use proteus_contracts::model_standard::input_estimates::{
    ContextTokenEstimateSource, ModelInputEstimate,
};

fn opaque() -> CanonicalMessage {
    CanonicalMessage::new(
        MessageRole::Assistant,
        vec![ContentPart::Opaque {
            format: "fixture.legacy.v1".into(),
            data: json!({"ciphertext": "x".repeat(3172)}),
        }],
    )
}

fn estimated_input(message: CanonicalMessage, trigger: u32) -> TestInput {
    let mut input = input(vec![message], 1).with_config(json!({"trigger_tokens": trigger}));
    ContextTokenEstimateSource::Estimated.write_metadata(&mut input.request.metadata);
    input
}

#[test]
fn legacy_checkpoint_native_size_controls_real_trigger_and_new_checkpoint_report() {
    let old = opaque();
    let mut host = TestHost {
        estimate_tokens: Some(433),
        ..TestHost::with_model_response(super::remote::checkpoint("new"))
    };
    let skipped = super::remote::remote(estimated_input(old.clone(), 434), &mut host).unwrap();
    assert!(!skipped.changed);
    assert_eq!(skipped.token_estimate, Some(433));
    assert_eq!(
        skipped.messages,
        vec![old.clone()],
        "advisory sizing never rewrites durable material"
    );
    assert!(host.requests.lock().unwrap().is_empty());
    let compacted = super::remote::remote(estimated_input(old, 433), &mut host).unwrap();
    assert!(compacted.changed);
    assert_eq!(compacted.original_token_estimate, Some(433));
    assert_eq!(compacted.token_estimate, Some(433));
    let estimates = ModelInputEstimate::from_metadata(&compacted.metadata).unwrap();
    assert_eq!(
        estimates.tokens_for(&compacted.messages.last().unwrap().parts[0]),
        Some(433)
    );
    assert_eq!(host.requests.lock().unwrap().len(), 1);
    assert_eq!(
        host.estimate_requests.lock().unwrap().len(),
        3,
        "cold input, threshold input, fresh output identity"
    );
}

#[test]
fn known_zero_remains_zero_and_observed_usage_is_not_charged_again() {
    let mut host = TestHost {
        estimate_tokens: Some(0),
        ..TestHost::default()
    };
    let output = super::remote::remote(estimated_input(opaque(), 1), &mut host).unwrap();
    assert_eq!(output.token_estimate, Some(0));
    assert!(!output.changed);
    host.estimate_tokens = Some(433);
    let mut input = estimated_input(opaque(), 300);
    input.token_estimate = Some(214);
    ContextTokenEstimateSource::Observed.write_metadata(&mut input.request.metadata);
    let output = super::remote::remote(input, &mut host).unwrap();
    assert_eq!(output.token_estimate, Some(214));
    assert!(!output.changed);
    assert!(host.requests.lock().unwrap().is_empty());
}

#[test]
fn unsupported_estimator_keeps_conservative_nonzero_fallback() {
    let mut host = TestHost::default();
    let output = super::remote::remote(estimated_input(opaque(), 1000), &mut host).unwrap();
    assert!(output.token_estimate.unwrap() > 433);
    assert!(!output.changed);
}

#[test]
fn native_checkpoint_estimate_avoids_preflight_trimming_fitting_tail() {
    let checkpoint = opaque();
    let tool = CanonicalMessage::new(
        MessageRole::Tool,
        vec![ContentPart::ToolResult {
            result: ToolResult::ok("fixture".into(), "x".repeat(100)),
        }],
    );
    let mut host = TestHost {
        estimate_tokens: Some(433),
        ..TestHost::default()
    };
    let mut messages = vec![checkpoint, tool.clone()];
    let estimates = crate::input_estimates::for_messages(
        &ModelRef::new("fixture", "model"),
        &messages,
        &mut host,
    )
    .unwrap();
    crate::remote_window::trim_trailing_tool_outputs_with_estimates(
        &mut messages,
        &[],
        Some(500),
        &estimates,
    );
    assert_eq!(messages.last().unwrap(), &tool);
    crate::remote_window::trim_trailing_tool_outputs(&mut messages, &[], Some(500));
    assert_ne!(
        messages.last().unwrap(),
        &tool,
        "serialized fallback crosses this boundary"
    );
}

#[test]
fn base_instruction_hook_reuses_local_sizing_at_final_preflight() {
    use proteus_contracts::{
        contracts::{ExecutionAttribution, HookEvent, HookInput, HookResponse, ModelCallOrigin},
        model_standard::ModelOperation,
        process_module::HookModule,
    };
    let tool = CanonicalMessage::new(
        MessageRole::Tool,
        vec![ContentPart::ToolResult {
            result: ToolResult::ok("fixture".into(), "x".repeat(100)),
        }],
    );
    let mut request = request(vec![opaque(), tool.clone()]).with_operation(ModelOperation::Compact);
    request.model.model = "gpt-6.1-sol".into();
    let base =
        crate::budget::estimate_text_tokens(include_str!("../upstream/0.167.0/gpt-6.1-sol.md"))
            as u32;
    let existing = request
        .instructions
        .iter()
        .map(|block| crate::budget::estimate_text_tokens(&block.text) as u32)
        .sum::<u32>();
    request.limits.max_input_tokens = Some(base + existing + 500);
    let input = HookInput {
        event: HookEvent::BeforeModel {
            request,
            origin: ModelCallOrigin::Auxiliary,
        },
        attribution: ExecutionAttribution::detached(proteus_contracts::domain::new_execution_id()),
        cwd: "/fixture".into(),
        conversation: None,
    };
    let mut host = TestHost {
        estimate_tokens: Some(433),
        ..TestHost::default()
    };
    let response = crate::base_instructions::CodexBaseInstructions
        .invoke_json(serde_json::to_string(&input).unwrap(), &mut host)
        .unwrap();
    let HookResponse::ModelContext { messages, .. } = serde_json::from_str(&response).unwrap()
    else {
        panic!("model context")
    };
    assert_eq!(messages.last().unwrap(), &tool);
    assert_eq!(host.estimate_requests.lock().unwrap().len(), 1);
    assert!(host.requests.lock().unwrap().is_empty());
}

#[test]
fn preceding_changed_hook_invalidates_observed_floor_and_preserves_map() {
    use proteus_contracts::{
        contracts::{ExecutionAttribution, HookEvent, HookInput, HookResponse},
        model_standard::ModelCapabilities,
        process_module::HookModule,
    };
    let candidate = opaque();
    let mut material = estimated_input(candidate.clone(), 434).preparation();
    material.token_estimate = Some(9999);
    ContextTokenEstimateSource::Observed
        .write_metadata(&mut material.request.as_mut().unwrap().metadata);
    let mut previous = ContextPreparationOutput::changed(vec![candidate.clone()], None);
    previous.token_estimate = Some(9999);
    let mut capabilities = ModelCapabilities::empty();
    capabilities.supports_compaction = true;
    let input = HookInput {
        event: HookEvent::BeforeContext {
            input: material,
            output: previous,
            model_capabilities: Some(capabilities),
            target: None,
        },
        attribution: ExecutionAttribution::detached(proteus_contracts::domain::new_execution_id()),
        cwd: "/fixture".into(),
        conversation: None,
    };
    let module = crate::CodexPreparationHook(
        crate::config::CompactorConfig::parse(&json!({"trigger_tokens":434})).unwrap(),
    );
    let mut host = TestHost {
        estimate_tokens: Some(433),
        ..TestHost::default()
    };
    let response = module
        .invoke_json(serde_json::to_string(&input).unwrap(), &mut host)
        .unwrap();
    let response: HookResponse = serde_json::from_str(&response).unwrap();
    assert!(matches!(
        response,
        HookResponse::ContextPreparationDiagnostics { .. }
    ));
    let HookEvent::BeforeContext { output, .. } =
        proteus_contracts::contracts::apply_hook_response(&input.event, &response).unwrap()
    else {
        panic!("preparation")
    };
    assert!(output.changed);
    assert_eq!(output.messages, vec![candidate]);
    assert_eq!(output.token_estimate, Some(433));
    assert!(ModelInputEstimate::from_metadata(&output.metadata).is_some());
    assert!(host.requests.lock().unwrap().is_empty());
}

#[test]
fn malformed_or_foreign_estimates_fail_without_a_compaction_call() {
    struct InvalidHost(serde_json::Value);
    impl HookModuleHost for InvalidHost {
        fn is_cancelled(&self) -> Result<bool, ProcessModuleError> {
            Ok(false)
        }
        fn estimate_model_input_json(&self, _: String) -> Result<String, ProcessModuleError> {
            Ok(self.0.to_string())
        }
    }
    let checkpoint = opaque();
    let entry = json!({"part_id": checkpoint.parts[0].part_id, "tokens": 433});
    for result in [
        json!({"parts": [entry.clone(), entry]}),
        json!({"parts": [{"part_id": proteus_contracts::domain::new_part_id(), "tokens": 1}]}),
        json!({"parts": [{"part_id": checkpoint.parts[0].part_id, "tokens": -1}]}),
    ] {
        let mut host = InvalidHost(result);
        assert!(super::remote::remote(estimated_input(checkpoint.clone(), 1), &mut host).is_err());
    }
}
