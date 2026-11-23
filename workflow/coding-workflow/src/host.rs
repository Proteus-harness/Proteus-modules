use proteus_contracts::{
    contracts::{
        ContextPreparationInput, ContextPreparationResult, ContextPreparationTarget,
        ToolSelectionRequest,
    },
    domain::{
        CacheHints, ContextBundle, Event, HistoryCompactionReport, ToolCall, ToolResult, ToolSpec,
    },
    model_standard::{
        CanonicalMessage, CanonicalModelRequest, CanonicalModelResponse, ContentPart,
        InstructionBlock, InstructionKind, TokenUsage,
        input_estimates::{ContextTokenEstimateSource, ModelInputEstimate},
    },
    process_module::{ProcessModuleError, WorkflowModuleHostMut, WorkflowModuleInput},
};

use super::{
    dynamic_tools,
    metadata::{cache_routing_key, insert_request_metadata_u32, insert_request_metadata_value},
    token_accounting::{LastModelUsage, effective_token_estimate, request_token_usage_snapshot},
};

pub(super) struct PreparedRequest {
    pub(super) request: CanonicalModelRequest,
    pub(super) compaction: Option<HistoryCompactionReport>,
    pub(super) preparation_target: Option<ContextPreparationTarget>,
}

struct CompactedMessages {
    messages: Vec<CanonicalMessage>,
    report: Option<HistoryCompactionReport>,
    target: Option<ContextPreparationTarget>,
}

#[derive(Clone, Copy)]
struct RequestOptions<'a> {
    include_dynamic_meta_tools: bool,
    last_usage: Option<&'a LastModelUsage>,
    context_hint: Option<u32>,
    compaction_reason: Option<&'a str>,
    compaction_trigger_tokens: Option<u32>,
}

pub(super) fn request_from_state(
    input: &WorkflowModuleInput,
    host: &WorkflowModuleHostMut<'_>,
    messages: &[CanonicalMessage],
    system_instructions: &str,
    developer_instructions: Option<&str>,
    phase: &str,
    last_usage: Option<&LastModelUsage>,
) -> Result<PreparedRequest, ProcessModuleError> {
    let mut instructions = vec![InstructionBlock::new(
        InstructionKind::System,
        system_instructions,
        100,
    )];
    // Keep runtime order and priorities, including the host's continuation reason.
    // The workflow base precedes runtime blocks; phase-specific blocks follow them.
    instructions.extend(input.runtime.instructions.iter().cloned());
    request_from_state_with_instruction_blocks_and_options(
        input,
        host,
        messages,
        instructions,
        developer_instructions,
        phase,
        RequestOptions {
            include_dynamic_meta_tools: phase != "review",
            last_usage,
            context_hint: None,
            compaction_reason: Some(phase),
            compaction_trigger_tokens: None,
        },
    )
}

pub(super) fn request_from_state_with_instruction_blocks(
    input: &WorkflowModuleInput,
    host: &WorkflowModuleHostMut<'_>,
    messages: &[CanonicalMessage],
    instructions: Vec<InstructionBlock>,
    developer_instructions: Option<&str>,
    phase: &str,
    last_usage: Option<&LastModelUsage>,
    context_hint: Option<u32>,
) -> Result<PreparedRequest, ProcessModuleError> {
    request_from_state_with_instruction_blocks_and_options(
        input,
        host,
        messages,
        instructions,
        developer_instructions,
        phase,
        RequestOptions {
            include_dynamic_meta_tools: phase != "review",
            last_usage,
            context_hint,
            compaction_reason: Some(phase),
            compaction_trigger_tokens: None,
        },
    )
}

pub(super) fn pre_turn_request(
    input: &WorkflowModuleInput,
    host: &WorkflowModuleHostMut<'_>,
    prior_messages: &[CanonicalMessage],
    context_hint: Option<u32>,
) -> Result<PreparedRequest, ProcessModuleError> {
    request_from_state_with_instruction_blocks_and_options(
        input,
        host,
        prior_messages,
        input.runtime.instructions.clone(),
        None,
        "codex_loop",
        RequestOptions {
            include_dynamic_meta_tools: true,
            last_usage: None,
            context_hint,
            compaction_reason: Some("codex_pre_turn"),
            compaction_trigger_tokens: None,
        },
    )
}

pub(super) fn first_sampling_request(
    input: &WorkflowModuleInput,
    host: &WorkflowModuleHostMut<'_>,
    messages: &[CanonicalMessage],
    compaction_trigger_tokens: Option<u32>,
) -> Result<PreparedRequest, ProcessModuleError> {
    request_from_state_with_instruction_blocks_and_options(
        input,
        host,
        messages,
        input.runtime.instructions.clone(),
        None,
        "codex_loop",
        RequestOptions {
            include_dynamic_meta_tools: true,
            last_usage: None,
            context_hint: None,
            compaction_reason: None,
            compaction_trigger_tokens,
        },
    )
}

fn request_from_state_with_instruction_blocks_and_options(
    input: &WorkflowModuleInput,
    host: &WorkflowModuleHostMut<'_>,
    messages: &[CanonicalMessage],
    mut instructions: Vec<InstructionBlock>,
    developer_instructions: Option<&str>,
    phase: &str,
    options: RequestOptions<'_>,
) -> Result<PreparedRequest, ProcessModuleError> {
    if let Some(text) = super::intents::instructions(input)? {
        instructions.push(InstructionBlock::new(InstructionKind::Developer, text, 95));
    }
    let selected = visible_tools(host, input, phase)?;
    let selection_metadata = selected.metadata;
    let mut tools = selected.tools;
    if phase == "plan" {
        tools.retain(|tool| matches!(tool.safety, proteus_contracts::domain::ToolSafety::ReadOnly));
    }
    let dynamic_tools_enabled = if options.include_dynamic_meta_tools {
        let mut all_candidate_tools = dynamic_tools::all_policy_visible_tools(host, input)?;
        if phase == "plan" {
            all_candidate_tools.retain(|tool| {
                matches!(tool.safety, proteus_contracts::domain::ToolSafety::ReadOnly)
            });
        }
        dynamic_tools::has_hidden_tools(&tools, &all_candidate_tools)
    } else {
        false
    };
    if dynamic_tools_enabled {
        dynamic_tools::append_meta_tools(&mut tools, phase);
    }
    if let Some(developer_instructions) = developer_instructions {
        instructions.push(InstructionBlock::new(
            InstructionKind::Developer,
            developer_instructions,
            90,
        ));
    }
    if dynamic_tools_enabled {
        instructions.push(InstructionBlock::new(
            InstructionKind::Developer,
            dynamic_tools::INSTRUCTIONS,
            80,
        ));
    }
    let mut request = CanonicalModelRequest::new(
        input
            .runtime
            .model_ref
            .clone()
            .ok_or_else(|| ProcessModuleError::new("this workflow requires a model"))?,
        messages.to_vec(),
    )
    .with_instructions(instructions)
    .with_tools(tools)
    .with_reasoning(input.runtime.reasoning.clone())
    .with_cache(CacheHints::new(true, true).with_routing_key(cache_routing_key(input)));
    // Прокидываем потолок окна из capabilities в лимиты запроса, чтобы снимок
    // TokenUsageUpdated нёс max_input_tokens (хост-шейпер правит свою копию
    // уже после того, как module собрал снимок, поэтому делаем это здесь).
    request.limits.max_input_tokens = input.runtime.max_input_tokens;
    let (compaction, preparation_target) = if let Some(reason) = options.compaction_reason {
        let compacted = compact_messages(
            input,
            host,
            &mut request,
            reason,
            options.last_usage,
            options.context_hint,
        )?;
        request.messages = compacted.messages;
        (compacted.report, compacted.target)
    } else {
        // The request already owns its messages in the no-preparation case.
        (None, None)
    };
    // Advisory estimates travel with the preparation report, never the durable
    // canonical payload. Continuations reuse their recorded report without a new
    // provider callback or another context preparation.
    let estimates = compaction
        .as_ref()
        .and_then(|report| ModelInputEstimate::from_metadata(&report.metadata))
        .or_else(|| {
            input
                .runtime
                .continuation
                .as_ref()
                .and_then(|continuation| {
                    continuation
                        .history
                        .compactions
                        .iter()
                        .rev()
                        .find_map(|report| ModelInputEstimate::from_metadata(&report.metadata))
                })
        });
    if let Some(mut estimates) = estimates {
        estimates.retain_for(&request.messages);
        estimates.write_metadata(&mut request.metadata);
    }
    // Порог автокомпакта считает компактор (он владеет конфигом), а возвращает
    // его в отчёте. Кладём в metadata запроса, чтобы снимок взял именно его —
    // тогда метка на индикаторе контекста совпадает с реальным триггером.
    if let Some(trigger) = compaction
        .as_ref()
        .and_then(|report| report.trigger_tokens)
        .or(options.compaction_trigger_tokens)
    {
        insert_request_metadata_u32(&mut request, "compaction_trigger_tokens", trigger);
    }
    // Telemetry селектора (hidden count, saved schema tokens и т.п.) не должна
    // теряться на workflow-границе: кладём её в metadata запроса, откуда её
    // видят снимки usage и event log.
    if !selection_metadata.is_null() {
        insert_request_metadata_value(&mut request, "tool_selection", selection_metadata);
    }
    Ok(PreparedRequest {
        request,
        compaction,
        preparation_target,
    })
}

pub(super) fn execute_or_handle_tool(
    host: &WorkflowModuleHostMut<'_>,
    input: &WorkflowModuleInput,
    call: &ToolCall,
    phase: &str,
) -> Result<ToolResult, ProcessModuleError> {
    if dynamic_tools::is_meta_tool(&call.name) {
        dynamic_tools::handle_meta_tool_call(host, input, call, phase)
    } else {
        execute_tool(host, input, call)
    }
}

fn compact_messages(
    input: &WorkflowModuleInput,
    host: &WorkflowModuleHostMut<'_>,
    request: &mut CanonicalModelRequest,
    reason: &str,
    last_usage: Option<&LastModelUsage>,
    context_hint: Option<u32>,
) -> Result<CompactedMessages, ProcessModuleError> {
    ensure_not_cancelled(host)?;
    let valid_usage = last_usage.is_some_and(|usage| usage.message_count <= request.messages.len());
    let observed = context_hint.is_some() || valid_usage;
    let usage_estimate = effective_token_estimate(&request.messages, last_usage);
    let has_opaque = request
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .any(|part| matches!(part.payload, ContentPart::Opaque { .. }));
    let token_estimate = if has_opaque && observed {
        // A restored usage hint covers the checkpoint even when the generic
        // canonical estimate cannot interpret it.
        if valid_usage {
            usage_estimate
        } else {
            context_hint
        }
    } else {
        [usage_estimate, context_hint].into_iter().flatten().max()
    };
    // The caller replaces these messages with the preparation output.
    let messages = std::mem::take(&mut request.messages);
    let mut preparation_request = request.clone();
    preparation_request.messages = messages;
    let source = if observed {
        ContextTokenEstimateSource::Observed
    } else {
        ContextTokenEstimateSource::Estimated
    };
    source.write_metadata(&mut preparation_request.metadata);
    let compaction_input = ContextPreparationInput::from_request(preparation_request)
        .with_reason(reason)
        .with_token_estimate(token_estimate)
        .with_window_tokens(input.runtime.max_input_tokens);
    let input_json = to_json_string(&compaction_input)?;
    let output_json = match host.prepare_context_json(String::from(input_json)) {
        Ok(json) => json,
        Err(error) => return Err(error),
    };
    let result: ContextPreparationResult = from_json_string(output_json.as_str())?;
    let output = result.output;
    proteus_contracts::contracts::validate_context_preparation_output(&compaction_input, &output)
        .map_err(|error| ProcessModuleError::new(error.to_string()))?;
    if output.messages.is_empty() && !compaction_input.messages.is_empty() {
        return Err(ProcessModuleError::new(
            "compactor returned empty messages for non-empty history",
        ));
    }
    let report =
        HistoryCompactionReport::from_context_preparation_output(&compaction_input, &output);
    Ok(CompactedMessages {
        messages: output.messages,
        report: Some(report),
        target: result.target,
    })
}

pub(super) fn build_context(
    host: &WorkflowModuleHostMut<'_>,
    input: &WorkflowModuleInput,
) -> Result<ContextBundle, ProcessModuleError> {
    ensure_not_cancelled(host)?;
    let task_json = to_json_string(&input.task)?;
    let bundle_json = match host.build_context_json(String::from(task_json)) {
        Ok(json) => json,
        Err(error) => return Err(ProcessModuleError::new(error.message)),
    };
    from_json_string(bundle_json.as_str())
}

pub(super) fn complete_model(
    host: &WorkflowModuleHostMut<'_>,
    request: &CanonicalModelRequest,
    phase: &str,
) -> Result<CanonicalModelResponse, ProcessModuleError> {
    ensure_not_cancelled(host)?;
    let request_json = to_json_string(request)?;
    let response_json = match host.complete_model_json(String::from(request_json)) {
        Ok(json) => json,
        Err(error) => return Err(error),
    };
    let response: CanonicalModelResponse = from_json_string(response_json.as_str())?;
    emit_token_usage(host, request, response.usage.clone(), phase)?;
    Ok(response)
}

pub(super) struct SelectedTools {
    pub(super) tools: Vec<ToolSpec>,
    pub(super) metadata: serde_json::Value,
}

fn visible_tools(
    host: &WorkflowModuleHostMut<'_>,
    input: &WorkflowModuleInput,
    phase: &str,
) -> Result<SelectedTools, ProcessModuleError> {
    ensure_not_cancelled(host)?;
    let request = ToolSelectionRequest::new(input.task.clone())
        .with_reason("before_model_request")
        .with_phase(phase);
    let request_json = to_json_string(&request)?;
    let tools_json = match host.select_tools_json(String::from(request_json)) {
        Ok(json) => json,
        Err(error) => return Err(ProcessModuleError::new(error.message)),
    };
    let output: proteus_contracts::contracts::ToolSelectionOutput =
        from_json_string(tools_json.as_str())?;
    Ok(SelectedTools {
        tools: output.tools,
        metadata: output.metadata,
    })
}

/// Выполняет батч tool calls одного ответа модели. Workflow-owned meta-tools
/// динамической экспозиции обрабатываются локально; зарегистрированные tools
/// (включая facade-tool `task`) уходят в host batch API, где core применяет
/// registry/policy/orchestrator и выбирает допустимую concurrency.
pub(super) fn execute_tools(
    host: &WorkflowModuleHostMut<'_>,
    input: &WorkflowModuleInput,
    calls: &[ToolCall],
    phase: &str,
) -> Result<Vec<ToolResult>, ProcessModuleError> {
    if calls.len() <= 1
        || calls
            .iter()
            .any(|call| dynamic_tools::is_meta_tool(&call.name))
    {
        return calls
            .iter()
            .map(|call| execute_or_handle_tool(host, input, call, phase))
            .collect();
    }
    ensure_not_cancelled(host)?;
    let task_json = to_json_string(&input.task)?;
    let calls_json = to_json_string(&calls)?;
    let results_json =
        match host.execute_tools_json(String::from(task_json), String::from(calls_json)) {
            Ok(json) => json,
            Err(error) => return Err(ProcessModuleError::new(error.message)),
        };
    from_json_string(results_json.as_str())
}

pub(super) fn execute_tool(
    host: &WorkflowModuleHostMut<'_>,
    input: &WorkflowModuleInput,
    call: &ToolCall,
) -> Result<ToolResult, ProcessModuleError> {
    ensure_not_cancelled(host)?;
    let task_json = to_json_string(&input.task)?;
    let call_json = to_json_string(call)?;
    let result_json = match host.execute_tool_json(String::from(task_json), String::from(call_json))
    {
        Ok(json) => json,
        Err(error) => return Err(ProcessModuleError::new(error.message)),
    };
    from_json_string(result_json.as_str())
}

pub(super) fn ensure_not_cancelled(
    host: &WorkflowModuleHostMut<'_>,
) -> Result<(), ProcessModuleError> {
    match host.is_cancelled() {
        Ok(false) => Ok(()),
        Ok(true) => Err(ProcessModuleError::new("turn canceled by client")),
        Err(error) => Err(ProcessModuleError::new(error.message)),
    }
}

pub(super) fn emit_event(
    host: &WorkflowModuleHostMut<'_>,
    event: &Event,
) -> Result<(), ProcessModuleError> {
    let event_json = to_json_string(event)?;
    match host.emit_event_json(String::from(event_json)) {
        Ok(()) => Ok(()),
        Err(error) => Err(ProcessModuleError::new(error.message)),
    }
}

pub(super) fn to_json_string<T: serde::Serialize>(value: &T) -> Result<String, ProcessModuleError> {
    serde_json::to_string(value).map_err(|error| ProcessModuleError::new(error.to_string()))
}

pub(super) fn from_json_string<T: serde::de::DeserializeOwned>(
    value: &str,
) -> Result<T, ProcessModuleError> {
    serde_json::from_str(value).map_err(|error| ProcessModuleError::new(error.to_string()))
}

pub(super) fn emit_token_usage(
    host: &WorkflowModuleHostMut<'_>,
    request: &CanonicalModelRequest,
    actual: Option<TokenUsage>,
    phase: &str,
) -> Result<(), ProcessModuleError> {
    let usage = request_token_usage_snapshot(request, actual, phase);
    emit_event(host, &Event::TokenUsageUpdated { usage })
}
