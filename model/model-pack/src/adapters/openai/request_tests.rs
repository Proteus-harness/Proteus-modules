use super::*;
use crate::model_standard::{InstructionBlock, InstructionKind};

#[test]
fn request_rejects_opaque_parts_instead_of_silently_dropping_them() {
    for format in [
        "anthropic.messages.redacted_thinking",
        "other.provider.block",
    ] {
        let request = CanonicalModelRequest::new(
            ModelRef::new("openai", "fixture"),
            vec![CanonicalMessage::new(
                MessageRole::Assistant,
                vec![ContentPart::Opaque {
                    format: format.to_owned(),
                    data: json!({"type": "redacted_thinking", "data": "opaque+/="}),
                }],
            )],
        );
        let error = to_openai_request(&request).unwrap_err().to_string();
        assert!(
            error.contains("openai.responses does not support opaque format"),
            "{error}"
        );
        assert!(error.contains(format), "{error}");
    }
}

#[test]
fn moved_input_array_preserves_provider_json_bytes_and_order() {
    let mut request = CanonicalModelRequest::new(
        ModelRef::new("openai", "fixture"),
        vec![
            CanonicalMessage::new(
                MessageRole::User,
                vec![
                    ContentPart::Text {
                        text: " first\n".into(),
                    },
                    ContentPart::Text {
                        text: "second".into(),
                    },
                ],
            ),
            CanonicalMessage::text(MessageRole::Assistant, "reply"),
        ],
    );
    request.tool_choice = ToolChoice::None;
    request.limits.max_output_tokens = None;
    let expected = json!({
        "model": "fixture",
        "input": [
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": " first\n"},
                {"type": "input_text", "text": "second"},
            ]},
            {"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "reply"},
            ]},
        ],
        "tool_choice": "none",
        "parallel_tool_calls": true,
        "store": false,
        "temperature": 0.0,
        "include": [],
    });
    let body = to_openai_request(&request).unwrap();
    assert_eq!(body, expected);
    // Value equality alone does not check key order with preserve_order enabled.
    assert_eq!(body.to_string(), expected.to_string());
}

#[test]
fn client_metadata_keeps_sorted_keys_and_request_overrides_profile() {
    let profile = OpenAiModelProfile::from_provider_config(&json!({
        "client_metadata": {"z_profile":"profile", "a_shared":"original"}
    }))
    .unwrap();
    let mut request = CanonicalModelRequest::new(ModelRef::new("openai", "fixture"), vec![]);

    let rendered = |request: &CanonicalModelRequest, profile: &OpenAiModelProfile| {
        crate::adapters::openai::request::to_openai_request_with_cache(
            request,
            &OpenAiPromptCacheConfig::default(),
            profile,
        )
        .unwrap()
    };
    let empty_profile = OpenAiModelProfile::from_provider_config(&json!({})).unwrap();
    assert!(
        rendered(&request, &empty_profile)
            .get("client_metadata")
            .is_none()
    );
    assert_eq!(
        rendered(&request, &profile)["client_metadata"].to_string(),
        r#"{"a_shared":"original","z_profile":"profile"}"#
    );

    request
        .client_metadata
        .insert("c_request".into(), "запрос 🦀".into());
    request
        .client_metadata
        .insert("a_shared".into(), String::new());
    assert_eq!(
        rendered(&request, &empty_profile)["client_metadata"].to_string(),
        r#"{"a_shared":"","c_request":"запрос 🦀"}"#
    );
    assert_eq!(
        rendered(&request, &profile)["client_metadata"].to_string(),
        r#"{"a_shared":"","c_request":"запрос 🦀","z_profile":"profile"}"#
    );
    assert_eq!(profile.client_metadata["a_shared"], "original");
}

#[test]
fn request_preserves_instruction_order_and_file_content_without_mutating_source() {
    let request = CanonicalModelRequest::new(
        ModelRef::new("openai", "fixture"),
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
    let body = to_openai_request(&request).unwrap();
    assert_eq!(body["instructions"], " high first\n\n\nhigh second\n\nlow");
    assert_eq!(
        body["input"][0]["content"][0]["text"],
        "File reference: src/пример.rs\n  fn main() {}\n\n"
    );
    assert_eq!(
        body["input"][1]["content"][0]["text"],
        "File reference: missing.txt\n"
    );
    assert_eq!(serde_json::to_value(&request).unwrap(), source);

    let blank = request.with_instructions(vec![InstructionBlock::new(
        InstructionKind::System,
        " \n\t",
        255,
    )]);
    assert!(
        to_openai_request(&blank)
            .unwrap()
            .get("instructions")
            .is_none()
    );
}

#[test]
fn rendered_tool_calls_keep_argument_bytes_and_wire_key_order() {
    // Replay preserves even malformed raw provider arguments, byte for byte.
    let raw = " {bad \"пример\"\n";
    let args = json!({"z": [42], "a": "строка\t"});
    let encoded = args.to_string();
    let freeform = "  заметка\n🦀 ";
    let req = |calls: Vec<ToolCall>| {
        CanonicalModelRequest::new(
            ModelRef::new("openai", "fixture"),
            vec![CanonicalMessage::new(
                MessageRole::Assistant,
                calls
                    .into_iter()
                    .map(|call| ContentPart::ToolCall { call })
                    .collect(),
            )],
        )
    };
    let request = req(vec![
        ToolCall::new("call_raw", "fixture_fn", json!({})).with_raw_arguments(raw),
        ToolCall::new("call_json", "fixture_fn", args),
        ToolCall::new("call_note", "fixture_note", json!({"input":freeform}))
            .with_surface(ToolCallSurface::Freeform),
    ]);
    let expected = json!([
        {"type":"function_call","call_id":"call_raw","name":"fixture_fn","arguments":raw},
        {"type":"function_call","call_id":"call_json","name":"fixture_fn","arguments":encoded},
        {"type":"custom_tool_call","call_id":"call_note","name":"fixture_note","input":freeform},
    ]);
    let body = to_openai_request(&request).unwrap();
    assert_eq!(body["input"].to_string(), expected.to_string());

    let invalid = req(vec![
        ToolCall::new("bad", "fixture_note", json!({"input":42}))
            .with_surface(ToolCallSurface::Freeform),
    ]);
    assert_eq!(
        to_openai_request(&invalid).unwrap_err().to_string(),
        "freeform tool call 'fixture_note' requires string arg 'input'"
    );
}
