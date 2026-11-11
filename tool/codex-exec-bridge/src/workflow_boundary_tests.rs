//! Checked eligibility boundaries using the actual current Core validators.
use super::*;
use crate::contracts::WorkflowToolResultBinding;
use crate::core_history_capture::HistoryCapture;
use crate::model_standard::{CanonicalMessage, ContentPart, MessageRole};
use std::collections::HashMap;

fn history(call: ToolCall) -> Vec<CanonicalMessage> {
    vec![CanonicalMessage::new(
        MessageRole::Assistant,
        vec![ContentPart::ToolCall { call }],
    )]
}

#[test]
fn current_core_rejects_native_child_binding_without_a_history_call() {
    let parent = ToolCall::new(
        "exec-parent",
        "exec",
        json!({"code":"await tools.read({path:'a'})"}),
    );
    let child = ToolCall::new("exec-parent/tool-1", "read", json!({"path":"a"}));
    let error = HistoryCapture::new(&history(parent), &[WorkflowToolResultBinding::new(child)])
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "checkpoint binding exec-parent/tool-1 has no history call"
    );
    println!("WORKFLOW_CHECKPOINT_CHILD_PROOF {error}");
}

#[test]
fn current_core_allows_one_explicit_execution_per_model_call_but_not_many() {
    let parent = ToolCall::new(
        "exec-parent",
        "exec",
        json!({"code":"await tools.read({path:'a'})"}),
    );
    let first = ToolCall::new("exec-parent", "read", json!({"path":"a"}));
    let second = ToolCall::new("exec-parent", "read", json!({"path":"b"}));
    let bindings = [
        WorkflowToolResultBinding::new(first.clone()),
        WorkflowToolResultBinding::new(second.clone()),
    ];
    let capture = HistoryCapture::new(&history(parent.clone()), &bindings[..1]).unwrap();
    capture.validate_call(&first).unwrap();
    assert_eq!(
        capture.validate_call(&second).unwrap_err().to_string(),
        "executed tool call disagrees with checkpoint execution binding"
    );
    assert_eq!(
        HistoryCapture::new(&history(parent), &bindings)
            .unwrap_err()
            .to_string(),
        "duplicate checkpoint tool binding exec-parent"
    );
}

#[test]
fn unbound_workflow_callbacks_are_executable_but_not_durable_root_results() {
    let parent = ToolCall::new(
        "exec-parent",
        "exec",
        json!({"code":"await tools.read({path:'a'})"}),
    );
    let child = ToolCall::new("exec-parent/tool-1", "read", json!({"path":"a"}));
    let mut messages = history(parent);
    let mut capture = HistoryCapture::new(&messages, &[]).unwrap();
    // Lack of a checkpoint binding grants no policy bypass; it just means
    // HistoryCapture does not reserve a conversation result for this operation.
    capture.validate_call(&child).unwrap();
    assert!(
        !capture
            .record(
                &mut messages,
                &ToolResult::ok(child.id, "recorded callback")
            )
            .unwrap()
    );
    assert_eq!(messages.len(), 1);
}

#[test]
fn actual_native_random_workflow_result_fails_current_core_replay_equality() {
    let native = crate::tests::tool();
    let host = crate::tests::Host::default();
    let first = native
        .execute(
            "exec-parent",
            "text(Math.random());",
            Duration::from_secs(2),
            &host,
        )
        .unwrap();
    let second = native
        .execute(
            "exec-parent",
            "text(Math.random());",
            Duration::from_secs(2),
            &host,
        )
        .unwrap();
    let first = crate::tests::text(&first);
    let second = crate::tests::text(&second);
    assert_ne!(first, second);
    let messages = |output| {
        vec![
            CanonicalMessage::new(
                MessageRole::Tool,
                vec![ContentPart::ToolResult {
                    result: ToolResult::ok("exec-parent".into(), output),
                }],
            )
            .with_tool_call_id("exec-parent".into()),
        ]
    };
    assert!(!crate::core_replay_normalize::messages_equal(
        &messages(first.clone()),
        &messages(second.clone()),
        &HashMap::new()
    ));
    println!(
        "WORKFLOW_REPLAY_RANDOM_PROOF {}",
        json!({"first":first,"replayed":second,"core_messages_equal":false})
    );
}

#[test]
fn workflow_function_surface_has_no_native_freeform_call_surface() {
    use proteus_contracts::domain::{ToolCallSurface, ToolSurface};
    assert_eq!(
        ToolSurface::workflow_function().call_surface(),
        Some(ToolCallSurface::Function)
    );
    assert_eq!(
        ToolSurface::freeform_lark("start: /[\\s\\S]+/").call_surface(),
        Some(ToolCallSurface::Freeform)
    );
}
