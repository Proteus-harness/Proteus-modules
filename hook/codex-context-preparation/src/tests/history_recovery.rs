use super::*;
use proteus_contracts::model_standard::PartScope;

#[test]
fn request_tool_pairs_are_summarized_not_reinjected_without_their_calls() {
    for call_scope in [PartScope::Conversation, PartScope::Request] {
        for compact in [false, true] {
            let mut context = CanonicalMessage::text(MessageRole::User, "workspace context");
            context.parts[0].scope = PartScope::Request;
            let mut marker = CanonicalMessage::text(
                MessageRole::User,
                "<turn_aborted>interrupted</turn_aborted>",
            );
            marker.parts[0].scope = PartScope::Request;
            let mut call = CanonicalMessage::new(
                MessageRole::Assistant,
                vec![ContentPart::ToolCall {
                    call: ToolCall::new("old-call", "read_file", json!({"path":"old"})),
                }],
            );
            call.parts[0].scope = call_scope;
            let mut result = CanonicalMessage::new(
                MessageRole::Tool,
                vec![ContentPart::ToolResult {
                    result: ToolResult::error("old-call".into(), "aborted"),
                }],
            )
            .with_tool_call_id("old-call".into());
            result.parts[0].scope = PartScope::Request;
            let messages = vec![
                context.clone(),
                CanonicalMessage::text(MessageRole::User, "previous request"),
                call.clone(),
                result.clone(),
                marker.clone(),
                CanonicalMessage::text(MessageRole::User, "continue"),
            ];
            let mut host = TestHost::with_response("summary");
            let prepared = input(messages.clone(), if compact { 500 } else { 0 });
            let original = prepared.preparation();
            let output = compact_with_host(prepared, &mut host);
            proteus_contracts::contracts::validate_context_preparation_output(&original, &output)
                .unwrap();
            assert_eq!(output.changed, compact);
            if compact {
                assert!(
                    !output
                        .messages
                        .iter()
                        .flat_map(|message| &message.parts)
                        .any(|part| matches!(
                            part.payload,
                            ContentPart::ToolCall { .. } | ContentPart::ToolResult { .. }
                        ))
                );
                assert!(output.messages.contains(&context));
                assert!(output.messages.contains(&marker));
                let requests = host.requests.lock().unwrap();
                assert_eq!(requests.len(), 1);
                assert!(requests[0].messages.contains(&call));
                assert!(requests[0].messages.contains(&result));
            } else {
                assert_eq!(output.messages, messages);
                assert!(host.requests.lock().unwrap().is_empty());
            }
        }
    }
}
