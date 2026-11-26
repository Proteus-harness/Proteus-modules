use super::*;
use crate::history::normalize_missing_tool_outputs;
use proteus_contracts::{
    contracts::{WorkflowCheckpointAck, WorkflowHistoryInterruption, WorkflowHistoryUpdate},
    model_standard::PartScope,
};

fn resumed_input(result: Option<ToolResult>) -> WorkflowModuleInput {
    let mut input = workflow_input("continue");
    let current = input.history.pop().unwrap();
    input.history = vec![
        CanonicalMessage::text(MessageRole::User, "previous request"),
        tool_call_response(ToolCall::new(
            "old-call",
            "read_file",
            json!({"path":"old"}),
        ))
        .messages
        .remove(0),
    ];
    if let Some(result) = result {
        input
            .history
            .push(crate::history::tool_result_message(result));
    }
    input.history.push(current);
    input
}

fn results_for<'a>(messages: &'a [CanonicalMessage], id: &str) -> Vec<(&'a ToolResult, PartScope)> {
    messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match &part.payload {
            ContentPart::ToolResult { result } if result.call_id == id => {
                Some((result, part.scope))
            }
            _ => None,
        })
        .collect()
}

fn assert_resumed(workflow: &dyn WorkflowModule, expected_requests: usize) {
    for real_result in [
        None,
        Some(ToolResult::ok("old-call".into(), "known result")),
        Some(ToolResult::error("old-call".into(), "known failure")),
    ] {
        let input = resumed_input(real_result.clone());
        let mut host = FakeHost::default();
        let output: WorkflowModuleOutput = serde_json::from_str(
            &workflow
                .run_json(serde_json::to_string(&input).unwrap(), &mut host)
                .unwrap(),
        )
        .unwrap();
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), expected_requests);
        for request in requests.iter() {
            let results = results_for(&request.messages, "old-call");
            assert_eq!(results.len(), 1, "resumed request must close the old call");
            match &real_result {
                Some(real) => assert_eq!(results[0].0, real),
                None => {
                    assert_eq!(
                        results[0].0,
                        &ToolResult::error("old-call".into(), "aborted")
                    );
                    assert_eq!(results[0].1, PartScope::Request);
                }
            }
        }
        // The recovery must precede context preparation, not only provider encoding.
        for compaction in host.compactions.lock().unwrap().iter() {
            assert_eq!(results_for(&compaction.messages, "old-call").len(), 1);
        }
        assert_no_executed_calls(&host);
        assert!(results_for(&output.new_messages, "old-call").is_empty());
        assert!(output.history_replacement.is_none());
        for checkpoint in host.checkpoints.lock().unwrap().iter() {
            assert!(results_for(&checkpoint.history.new_messages, "old-call").is_empty());
            assert!(checkpoint.history.history_replacement.is_none());
            assert!(checkpoint.tool_results.is_empty());
        }
    }
}

#[test]
fn resumed_single_loop_closes_history_without_recording_a_fake_execution() {
    assert_resumed(&CodingSingleLoopWorkflow::default(), 1);
}

#[test]
fn resumed_plan_execute_review_closes_history_in_all_three_phases() {
    assert_resumed(&CodingPlanExecuteReviewWorkflow, 3);
}

#[test]
fn resumed_codex_loop_keeps_existing_recovery_contract() {
    assert_resumed(&CodingCodexLoopWorkflow, 1);
}

#[test]
fn missing_outputs_preserve_real_results_order_and_are_idempotent() {
    let calls = ["a", "b", "c"].map(|id| ToolCall::new(id, "read_file", json!({"path":id})));
    let mut messages = vec![
        CanonicalMessage::new(
            MessageRole::Assistant,
            calls
                .iter()
                .cloned()
                .map(|call| ContentPart::ToolCall { call })
                .collect(),
        ),
        crate::history::tool_result_message(ToolResult::ok("b".into(), "real")),
        CanonicalMessage::text(MessageRole::User, "continue"),
    ];
    let original = messages.clone();
    normalize_missing_tool_outputs(&mut messages);
    assert_eq!(messages[0], original[0]);
    assert_eq!(messages[3..], original[1..]);
    assert_eq!(results_for(&messages[1..2], "a").len(), 1);
    assert_eq!(results_for(&messages[2..3], "c").len(), 1);
    assert_eq!(
        results_for(&messages, "b")[0].0,
        &ToolResult::ok("b".into(), "real")
    );
    let normalized = messages.clone();
    normalize_missing_tool_outputs(&mut messages);
    assert_eq!(messages, normalized);
}

#[test]
fn codex_interruption_marker_follows_the_synthetic_result() {
    let mut input = resumed_input(None);
    let boundary = input.history[1].id;
    let turn_id = new_turn_id();
    input
        .runtime
        .interrupted_turns
        .push(WorkflowHistoryInterruption {
            turn_id,
            after_message_id: boundary,
        });
    let mut host = FakeHost::default();
    CodingCodexLoopWorkflow
        .run_json(serde_json::to_string(&input).unwrap(), &mut host)
        .unwrap();
    let requests = host.requests.lock().unwrap();
    let messages = &requests[0].messages;
    let call_index = messages
        .iter()
        .position(|message| message.id == boundary)
        .unwrap();
    assert_eq!(
        results_for(&messages[call_index + 1..call_index + 2], "old-call").len(),
        1
    );
    assert_eq!(messages[call_index + 2].id, turn_id);
    assert_eq!(messages[call_index + 2].parts[0].part_id, turn_id);
    assert_eq!(messages[call_index + 2].parts[0].scope, PartScope::Request);
    assert_eq!(messages[call_index + 3], *input.history.last().unwrap());
}

#[test]
fn canonical_ack_retains_request_result_without_persisting_it() {
    let mut input = resumed_input(None);
    let original = input.history.clone();
    let mut host = FakeHost::default();
    let mut turn = crate::scaffold::TurnScaffold::begin(&mut host, &mut input).unwrap();
    assert_eq!(results_for(&turn.model_messages, "old-call").len(), 1);
    let candidate = turn.model_messages.clone();
    let material = ContextPreparationInput::new(candidate.clone(), None);
    let compacted = proteus_contracts::contracts::ContextPreparationOutput::changed(
        candidate.clone(),
        Some("retained history".into()),
    );
    let report =
        proteus_contracts::domain::HistoryCompactionReport::from_context_preparation_output(
            &material, &compacted,
        );
    turn.apply_compaction_report(
        Some(&report),
        None,
        &candidate,
        crate::scaffold::PersistentRepair::Rebuild,
    )
    .unwrap();
    assert!(results_for(&turn.persistent_messages, "old-call").is_empty());
    *host.checkpoint_ack.lock().unwrap() = Some(WorkflowCheckpointAck {
        history: WorkflowHistoryUpdate::new(vec![])
            .with_history_replacement(original.clone())
            .with_compactions(vec![report]),
        messages: original.clone(),
        revision: Some(1),
    });
    turn.checkpoint(&host, &[]).unwrap();
    assert_eq!(turn.persistent_messages, original);
    assert_eq!(results_for(&turn.model_messages, "old-call").len(), 1);
    let checkpoints = host.checkpoints.lock().unwrap();
    let persisted = checkpoints[0].history.history_replacement.as_ref().unwrap();
    assert!(results_for(persisted, "old-call").is_empty());
}
