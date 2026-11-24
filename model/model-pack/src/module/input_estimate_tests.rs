use super::*;
use proteus_contracts::{
    contracts::ModelInputEstimateRequest,
    model_standard::{CanonicalPart, ContentPart, PartProvenance, PartScope},
};
use serde_json::json;

fn provider(implementation: &str, config: Value) -> ProviderModule {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let client = {
        let _guard = runtime.enter();
        build(implementation, &config, true).unwrap()
    };
    ProviderModule {
        go: None,
        runtime,
        streaming: client.clone(),
        complete: client,
    }
}

fn request() -> ModelInputEstimateRequest {
    ModelInputEstimateRequest {
        model: ModelRef::new("native", "fixture-model"),
        parts: vec![CanonicalPart::new(
            PartProvenance::Compactor,
            PartScope::Conversation,
            ContentPart::Opaque {
                format: "openai.responses.compaction.v1".into(),
                data: json!({"type":"compaction","encrypted_content":"x".repeat(3172)}),
            },
        )],
    }
}

#[test]
fn provider_module_dispatches_local_estimate_without_authentication_or_model_generation() {
    for (implementation, config) in [
        ("openai", json!({})),
        (
            "openai_codex",
            json!({"auth_file":"estimate-module-does-not-read-auth.json"}),
        ),
        (
            "openai_compatible",
            json!({"base_url":"http://127.0.0.1:1/v1",
            "capabilities":{"supports_compaction":true}}),
        ),
    ] {
        let provider = provider(implementation, config);
        assert!(provider.supports_input_estimate());
        let input = request();
        let encoded = serde_json::to_string(&input).unwrap();
        let response = provider.estimate_input_json(encoded.clone()).unwrap();
        let response: Option<proteus_contracts::contracts::ModelInputEstimate> =
            serde_json::from_str(&response).unwrap();
        let response = response.unwrap();
        response.validate(&input).unwrap();
        assert_eq!(response.parts[0].part_id, input.parts[0].part_id);
        assert_eq!(response.parts[0].tokens, 433);
        assert_eq!(serde_json::to_string(&input).unwrap(), encoded);
    }
}

#[test]
fn provider_module_rejects_invalid_requests_and_does_not_claim_unknown_format_zero() {
    let provider = provider("openai", json!({}));
    let mut input = request();
    input.parts.push(input.parts[0].clone());
    assert!(
        provider
            .estimate_input_json(serde_json::to_string(&input).unwrap())
            .is_err()
    );

    let mut input = request();
    let ContentPart::Opaque { format, .. } = &mut input.parts[0].payload else {
        unreachable!()
    };
    *format = "unsupported/opaque".into();
    let response = provider
        .estimate_input_json(serde_json::to_string(&input).unwrap())
        .unwrap();
    let response: Option<proteus_contracts::contracts::ModelInputEstimate> =
        serde_json::from_str(&response).unwrap();
    assert!(response.unwrap().parts.is_empty());

    let mut invalid = serde_json::to_value(request()).unwrap();
    invalid["unexpected"] = json!(true);
    assert!(provider.estimate_input_json(invalid.to_string()).is_err());
}

#[test]
fn unsupported_provider_module_advertises_handler_but_returns_no_estimate_without_transport() {
    for (implementation, config) in [
        ("fake", json!({})),
        (
            "anthropic",
            json!({"base_url":"http://127.0.0.1:1/v1", "api_key_env":"PROTEUS_INPUT_ESTIMATE_UNUSED_KEY"}),
        ),
        (
            "openai_compatible",
            json!({"base_url":"http://127.0.0.1:1/v1"}),
        ),
    ] {
        let provider = provider(implementation, config);
        assert!(provider.supports_input_estimate());
        assert_eq!(
            provider
                .estimate_input_json(serde_json::to_string(&request()).unwrap())
                .unwrap(),
            "null"
        );
    }
}
