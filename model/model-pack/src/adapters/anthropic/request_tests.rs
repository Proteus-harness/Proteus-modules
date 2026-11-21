use super::*;
use crate::model_standard::{InstructionBlock, InstructionKind};

#[test]
fn canonical_prefix_preserves_legacy_system_and_cache_semantics() {
    let cache = AnthropicPromptCacheConfig::from_provider_config(&json!({
        "prompt_cache_ttl": "1h"
    }));
    for (canonical, legacy, expected) in [
        ("", "", None),
        (" \n\t", "", None),
        ("", " \n\t", Some(" \n\t")),
        (" canonical ", "", Some(" canonical ")),
        (" canonical ", " legacy ", Some(" canonical \n\n legacy ")),
        (" \n\t", " legacy ", Some(" legacy ")),
    ] {
        for cache_instructions in [false, true] {
            let request = CanonicalModelRequest::new(
                ModelRef::new("anthropic", "fixture"),
                vec![
                    CanonicalMessage::text(MessageRole::System, legacy),
                    CanonicalMessage::text(MessageRole::User, "question"),
                ],
            )
            .with_instructions(vec![InstructionBlock::new(
                InstructionKind::System,
                canonical,
                100,
            )])
            .with_cache(crate::domain::CacheHints::new(cache_instructions, false));
            let source = serde_json::to_value(&request).unwrap();
            let body = to_anthropic_request_with_cache(&request, &cache).unwrap();
            let expected_system = expected.map(|text| {
                if cache_instructions {
                    json!([{
                        "type": "text",
                        "text": text,
                        "cache_control": { "type": "ephemeral", "ttl": "1h" },
                    }])
                } else {
                    json!(text)
                }
            });
            assert_eq!(body.get("system"), expected_system.as_ref());
            assert!(body.get("cache_control").is_none());
            assert_eq!(body["messages"].as_array().unwrap().len(), 1);
            assert_eq!(body["messages"][0]["content"][0]["text"], "question");
            assert_eq!(serde_json::to_value(&request).unwrap(), source);
        }
    }
}

#[test]
fn canonical_prefix_keeps_separators_for_empty_legacy_parts() {
    for (legacy, expected) in [
        (vec![""], "canonical"),
        (vec!["", ""], "canonical\n\n\n\n"),
        (
            vec!["", " legacy 🦀 ", ""],
            "canonical\n\n\n\n legacy 🦀 \n\n",
        ),
    ] {
        let mut messages = legacy
            .into_iter()
            .map(|text| CanonicalMessage::text(MessageRole::System, text))
            .collect::<Vec<_>>();
        messages.push(CanonicalMessage::text(MessageRole::User, "question"));
        let request = CanonicalModelRequest::new(ModelRef::new("anthropic", "fixture"), messages)
            .with_instructions(vec![InstructionBlock::new(
                InstructionKind::Developer,
                "canonical",
                100,
            )]);
        let body = to_anthropic_request(&request).unwrap();
        assert_eq!(body["system"], expected);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn shaped_output_limits_keep_required_anthropic_max_tokens() {
    let adapter = AnthropicMessagesClient::from_provider_config(json!({})).unwrap();
    for (requested, expected) in [(None, 64_000), (Some(1_024), 1_024)] {
        let mut request = CanonicalModelRequest::new(
            ModelRef::new("anthropic", "fixture"),
            vec![CanonicalMessage::text(MessageRole::User, "answer")],
        );
        assert_eq!(request.limits.max_output_tokens, None);
        request.limits.max_output_tokens = requested;
        let capabilities = adapter.capabilities(&request.model).unwrap();
        let shaped = crate::model_standard::RequestShaper
            .shape(request, &capabilities)
            .unwrap();
        let body = to_anthropic_request(&shaped).unwrap();
        assert_eq!(body["max_tokens"], expected);
    }
}

#[test]
fn moved_message_arrays_preserve_provider_json_bytes_and_merged_roles() {
    let mut request = CanonicalModelRequest::new(
        ModelRef::new("anthropic", "fixture"),
        vec![
            CanonicalMessage::text(MessageRole::User, " first\n"),
            CanonicalMessage::text(MessageRole::User, "second"),
            CanonicalMessage::text(MessageRole::Assistant, "reply"),
            CanonicalMessage::text(MessageRole::User, "last"),
        ],
    );
    request.limits.max_output_tokens = None;
    request.sampling.temperature = None;
    request.sampling.top_p = None;
    let expected = json!({
        "model": "fixture",
        "max_tokens": 2048,
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": " first\n"},
                {"type": "text", "text": "second"},
            ]},
            {"role": "assistant", "content": [{"type": "text", "text": "reply"}]},
            {"role": "user", "content": [{"type": "text", "text": "last"}]},
        ],
    });
    let body = to_anthropic_request(&request).unwrap();
    assert_eq!(body, expected);
    // Value equality alone does not check key order with preserve_order enabled.
    assert_eq!(body.to_string(), expected.to_string());
}

#[test]
fn request_preserves_instruction_order_and_file_content_without_mutating_source() {
    let request = CanonicalModelRequest::new(
        ModelRef::new("anthropic", "fixture"),
        vec![CanonicalMessage::new(
            MessageRole::User,
            vec![
                ContentPart::FileRef {
                    path: "src/пример.rs".into(),
                    content: Some("  fn main() {}\n\n".into()),
                },
                ContentPart::FileRef {
                    path: "missing.txt".into(),
                    content: None,
                },
            ],
        )],
    )
    .with_instructions(vec![
        InstructionBlock::new(InstructionKind::System, "low", 1),
        InstructionBlock::new(InstructionKind::Developer, " high first\n", 100),
        InstructionBlock::new(InstructionKind::System, " \n\t", 255),
        InstructionBlock::new(InstructionKind::Project, "high second", 100),
    ]);
    let source = serde_json::to_value(&request).unwrap();
    let body = to_anthropic_request(&request).unwrap();
    assert_eq!(body["system"], " high first\n\n\nhigh second\n\nlow");
    assert_eq!(
        body["messages"][0]["content"][0]["text"],
        "File reference: src/пример.rs\n  fn main() {}\n\n"
    );
    assert_eq!(
        body["messages"][0]["content"][1]["text"],
        "File reference: missing.txt\n"
    );
    assert_eq!(serde_json::to_value(&request).unwrap(), source);

    let blank = request.with_instructions(vec![InstructionBlock::new(
        InstructionKind::System,
        " \n\t",
        255,
    )]);
    assert!(
        to_anthropic_request(&blank)
            .unwrap()
            .get("system")
            .is_none()
    );
}
