//! Shared sampling lifecycle for the non-streaming reference workflows.
//!
//! Phase selection and history/tool-result ownership remain with the caller.
use crate::{
    host::{complete_model, emit_event, request_from_state},
    scaffold::{PersistentRepair, TurnScaffold},
    token_accounting::LastModelUsage,
    validation::validate_model_response,
};
use proteus_contracts::{
    domain::{Event, ToolChoice},
    model_standard::CanonicalModelResponse,
    process_module::{ProcessModuleError, WorkflowModuleHostMut, WorkflowModuleInput},
};

pub(crate) struct SamplingStep<'a> {
    pub phase: &'a str,
    pub system_instructions: &'a str,
    pub developer_instructions: Option<&'a str>,
    pub disable_tools: bool,
}

pub(crate) fn sample_step(
    input: &WorkflowModuleInput,
    host: &mut WorkflowModuleHostMut<'_>,
    turn: &mut TurnScaffold,
    last_usage: &mut Option<LastModelUsage>,
    step: SamplingStep<'_>,
) -> Result<CanonicalModelResponse, ProcessModuleError> {
    let mut prepared = request_from_state(
        input,
        host,
        &turn.model_messages,
        step.system_instructions,
        step.developer_instructions,
        step.phase,
        last_usage.as_ref(),
    )?;
    if turn.apply_compaction_report(
        prepared.compaction.as_ref(),
        prepared.preparation_target.as_ref(),
        &prepared.request.messages,
        PersistentRepair::Rebuild,
    )? {
        *last_usage = None;
        turn.checkpoint(host, &[])?;
        prepared.request.messages = turn.model_messages.clone();
    }
    let mut request = prepared.request;
    // Preserve preparation's selected-tool budget. Suppress tools only after
    // any history replacement has been acknowledged, as in the original loops.
    if step.disable_tools {
        request.tools.clear();
        request.tool_choice = ToolChoice::None;
    }
    emit_event(
        host,
        &Event::ModelRequestPrepared {
            model: request.model.clone(),
        },
    )?;
    let response = complete_model(host, &request, step.phase)?;
    emit_event(
        host,
        &Event::ModelResponseReceived {
            finish_reason: response.finish_reason.clone(),
        },
    )?;
    validate_model_response(step.phase, &request, &response)?;
    Ok(response)
}
