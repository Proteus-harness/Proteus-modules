use super::*;
use crate::output_text::output_text;

#[test]
fn empty_text_response_gets_placeholder() {
    let message = CanonicalMessage::new(MessageRole::Assistant, Vec::new());

    assert_eq!(message_text(&message), "<empty model response>");
}

#[test]
fn empty_final_output_falls_back_to_latest_tool_result() {
    let result = ToolResult::new(
        proteus_contracts::domain::new_call_id(),
        false,
        "usage: skatewind --place NAME".to_owned(),
        Vec::new(),
        Some("process exited with code 1".to_owned()),
        json!({}),
    );
    let messages = vec![CanonicalMessage::new(
        MessageRole::Tool,
        vec![ContentPart::ToolResult { result }],
    )];
    let message = CanonicalMessage::new(MessageRole::Assistant, Vec::new());

    let text = output_text(&message, &messages);

    assert!(text.contains("Model returned an empty final response"));
    assert!(text.contains("usage: skatewind --place NAME"));
    assert!(text.contains("process exited with code 1"));
}

#[test]
fn empty_final_output_does_not_fall_back_to_previous_turn_tool_result() {
    let result = ToolResult::new(
        proteus_contracts::domain::new_call_id(),
        false,
        "old turn output".to_owned(),
        Vec::new(),
        Some("old turn error".to_owned()),
        json!({}),
    );
    let history = [CanonicalMessage::new(
        MessageRole::Tool,
        vec![ContentPart::ToolResult { result }],
    )];
    let message = CanonicalMessage::new(MessageRole::Assistant, Vec::new());

    let text = output_text(&message, &history[history.len()..]);

    assert_eq!(text, "<empty model response>");
}

#[test]
fn estimates_tokens_from_text_context_and_tool_results() {
    let result =
        ToolResult::ok(proteus_contracts::domain::new_call_id(), "abcd").with_metadata(json!({}));
    let messages = vec![
        CanonicalMessage::text(MessageRole::User, "abcd"),
        CanonicalMessage::new(MessageRole::Tool, vec![ContentPart::ToolResult { result }]),
    ];

    assert_eq!(estimate_message_tokens(&messages), Some(4));
}

#[test]
fn empty_final_output_truncates_tool_summary_at_unicode_scalar_boundary() {
    let message = CanonicalMessage::new(MessageRole::Assistant, Vec::new());
    for unit in ["x", "🦀"] {
        for length in [1_999, 2_000, 2_001] {
            let result = ToolResult::ok(
                proteus_contracts::domain::new_call_id(),
                unit.repeat(length),
            );
            let messages = vec![CanonicalMessage::new(
                MessageRole::Tool,
                vec![ContentPart::ToolResult { result }],
            )];
            let suffix = if length > 2_000 { "\n[truncated]" } else { "" };
            let expected = format!(
                "Model returned an empty final response after the last tool call.\n\nLast tool result:\n{}{suffix}",
                unit.repeat(length.min(2_000)),
            );

            assert_eq!(
                output_text(&message, &messages),
                expected,
                "{unit} {length}"
            );
        }
    }
}

#[test]
fn empty_final_output_counts_separator_and_omits_blank_fields() {
    let empty = CanonicalMessage::new(MessageRole::Assistant, vec![]);
    let prefix =
        "Model returned an empty final response after the last tool call.\n\nLast tool result:\n";
    for (output, error, expected) in [
        ("\t".into(), Some("  例  "), Some("例".into())),
        (" 例 ".into(), Some("\u{2003}"), Some("例".into())),
        ("  ".into(), None, None),
        (
            "é".repeat(1_998),
            Some("🦀"),
            Some(format!("{}\n🦀", "é".repeat(1_998))),
        ),
        (
            "é".repeat(1_999),
            Some("🦀"),
            Some(format!("{}\n\n[truncated]", "é".repeat(1_999))),
        ),
        (
            "é".repeat(2_000),
            Some("🦀"),
            Some(format!("{}\n[truncated]", "é".repeat(2_000))),
        ),
    ] {
        let result = ToolResult::new(
            proteus_contracts::domain::new_call_id(),
            false,
            output,
            vec![],
            error.map(str::to_owned),
            json!({}),
        );
        let history = [CanonicalMessage::new(
            MessageRole::Tool,
            vec![ContentPart::ToolResult { result }],
        )];
        let expected = expected.map_or_else(
            || "<empty model response>".into(),
            |summary| format!("{prefix}{summary}"),
        );
        assert_eq!(output_text(&empty, &history), expected);
    }
}
