use super::*;
use crate::model_standard::{InstructionBlock, InstructionKind};

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
