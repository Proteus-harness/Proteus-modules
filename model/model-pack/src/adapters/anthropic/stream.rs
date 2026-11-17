use serde_json::Value;

use super::sanitize::{DsmlStreamFilter, sanitize_provider_text};
use super::usage::AnthropicUsage;
use crate::{
    domain::ToolCall,
    model_standard::{
        CanonicalMessage, CanonicalModelResponse, ContentPart, FinishReason, MessageRole,
        ModelFailure, ModelStreamEvent,
    },
};

/// Stateful аккумулятор для Anthropic SSE-потока: копит text parts и
/// tool_use блоки по мере прихода, на `message_stop` отдаёт финальный
/// CanonicalModelResponse или typed context-window failure.
pub(super) struct AnthropicStreamState {
    http_status: reqwest::StatusCode,
    message_id: crate::domain::MessageId,
    blocks: Vec<AnthropicBlock>,
    usage: AnthropicUsage,
    stop_reason: Option<String>,
    // Anthropic SSE референсит блоки по index, так что нужен index → block mapping.
}

impl Default for AnthropicStreamState {
    fn default() -> Self {
        Self {
            http_status: reqwest::StatusCode::OK,
            message_id: crate::domain::new_message_id(),
            blocks: Vec::new(),
            usage: AnthropicUsage::default(),
            stop_reason: None,
        }
    }
}

#[derive(Debug, Clone)]
enum AnthropicBlock {
    Thinking {
        text: String,
        signature: Option<String>,
    },
    Opaque {
        data: Value,
    },
    Text {
        text: String,
        filter: DsmlStreamFilter,
    },
    ToolUse {
        id: String,
        name: String,
        initial_input: Value,
        input_json: String,
    },
}

impl AnthropicStreamState {
    pub(super) fn new(http_status: reqwest::StatusCode) -> Self {
        Self {
            http_status,
            ..Self::default()
        }
    }

    pub(super) fn translate(&mut self, event_type: &str, data: &str) -> Vec<ModelStreamEvent> {
        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            return vec![ModelStreamEvent::Error {
                failure: ModelFailure::other("malformed Anthropic SSE JSON"),
            }];
        };
        match event_type {
            "message_start" => {
                if let Some(usage) = parsed
                    .get("message")
                    .and_then(|message| message.get("usage"))
                {
                    self.usage.merge(usage);
                }
                Vec::new()
            }
            "content_block_start" => {
                let index = parsed.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = parsed.get("content_block");
                let block_type = block
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let new_block = match block_type {
                    "thinking" => Some(AnthropicBlock::Thinking {
                        text: block
                            .and_then(|b| b.get("thinking"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        signature: block
                            .and_then(|b| b.get("signature"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }),
                    "redacted_thinking" => {
                        let Some(block) = block
                            .filter(|block| block.get("data").and_then(Value::as_str).is_some())
                        else {
                            return vec![ModelStreamEvent::Error {
                                failure: ModelFailure::other(
                                    "redacted_thinking missing opaque data",
                                ),
                            }];
                        };
                        Some(AnthropicBlock::Opaque {
                            data: block.clone(),
                        })
                    }
                    "text" => Some(AnthropicBlock::Text {
                        text: String::new(),
                        filter: DsmlStreamFilter::default(),
                    }),
                    "tool_use" => {
                        let id = block
                            .and_then(|b| b.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let name = block
                            .and_then(|b| b.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        Some(AnthropicBlock::ToolUse {
                            id,
                            name,
                            // SSE без JSON-байтов сохраняет input стартового блока.
                            initial_input: block
                                .and_then(|b| b.get("input"))
                                .cloned()
                                .unwrap_or(Value::Null),
                            input_json: String::new(),
                        })
                    }
                    _ => None,
                };
                if let Some(block) = new_block {
                    if self.blocks.len() <= index {
                        self.blocks.resize(
                            index + 1,
                            AnthropicBlock::Text {
                                text: String::new(),
                                filter: DsmlStreamFilter::default(),
                            },
                        );
                    }
                    self.blocks[index] = block;
                }
                if block_type == "text" {
                    let text = block
                        .and_then(|b| b.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !text.is_empty() {
                        return self.append_text(index, text);
                    }
                }
                Vec::new()
            }
            "content_block_delta" => {
                let index = parsed.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = parsed.get("delta");
                let delta_type = delta
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match delta_type {
                    "thinking_delta" => {
                        let text = delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        if let Some(AnthropicBlock::Thinking { text: buf, .. }) =
                            self.blocks.get_mut(index)
                        {
                            buf.push_str(&text);
                        }
                        if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![ModelStreamEvent::ReasoningSummaryDelta { text }]
                        }
                    }
                    "signature_delta" => {
                        let partial = delta
                            .and_then(|d| d.get("signature"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(AnthropicBlock::Thinking { signature, .. }) =
                            self.blocks.get_mut(index)
                        {
                            match signature {
                                Some(signature) => signature.push_str(partial),
                                None if !partial.is_empty() => {
                                    *signature = Some(partial.to_owned());
                                }
                                None => {}
                            }
                        }
                        Vec::new()
                    }
                    "text_delta" => {
                        let text = delta
                            .and_then(|d| d.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.append_text(index, text)
                    }
                    "input_json_delta" => {
                        let partial = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let call_id = if let Some(AnthropicBlock::ToolUse {
                            id,
                            initial_input,
                            input_json,
                            ..
                        }) = self.blocks.get_mut(index)
                        {
                            if !partial.is_empty() {
                                // JSON-дельты заменяют input, а не дополняют его.
                                *initial_input = Value::Null;
                            }
                            input_json.push_str(partial);
                            id.clone()
                        } else {
                            return Vec::new();
                        };
                        if partial.is_empty() {
                            Vec::new()
                        } else {
                            vec![ModelStreamEvent::ToolCallDelta {
                                call_id,
                                name: None,
                                args_delta: partial.to_owned(),
                            }]
                        }
                    }
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                let index = parsed.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let Some(AnthropicBlock::Text { text, filter }) = self.blocks.get_mut(index) else {
                    return Vec::new();
                };
                let pending = filter.finish();
                text.push_str(&pending);
                self.text_delta(pending)
            }
            "message_delta" => {
                if let Some(stop) = parsed
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(stop.to_owned());
                }
                if let Some(usage) = parsed.get("usage") {
                    self.usage.merge(usage);
                }
                Vec::new()
            }
            "message_stop" => {
                vec![self.finalise()]
            }
            "error" => {
                vec![ModelStreamEvent::Error {
                    failure: super::errors::failure_from_error_value(
                        parsed.get("error"),
                        None,
                        "unknown anthropic error",
                    ),
                }]
            }
            _ => Vec::new(),
        }
    }

    // Start/delta/stop share the same block-owned filter and text buffer.
    fn append_text(&mut self, index: usize, text: &str) -> Vec<ModelStreamEvent> {
        let Some(AnthropicBlock::Text { text: buf, filter }) = self.blocks.get_mut(index) else {
            return Vec::new();
        };
        let text = filter.filter(text);
        buf.push_str(&text);
        self.text_delta(text)
    }

    fn text_delta(&self, text: String) -> Vec<ModelStreamEvent> {
        if text.is_empty() {
            Vec::new()
        } else {
            vec![ModelStreamEvent::TextDelta {
                message_id: self.message_id,
                phase: None,
                text,
            }]
        }
    }

    fn finalise(&mut self) -> ModelStreamEvent {
        if let Some(failure) = super::errors::context_window_stop_failure(
            self.http_status,
            self.stop_reason.as_deref(),
        ) {
            return ModelStreamEvent::Error { failure };
        }

        let mut parts = Vec::new();
        let mut tool_calls = Vec::new();
        let accept_tool_calls = self.stop_reason.as_deref() == Some("tool_use");
        for block in self.blocks.drain(..) {
            match block {
                AnthropicBlock::Opaque { data } => parts.push(ContentPart::Opaque {
                    format: super::REDACTED_THINKING_FORMAT.into(),
                    data,
                }),
                AnthropicBlock::Thinking { text, signature } => {
                    if !text.trim().is_empty() || signature.is_some() {
                        parts.push(ContentPart::Reasoning { text, signature });
                    }
                }
                AnthropicBlock::Text {
                    mut text,
                    mut filter,
                } => {
                    // Tolerate a missing block_stop without moving its suffix to another block.
                    text.push_str(&filter.finish());
                    let text = sanitize_provider_text(&text);
                    if text.is_empty() {
                        continue;
                    }
                    parts.push(ContentPart::Text { text });
                }
                AnthropicBlock::ToolUse {
                    id,
                    name,
                    initial_input,
                    input_json,
                } => {
                    if !accept_tool_calls {
                        continue;
                    }
                    let args = if input_json.is_empty() {
                        initial_input
                    } else {
                        match serde_json::from_str(&input_json) {
                            Ok(args) => args,
                            Err(error) => {
                                return ModelStreamEvent::Error {
                                    failure: ModelFailure::other(format!(
                                        "invalid Anthropic tool arguments: {error}"
                                    )),
                                };
                            }
                        }
                    };
                    let call = ToolCall::new(id, name, args);
                    parts.push(ContentPart::ToolCall { call: call.clone() });
                    tool_calls.push(call);
                }
            }
        }

        let finish_reason = match self.stop_reason.as_deref() {
            Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
            Some("max_tokens") | Some("model_context_window_exceeded") => FinishReason::Length,
            Some("tool_use") if !tool_calls.is_empty() => FinishReason::ToolCalls,
            _ => FinishReason::Unknown,
        };
        if finish_reason != FinishReason::ToolCalls {
            tool_calls.clear();
            parts.retain(|part| !matches!(part, ContentPart::ToolCall { .. }));
        }
        let mut message = CanonicalMessage::new(MessageRole::Assistant, parts);
        message.id = self.message_id;
        let mut resp = CanonicalModelResponse::new(message, tool_calls, finish_reason);
        if let Some(u) = std::mem::take(&mut self.usage).into_token_usage() {
            resp = resp.with_usage(u);
        }
        ModelStreamEvent::Response { response: resp }
    }
}

#[cfg(test)]
mod start_tests;

#[cfg(test)]
mod block_tests;
