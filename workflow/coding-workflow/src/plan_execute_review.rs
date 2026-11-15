use crate::{MAX_TOOL_ROUNDS, PLAN_EXECUTE_REVIEW_MODULE_ID};
use crate::{
    host::execute_or_handle_tool,
    metadata::{output_metadata_with_extra, with_workflow_phase},
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

/// Ограничение read-only tool loop в plan-фазе `coding.plan_execute_review`:
/// план имеет право посмотреть код, но не должен превращаться в execute.
const MAX_PLAN_TOOL_ROUNDS: usize = 3;
const PLAN_SYSTEM_INSTRUCTIONS: &str = "\
You are running inside a modular coding workflow. First form a concise internal \
plan, then use tools only when they are necessary, then produce a final answer \
after reviewing the result. If the user says they are testing the agent or tools, \
focus on the requested test and do not inspect the project unless asked. Do not \
call remember_fact for temporary test notes; use it only when the user explicitly \
asks you to remember a stable preference or durable project fact. Do not invent \
dates or times; omit them unless the user supplied them or you verified them with \
a tool.";
const PLAN_DEVELOPER_INSTRUCTIONS: &str = "\
Interview-first planning phase: clarify material requirements before writing \
the final plan. You may use read-only tools to discover facts. For broad or \
underspecified tasks, call request_user_input with one focused multiple-choice \
question before writing a staged plan; ask follow-up questions only after prior \
answers when the next question depends on them. If all material requirements \
are already clear, produce a concise actionable plan. Do not ask whether the \
plan is approved; the client handles approval after the final plan. Do not use \
write, shell, network, or mutation-oriented tools in this phase.";
const EXECUTE_DEVELOPER_INSTRUCTIONS: &str = "Execute phase: follow the plan, inspect relevant context, and use available tools when they are necessary. If you are ready to answer, provide a concise draft response without calling tools.";
const REVIEW_DEVELOPER_INSTRUCTIONS: &str = "Review phase: produce the final user-facing answer. Mention what changed or what you found, and call out verification gaps if no verification was possible. Do not request tools in this phase.";

pub(crate) fn run_plan_execute_review(
    input: WorkflowModuleInput,
    host: &mut WorkflowModuleHostMut<'_>,
) -> Result<WorkflowModuleOutput, WorkflowFailure> {
    crate::intents::reject(&input).map_err(WorkflowFailure::from)?;
    let mut turn = TurnScaffold::begin(host, &input).map_err(WorkflowFailure::from)?;
    run_loop(&input, host, &mut turn).map_err(|error| turn.failure(error))
}

fn run_loop(
    input: &WorkflowModuleInput,
    host: &mut WorkflowModuleHostMut<'_>,
    turn: &mut TurnScaffold,
) -> Result<WorkflowModuleOutput, ProcessModuleError> {
    let mut last_usage: Option<LastModelUsage> = None;
    let mut plan_tool_rounds_used = 0usize;
    for plan_round in 0..=MAX_PLAN_TOOL_ROUNDS {
        // The last plan round must finish with text, not a pending tool call.
        let forced_text_round = plan_round == MAX_PLAN_TOOL_ROUNDS;
        let plan_response = sample_step(
            input,
            host,
            turn,
            &mut last_usage,
            SamplingStep {
                phase: "plan",
                system_instructions: PLAN_SYSTEM_INSTRUCTIONS,
                developer_instructions: Some(PLAN_DEVELOPER_INSTRUCTIONS),
                disable_tools: forced_text_round,
            },
        )?;
        let plan_messages = plan_response
            .messages
            .into_iter()
            .map(|message| with_workflow_phase(message, PLAN_EXECUTE_REVIEW_MODULE_ID, "plan"))
            .collect::<Vec<_>>();
        turn.model_messages.extend(plan_messages.iter().cloned());
        if let Some(usage) = plan_response.usage.clone() {
            last_usage = Some(LastModelUsage {
                usage,
                message_count: turn.model_messages.len(),
            });
        }

        let should_run_tools = plan_response.finish_reason == FinishReason::ToolCalls
            && !plan_response.tool_calls.is_empty();
        if forced_text_round || !should_run_tools {
            // The private plan must survive canonical durable ACK replacement,
            // without becoming public conversation history.
            let start = turn.model_messages.len() - plan_messages.len();
            make_private_projection(&mut turn.model_messages[start..]);
            break;
        }
        plan_tool_rounds_used += 1;
        turn.persistent_messages.extend(plan_messages);
        turn.checkpoint_tools(host, input, &plan_response.tool_calls, "plan")?;
        for call in plan_response.tool_calls {
            let result = execute_or_handle_tool(host, input, &call, "plan")?;
            turn.append_tool_results(std::iter::once(result))?;
            turn.checkpoint(host, &[])?;
        }
    }

    let mut draft_finish_reason = None;
    let mut tool_round_limit_reached = true;
    for _round in 0..MAX_TOOL_ROUNDS {
        let response = sample_step(
            input,
            host,
            turn,
            &mut last_usage,
            SamplingStep {
                phase: "execute",
                system_instructions: PLAN_SYSTEM_INSTRUCTIONS,
                developer_instructions: Some(EXECUTE_DEVELOPER_INSTRUCTIONS),
                disable_tools: false,
            },
        )?;

        let finish_reason = response.finish_reason.clone();
        turn.model_messages
            .extend(response.messages.iter().cloned());
        if let Some(usage) = response.usage.clone() {
            last_usage = Some(LastModelUsage {
                usage,
                message_count: turn.model_messages.len(),
            });
        }
        let should_run_tools =
            response.finish_reason == FinishReason::ToolCalls && !response.tool_calls.is_empty();
        if should_run_tools {
            turn.persistent_messages
                .extend(response.messages.iter().cloned());
        }
        if !should_run_tools {
            // The draft is also request-only, not a durable steering anchor.
            let start = turn.model_messages.len() - response.messages.len();
            make_private_projection(&mut turn.model_messages[start..]);
            draft_finish_reason = Some(finish_reason);
            tool_round_limit_reached = false;
            break;
        }

        turn.checkpoint_tools(host, input, &response.tool_calls, "execute")?;
        for call in response.tool_calls {
            let result = execute_or_handle_tool(host, input, &call, "execute")?;
            turn.append_tool_results(std::iter::once(result))?;
            turn.checkpoint(host, &[])?;
        }
    }

    let final_response = sample_step(
        input,
        host,
        turn,
        &mut last_usage,
        SamplingStep {
            phase: "review",
            system_instructions: PLAN_SYSTEM_INSTRUCTIONS,
            developer_instructions: Some(REVIEW_DEVELOPER_INSTRUCTIONS),
            disable_tools: true,
        },
    )?;

    let final_message = response_output_message("review", &final_response)?.clone();
    turn.model_messages
        .extend(final_response.messages.iter().cloned());
    turn.persistent_messages
        .extend(final_response.messages.iter().cloned());
    turn.checkpoint(host, &[])?;
    let text = output_text(
        &final_message,
        &turn.model_messages[turn.current_turn_messages_start..],
    );
    let metadata = output_metadata_with_extra(
        PLAN_EXECUTE_REVIEW_MODULE_ID,
        input,
        &turn.model_messages,
        turn.context_chunks,
        turn.context_token_estimate,
        json!({
            "max_tool_rounds": MAX_TOOL_ROUNDS,
            "tool_round_limit_reached": tool_round_limit_reached,
            "draft_finish_reason": draft_finish_reason,
            "max_plan_tool_rounds": MAX_PLAN_TOOL_ROUNDS,
            "plan_tool_rounds_used": plan_tool_rounds_used,
            "phases": ["plan", "execute", "review"],
        }),
    );
    turn.finish(host, text, metadata)
}

/// A private phase projection must not reuse the provider's durable identities.
/// Empty-part messages retain the existing ACK/compaction classification limit.
fn make_private_projection(messages: &mut [proteus_contracts::model_standard::CanonicalMessage]) {
    use proteus_contracts::model_standard::{PartProvenance, PartScope};
    use serde_json::Value;

    for message in messages {
        let source = message.id;
        message.id = proteus_contracts::domain::new_message_id();
        match &mut message.metadata {
            Value::Object(metadata) => {
                metadata.insert("workflow_private_source_message".to_owned(), json!(source));
            }
            Value::Null => {
                message.metadata = json!({ "workflow_private_source_message": source });
            }
            other => {
                let previous = std::mem::replace(other, Value::Null);
                message.metadata = json!({
                    "workflow_private_source_message": source,
                    "previous_metadata": previous,
                });
            }
        }
        for part in &mut message.parts {
            part.part_id = proteus_contracts::domain::new_part_id();
            part.provenance = PartProvenance::Runtime;
            part.scope = PartScope::Request;
        }
    }
}
