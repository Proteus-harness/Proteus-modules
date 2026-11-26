use crate::SINGLE_LOOP_MODULE_ID;
use crate::{
    host::execute_tools,
    metadata::{output_metadata, output_metadata_with_extra},
    output_text::output_text,
    sampling::{SamplingStep, sample_step},
    scaffold::TurnScaffold,
    token_accounting::LastModelUsage,
    validation::response_output_message,
};
use proteus_contracts::{
    contracts::WorkflowFailure,
    model_standard::FinishReason,
    process_module::{
        ProcessModuleError, WorkflowModuleHostMut, WorkflowModuleInput, WorkflowModuleOutput,
    },
};
use serde_json::json;

const SYSTEM_INSTRUCTIONS: &str = "\
You are running inside a modular v0 agent skeleton. Answer normal conversational \
questions directly. Use tools only when they are necessary and only if they are \
included in the current tool list. If the user says they are testing the agent \
or tools, focus on the requested test and do not inspect the project unless \
asked. Do not call remember_fact for temporary test notes; use it only when the \
user explicitly asks you to remember a stable preference or durable project fact. \
Do not invent dates or times; omit them unless the user supplied them or you \
verified them with a tool.";
pub(crate) fn run_single_loop(
    mut input: WorkflowModuleInput,
    host: &mut WorkflowModuleHostMut<'_>,
    max_tool_rounds: usize,
) -> Result<WorkflowModuleOutput, WorkflowFailure> {
    crate::intents::instructions(&input).map_err(WorkflowFailure::from)?;
    let mut turn = TurnScaffold::begin(host, &mut input).map_err(WorkflowFailure::from)?;
    run_loop(&input, host, max_tool_rounds, &mut turn).map_err(|error| turn.failure(error))
}

fn run_loop(
    input: &WorkflowModuleInput,
    host: &mut WorkflowModuleHostMut<'_>,
    max_tool_rounds: usize,
    turn: &mut TurnScaffold,
) -> Result<WorkflowModuleOutput, ProcessModuleError> {
    let mut last_usage: Option<LastModelUsage> = None;

    for _round in 0..max_tool_rounds {
        let response = sample_step(
            input,
            host,
            turn,
            &mut last_usage,
            SamplingStep {
                phase: "single_loop",
                system_instructions: SYSTEM_INSTRUCTIONS,
                developer_instructions: None,
                disable_tools: false,
            },
        )?;

        turn.model_messages
            .extend(response.messages.iter().cloned());
        turn.persistent_messages
            .extend(response.messages.iter().cloned());
        if let Some(usage) = response.usage.clone() {
            last_usage = Some(LastModelUsage {
                usage,
                message_count: turn.model_messages.len(),
            });
        }
        let should_run_tools =
            response.finish_reason == FinishReason::ToolCalls && !response.tool_calls.is_empty();
        if !should_run_tools {
            turn.checkpoint(host, &[])?;
            let output_message = response_output_message("single_loop", &response)?;
            let text = output_text(
                output_message,
                &turn.model_messages[turn.current_turn_messages_start..],
            );
            let metadata = output_metadata(
                SINGLE_LOOP_MODULE_ID,
                input,
                &turn.model_messages,
                turn.context_chunks,
                turn.context_token_estimate,
            );
            return turn.finish(host, text, metadata);
        }

        turn.checkpoint_tools(host, input, &response.tool_calls, "single_loop")?;
        let results = execute_tools(host, input, &response.tool_calls, "single_loop")?;
        turn.append_tool_results(results)?;
        turn.checkpoint(host, &[])?;
    }

    let response = sample_step(
        input,
        host,
        turn,
        &mut last_usage,
        SamplingStep {
            phase: "single_loop_final",
            system_instructions: SYSTEM_INSTRUCTIONS,
            developer_instructions: None,
            disable_tools: true,
        },
    )?;

    turn.model_messages
        .extend(response.messages.iter().cloned());
    turn.persistent_messages
        .extend(response.messages.iter().cloned());
    turn.checkpoint(host, &[])?;
    let output_message = response_output_message("single_loop_final", &response)?;
    let text = output_text(
        output_message,
        &turn.model_messages[turn.current_turn_messages_start..],
    );
    let metadata = output_metadata_with_extra(
        SINGLE_LOOP_MODULE_ID,
        input,
        &turn.model_messages,
        turn.context_chunks,
        turn.context_token_estimate,
        json!({
            "max_tool_rounds": max_tool_rounds,
            "tool_round_limit_reached": true,
        }),
    );
    turn.finish(host, text, metadata)
}
