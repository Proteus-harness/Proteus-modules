use super::*;
use proteus_contracts::model_standard::{PartProvenance, PartScope};

fn text_response(message: CanonicalMessage) -> CanonicalModelResponse {
    CanonicalModelResponse::new(message, Vec::new(), FinishReason::Stop)
}

fn assert_private_projection(projected: &CanonicalMessage, source: &CanonicalMessage) {
    assert_ne!(projected.id, source.id);
    assert_eq!(
        projected.metadata["workflow_private_source_message"],
        json!(source.id)
    );
    assert_eq!(projected.parts.len(), source.parts.len());
    let mut normalized = projected.clone();
    normalized.id = source.id;
    normalized.metadata = source.metadata.clone();
    for (part, original) in normalized.parts.iter_mut().zip(&source.parts) {
        assert_ne!(part.part_id, original.part_id);
        assert_eq!(part.provenance, PartProvenance::Runtime);
        assert_eq!(part.scope, PartScope::Request);
        part.part_id = original.part_id;
        part.provenance = original.provenance;
        part.scope = original.scope;
    }
    // All fields other than projection identity, scope, provenance and metadata
    // remain canonical payload facts, including provider phase and ordering.
    assert_eq!(&normalized, source);
}

#[test]
fn terminal_plan_and_draft_project_every_message_and_part() {
    let input = workflow_input("answer");
    let plan = vec![
        CanonicalMessage::new(
            MessageRole::Assistant,
            vec![
                ContentPart::ReasoningSummary {
                    text: "outline".into(),
                },
                ContentPart::Text {
                    text: "plan one".into(),
                },
            ],
        )
        .with_phase(MessagePhase::Commentary)
        .with_metadata(json!({"provider": "plan"})),
        CanonicalMessage::text(MessageRole::Assistant, "plan two")
            .with_phase(MessagePhase::FinalAnswer),
    ];
    let draft = vec![
        CanonicalMessage::new(
            MessageRole::Assistant,
            vec![
                ContentPart::Opaque {
                    format: "test.opaque".into(),
                    data: json!({"ordered": [1, 2]}),
                },
                ContentPart::Text {
                    text: "draft one".into(),
                },
            ],
        )
        .with_name("draft")
        .with_phase(MessagePhase::Commentary),
        CanonicalMessage::text(MessageRole::Assistant, "draft two")
            .with_phase(MessagePhase::FinalAnswer),
    ];
    let final_message = CanonicalMessage::text(MessageRole::Assistant, "final");
    let mut host = FakeHost::with_responses(vec![
        CanonicalModelResponse::from_messages(plan.clone(), Vec::new(), FinishReason::Stop),
        CanonicalModelResponse::from_messages(draft.clone(), Vec::new(), FinishReason::Stop),
        text_response(final_message.clone()),
    ]);
    let result = CodingPlanExecuteReviewWorkflow
        .run_json(serde_json::to_string(&input).unwrap(), &mut host)
        .unwrap();
    let output: WorkflowModuleOutput = serde_json::from_str(&result).unwrap();
    assert_eq!(output.new_messages, vec![final_message]);

    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let plan_start = requests[1].messages.len() - plan.len();
    let projected_plan = &requests[1].messages[plan_start..];
    let draft_start = requests[2].messages.len() - draft.len();
    let projected_draft = &requests[2].messages[draft_start..];
    for (projected, source) in projected_plan.iter().zip(&plan) {
        assert_private_projection(projected, source);
        assert_eq!(projected.metadata["workflow_phase"], "plan");
        assert_eq!(
            projected.metadata["workflow_module"],
            PLAN_EXECUTE_REVIEW_MODULE_ID
        );
        assert!(
            !requests[2]
                .messages
                .iter()
                .any(|message| message.id == source.id)
        );
    }
    assert_eq!(projected_plan[0].metadata["provider"], "plan");
    for (projected, source) in projected_draft.iter().zip(&draft) {
        assert_private_projection(projected, source);
        assert!(projected.metadata.get("workflow_phase").is_none());
        assert!(
            !requests[2]
                .messages
                .iter()
                .any(|message| message.id == source.id)
        );
    }
    let projected = projected_plan.iter().chain(projected_draft);
    let ids = projected
        .clone()
        .map(|message| message.id)
        .collect::<std::collections::HashSet<_>>();
    let part_ids = projected
        .flat_map(|message| &message.parts)
        .map(|part| part.part_id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 4);
    assert_eq!(part_ids.len(), 6);
    // Exercise the actual rebuild classifier, not a second implementation of it.
    let durable = crate::history::persistent_messages_from_model_messages(&requests[2].messages);
    assert!(durable.iter().all(|message| !ids.contains(&message.id)));
}

#[test]
fn private_draft_preserves_all_metadata_shapes_without_phase_tagging() {
    for metadata in [
        Value::Null,
        json!({"provider": 7, "workflow_private_source_message": "old"}),
        json!("provider metadata"),
        json!(["provider", {"value": 7}]),
        json!(42),
        json!(false),
    ] {
        let draft =
            CanonicalMessage::text(MessageRole::Assistant, "draft").with_metadata(metadata.clone());
        let mut host = FakeHost::with_responses(vec![
            text_response(CanonicalMessage::text(MessageRole::Assistant, "plan")),
            text_response(draft.clone()),
            text_response(CanonicalMessage::text(MessageRole::Assistant, "final")),
        ]);
        CodingPlanExecuteReviewWorkflow
            .run_json(
                serde_json::to_string(&workflow_input("answer")).unwrap(),
                &mut host,
            )
            .unwrap();
        let requests = host.requests.lock().unwrap();
        let projected = requests[2].messages.last().unwrap();
        assert_private_projection(projected, &draft);
        let expected = match metadata {
            Value::Object(mut object) => {
                object.insert("workflow_private_source_message".into(), json!(draft.id));
                Value::Object(object)
            }
            Value::Null => json!({"workflow_private_source_message": draft.id}),
            other => json!({
                "workflow_private_source_message": draft.id,
                "previous_metadata": other,
            }),
        };
        assert_eq!(projected.metadata, expected);
    }
}

#[test]
fn private_projection_leaves_tool_bearing_responses_and_final_canonical() {
    let tool = test_tool("read_file", "Read", ToolSafety::ReadOnly);
    let plan_tool = tool_call_response(ToolCall::new(
        new_call_id(),
        "read_file",
        json!({"path": "plan.rs"}),
    ));
    let execute_tool = tool_call_response(ToolCall::new(
        new_call_id(),
        "read_file",
        json!({"path": "execute.rs"}),
    ));
    let draft = CanonicalMessage::text(MessageRole::Assistant, "draft");
    let final_message = CanonicalMessage::text(MessageRole::Assistant, "final");
    let mut host = FakeHost::with_responses(vec![
        plan_tool.clone(),
        text_response(CanonicalMessage::text(MessageRole::Assistant, "plan")),
        execute_tool.clone(),
        text_response(draft.clone()),
        text_response(final_message.clone()),
    ])
    .with_tools(vec![tool.clone()], vec![tool]);
    let result = CodingPlanExecuteReviewWorkflow
        .run_json(
            serde_json::to_string(&workflow_input("answer")).unwrap(),
            &mut host,
        )
        .unwrap();
    let output: WorkflowModuleOutput = serde_json::from_str(&result).unwrap();
    let plan_source = &plan_tool.messages[0];
    let saved_plan = output
        .new_messages
        .iter()
        .find(|message| message.id == plan_source.id)
        .unwrap();
    assert_eq!(saved_plan.parts, plan_source.parts);
    assert_eq!(saved_plan.metadata["workflow_phase"], "plan");
    assert!(
        saved_plan
            .metadata
            .get("workflow_private_source_message")
            .is_none()
    );
    assert!(output.new_messages.contains(&execute_tool.messages[0]));
    assert!(output.new_messages.contains(&final_message));
    assert!(
        !output
            .new_messages
            .iter()
            .any(|message| message.id == draft.id)
    );
    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_private_projection(requests[4].messages.last().unwrap(), &draft);
    assert!(requests[4].messages.contains(&execute_tool.messages[0]));
    assert_eq!(host.executed_calls.lock().unwrap().len(), 2);
}
