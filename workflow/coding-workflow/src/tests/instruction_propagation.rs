use super::*;
use proteus_contracts::contracts::{WorkflowContinuation, WorkflowHistoryUpdate};

fn input_with_instructions(continued: bool) -> WorkflowModuleInput {
    let mut input = workflow_input("answer the question");
    input.runtime.instructions = vec![
        InstructionBlock::new(InstructionKind::System, "profile system", 100),
        InstructionBlock::new(InstructionKind::Developer, "shared text", 80),
        InstructionBlock::new(InstructionKind::Project, "shared text", 80),
        InstructionBlock::new(InstructionKind::UserPreference, "profile preference", 0),
    ];
    if continued {
        let candidate = CanonicalMessage::text(MessageRole::Assistant, "first candidate");
        let reason = "Include the requested verification result";
        input.runtime.continuation = Some(WorkflowContinuation {
            attempt: 1,
            reason: reason.to_owned(),
            current_user_message_id: input.history[0].id,
            history: WorkflowHistoryUpdate::new(vec![candidate.clone()]),
        });
        input.history.push(candidate);
        // Match Core's reviewed_workflow boundary: the reason is already a block.
        input.runtime.instructions.push(InstructionBlock::new(
            InstructionKind::Developer,
            reason,
            128,
        ));
    }
    input
}

#[test]
fn legacy_request_builder_keeps_base_runtime_and_phase_blocks_in_order() {
    for runtime_blocks in [
        Vec::new(),
        input_with_instructions(true).runtime.instructions,
    ] {
        let mut input = workflow_input("answer the question");
        input.runtime.instructions = runtime_blocks.clone();
        let host = FakeHost::default();
        let prepared = crate::host::request_from_state(
            &input,
            &host,
            &input.history,
            "workflow system",
            Some("phase instruction"),
            "execute",
            None,
        )
        .expect("request prepares");

        let mut expected = vec![InstructionBlock::new(
            InstructionKind::System,
            "workflow system",
            100,
        )];
        expected.extend(runtime_blocks.clone());
        expected.push(InstructionBlock::new(
            InstructionKind::Developer,
            "phase instruction",
            90,
        ));
        assert_eq!(prepared.request.instructions, expected);
        assert_eq!(input.runtime.instructions, runtime_blocks);
    }
}

fn assert_legacy_requests(
    workflow: &dyn WorkflowModule,
    input: &WorkflowModuleInput,
    phases: &[&str],
    has_phase_instruction: bool,
) {
    let mut host = FakeHost::default();
    workflow
        .run_json(serde_json::to_string(input).unwrap(), &mut host)
        .expect("workflow succeeds");
    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), phases.len());
    let compactions = host.compactions.lock().unwrap();
    assert_eq!(
        compactions
            .iter()
            .map(|input| input.reason.as_deref())
            .collect::<Vec<_>>(),
        phases.iter().copied().map(Some).collect::<Vec<_>>()
    );
    for (request, preparation) in requests.iter().zip(compactions.iter()) {
        assert_eq!(
            preparation.request.as_ref().unwrap().instructions,
            request.instructions
        );
        let runtime_end = 1 + input.runtime.instructions.len();
        assert_eq!(
            request.instructions.len(),
            runtime_end + usize::from(has_phase_instruction)
        );
        assert_eq!(request.instructions[0].kind, InstructionKind::System);
        assert_eq!(request.instructions[0].priority, 100);
        assert!(!request.instructions[0].text.is_empty());
        assert_eq!(
            &request.instructions[1..runtime_end],
            input.runtime.instructions.as_slice()
        );
        if has_phase_instruction {
            assert_eq!(
                request.instructions[runtime_end].kind,
                InstructionKind::Developer
            );
            assert_eq!(request.instructions[runtime_end].priority, 90);
        }
    }
}

#[test]
fn profile_and_review_instructions_reach_every_legacy_sampling_path() {
    for continued in [false, true] {
        let input = input_with_instructions(continued);
        assert_legacy_requests(
            &CodingSingleLoopWorkflow::default(),
            &input,
            &["single_loop"],
            false,
        );
        assert_legacy_requests(
            &CodingSingleLoopWorkflow { max_tool_rounds: 0 },
            &input,
            &["single_loop_final"],
            false,
        );
        assert_legacy_requests(
            &CodingPlanExecuteReviewWorkflow,
            &input,
            &["plan", "execute", "review"],
            true,
        );
    }
}

#[test]
fn codex_first_and_follow_up_requests_keep_runtime_blocks_exactly_once() {
    for continued in [false, true] {
        let input = input_with_instructions(continued);
        let mut host = FakeHost::with_responses(vec![
            CanonicalModelResponse::new(
                CanonicalMessage::text(MessageRole::Assistant, "working"),
                Vec::new(),
                FinishReason::Stop,
            )
            .with_end_turn(false),
        ]);
        CodingCodexLoopWorkflow
            .run_json(serde_json::to_string(&input).unwrap(), &mut host)
            .expect("workflow succeeds");
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(request.instructions, input.runtime.instructions);
        }
    }
}
