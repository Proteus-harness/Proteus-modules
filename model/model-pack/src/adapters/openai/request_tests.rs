use super::*;
use crate::model_standard::{InstructionBlock, InstructionKind};

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
        "include": [],
    });
    let body = to_openai_request(&request).unwrap();
    assert_eq!(body, expected);
    // Value equality alone does not check key order with preserve_order enabled.
    assert_eq!(body.to_string(), expected.to_string());
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
