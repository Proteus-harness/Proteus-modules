use super::*;
use crate::model_standard::{InstructionBlock, InstructionKind};

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
