use super::*;
use crate::{
    domain::{ToolCallSurface, new_message_id},
    model_standard::{CanonicalModelResponse, FinishReason, MessagePhase},
};
use serde_json::json;
use std::collections::HashMap;

fn terminal(output: Value) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": "response-id", "status": "completed", "output": output,
            "end_turn": false, "extra": {"preserve": ["provider", "metadata"]},
            "usage": {"input_tokens": 17, "output_tokens": 9,
                "input_tokens_details": {"cached_tokens": 5},
                "output_tokens_details": {"reasoning_tokens": 3}},
        },
    })
}

fn response(events: &[ModelStreamEvent]) -> &CanonicalModelResponse {
    let [ModelStreamEvent::Response { response }] = events else {
        panic!("expected one terminal response: {events:?}");
    };
    response
}

#[test]
fn authoritative_terminal_or_completed_items_do_not_build_streamed_items() {
    for item in [
        json!({"id":"item", "type":"message", "role":"assistant", "phase":"final_answer",
            "content":[{"type":"output_text", "text":"authoritative"}]}),
        json!({"id":"item", "type":"message", "role":"assistant", "phase":"final_answer",
            "content":[{"type":"refusal", "refusal":"Cannot comply."}]}),
        json!({"id":"item", "type":"function_call", "call_id":"call", "name":"shell",
            "arguments":r#"{ "z": -0.0, "a": 1 }"#}),
        json!({"id":"item", "type":"custom_tool_call", "call_id":"call", "name":"apply_patch",
            "input":"*** Begin Patch\n*** End Patch"}),
    ] {
        for completed_fallback in [false, true] {
            let parsed = terminal(if completed_fallback {
                json!([])
            } else {
                json!([item])
            });
            let mut expected = parsed["response"].clone();
            expected["output"] = json!([item]);
            let fallback = if completed_fallback {
                vec![item.clone()]
            } else {
                vec![json!({"id":"ignored", "type":"message", "role":"assistant",
                    "content":[{"type":"output_text", "text":"stale fallback"}]})]
            };
            let id = new_message_id();
            let ids = HashMap::from([("item".to_owned(), id)]);
            let events = finalize_completed_event(
                parsed,
                &fallback,
                || panic!("authoritative output must not reconstruct streamed items"),
                &ids,
            );
            let response = response(&events);
            assert_eq!(response.provider_metadata.to_string(), expected.to_string());
            assert_eq!(response.messages.len(), 1);
            assert_eq!(response.messages[0].id, id);
            assert_eq!(response.end_turn, Some(false));
            let usage = response.usage.as_ref().unwrap();
            assert_eq!((usage.input_tokens, usage.output_tokens), (17, 9));
            assert_eq!(usage.cached_input_tokens, Some(5));
            assert_eq!(usage.reasoning_output_tokens, Some(3));
            match item["type"].as_str().unwrap() {
                "message" => {
                    assert_eq!(response.finish_reason, FinishReason::Stop);
                    assert_eq!(response.messages[0].phase, Some(MessagePhase::FinalAnswer));
                    if item["content"][0]["type"] == "refusal" {
                        assert_eq!(response.messages[0].display_text(), "Cannot comply.");
                        assert_eq!(
                            response.messages[0].metadata["refusal_part_indices"],
                            json!([0])
                        );
                    } else {
                        assert_eq!(response.messages[0].display_text(), "authoritative");
                    }
                }
                kind => {
                    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
                    let [call] = response.tool_calls.as_slice() else {
                        panic!("one tool call");
                    };
                    assert_eq!(call.id, "call");
                    assert_eq!(call.name, item["name"].as_str().unwrap());
                    if kind == "function_call" {
                        assert_eq!(call.surface, ToolCallSurface::Function);
                        assert_eq!(call.raw_arguments.as_deref(), item["arguments"].as_str());
                    } else {
                        assert_eq!(call.surface, ToolCallSurface::Freeform);
                        assert_eq!(call.args["input"], item["input"]);
                    }
                }
            }
        }
    }
}

#[test]
fn missing_or_nonemittable_output_builds_streamed_items_once() {
    for output in [
        None,
        Some(Value::Null),
        Some(json!({"not": "an array"})),
        Some(json!([])),
        Some(json!([{"id":"empty", "type":"message", "role":"assistant", "content":[]}])),
        Some(json!([{"id":"blank", "type":"message", "role":"assistant",
            "content":[{"type":"output_text", "text":" \n\t"}]}])),
        Some(json!([{"id":"blank", "type":"message", "role":"assistant",
            "content":[{"type":"refusal", "refusal":" \n\t"}]}])),
        Some(json!([{"id":"reasoning", "type":"reasoning", "summary":[],
            "encrypted_content":"opaque"}])),
    ] {
        let mut parsed = terminal(Value::Null);
        if let Some(output) = output {
            parsed["response"]["output"] = output;
        } else {
            parsed["response"].as_object_mut().unwrap().remove("output");
        }
        let recovered = json!({"id":"streamed", "type":"message", "role":"assistant",
            "phase":"commentary", "content":[{"type":"output_text", "text":"recovered"}]});
        let mut expected = parsed["response"].clone();
        let mut expected_output = expected
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        expected_output.push(recovered.clone());
        expected["output"] = Value::Array(expected_output);
        let id = new_message_id();
        let ids = HashMap::from([("streamed".to_owned(), id)]);
        let mut calls = 0;
        let events = finalize_completed_event(
            parsed,
            &[],
            || {
                calls += 1;
                vec![recovered]
            },
            &ids,
        );
        assert_eq!(calls, 1);
        let response = response(&events);
        assert_eq!(response.provider_metadata.to_string(), expected.to_string());
        let message = response.messages.last().unwrap();
        assert_eq!(message.id, id);
        assert_eq!(message.phase, Some(MessagePhase::Commentary));
        assert_eq!(message.display_text(), "recovered");
    }
}

#[test]
fn streamed_repair_preserves_first_same_id_replacement_and_append_order() {
    let blank = json!({"id":"same", "type":"message", "role":"assistant", "content":[]});
    let reasoning =
        json!({"id":"same", "type":"reasoning", "summary":[], "encrypted_content":"opaque"});
    let unknown = json!({"type":"provider-specific", "untouched":true});
    let first = json!({"id":"same", "type":"message", "role":"assistant",
        "content":[{"type":"output_text", "text":"first"}]});
    let last = json!({"id":"same", "type":"message", "role":"assistant", "phase":"final_answer",
        "content":[{"type":"output_text", "text":"last"}]});
    let appended = json!({"id":"new", "type":"message", "role":"assistant",
        "content":[{"type":"refusal", "refusal":"No."}]});
    for completed_fallback in [false, true] {
        let initial = vec![blank.clone(), reasoning.clone(), unknown.clone()];
        let parsed = terminal(if completed_fallback {
            json!([])
        } else {
            json!(initial)
        });
        let mut expected = parsed["response"].clone();
        expected["output"] = json!([last, reasoning, unknown, appended]);
        let fallback = if completed_fallback {
            initial
        } else {
            Vec::new()
        };
        let mut calls = 0;
        let events = finalize_completed_event(
            parsed,
            &fallback,
            || {
                calls += 1;
                vec![first.clone(), appended.clone(), last.clone()]
            },
            &HashMap::new(),
        );
        assert_eq!(calls, 1);
        let response = response(&events);
        assert_eq!(response.provider_metadata.to_string(), expected.to_string());
        assert_eq!(response.messages.len(), 3);
        assert_eq!(response.messages[0].display_text(), "last");
        assert_eq!(response.messages[0].phase, Some(MessagePhase::FinalAnswer));
        assert_eq!(response.messages[2].display_text(), "No.");
        assert_eq!(
            response.messages[2].metadata["refusal_part_indices"],
            json!([0])
        );
    }
}

#[test]
fn lazy_reconstruction_does_not_change_terminal_error_precedence() {
    let invalid = json!({"type":"message", "role":"assistant", "phase":"invalid",
        "content":[{"type":"output_text", "text":"visible"}]});
    for (fields, expected) in [
        (json!({}), "unknown OpenAI message phase 'invalid'"),
        (
            json!({"status":"incomplete", "incomplete_details":{"reason":"max_output_tokens"}}),
            "Incomplete response returned, reason: max_output_tokens",
        ),
        (
            json!({"error":"provider error", "status":"incomplete"}),
            "OpenAI API error: \"provider error\"",
        ),
    ] {
        let mut parsed = terminal(json!([invalid]));
        parsed["response"]
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let events = finalize_completed_event(
            parsed,
            &[],
            || panic!("the unchanged emittable guard wins before parsing errors"),
            &HashMap::new(),
        );
        let [ModelStreamEvent::Error { failure }] = events.as_slice() else {
            panic!("terminal error");
        };
        assert_eq!(
            failure.message,
            format!("failed to parse final response: {expected}")
        );
    }
    // An API error still does not bypass repair of nonemittable output.
    let mut parsed = terminal(json!([]));
    parsed["response"]["error"] = json!("provider error");
    let mut calls = 0;
    let events = finalize_completed_event(
        parsed,
        &[],
        || {
            calls += 1;
            Vec::new()
        },
        &HashMap::new(),
    );
    assert_eq!(calls, 1);
    assert!(
        matches!(events.as_slice(), [ModelStreamEvent::Error { failure }]
        if failure.message == "failed to parse final response: OpenAI API error: \"provider error\"")
    );
}

#[test]
fn repeated_terminal_repair_keeps_stream_state_and_ordered_parts() {
    use super::super::stream_state::OpenAiStreamState;
    let mut state = OpenAiStreamState::default();
    state.translate(&json!({"type":"response.output_item.added", "output_index":2,
        "item":{"id":"later", "type":"message", "role":"assistant", "phase":"commentary", "content":[]}
    }).to_string());
    let later = state.translate(
        &json!({"type":"response.refusal.delta", "output_index":2,
        "item_id":"later", "content_index":1, "delta":"No."})
        .to_string(),
    );
    let [
        ModelStreamEvent::TextDelta {
            message_id: later_id,
            ..
        },
    ] = later.as_slice()
    else {
        panic!("later delta");
    };
    let earlier = state.translate(
        &json!({"type":"response.output_text.delta", "output_index":0,
        "item_id":"earlier", "content_index":0, "delta":"first"})
        .to_string(),
    );
    let [
        ModelStreamEvent::TextDelta {
            message_id: earlier_id,
            ..
        },
    ] = earlier.as_slice()
    else {
        panic!("earlier delta");
    };
    state.translate(
        &json!({"type":"response.output_text.delta", "output_index":2,
        "item_id":"later", "content_index":0, "delta":"before refusal"})
        .to_string(),
    );
    let first = state.translate(&terminal(json!([])).to_string());
    let second = state.translate(&terminal(json!([])).to_string());
    let first = response(&first);
    let second = response(&second);
    assert_eq!(
        first.provider_metadata.to_string(),
        second.provider_metadata.to_string()
    );
    for response in [first, second] {
        assert_eq!(response.messages.len(), 2);
        assert_eq!(response.messages[0].id, *earlier_id);
        assert_eq!(response.messages[0].display_text(), "first");
        assert_eq!(response.messages[1].id, *later_id);
        assert_eq!(response.messages[1].phase, Some(MessagePhase::Commentary));
        assert_eq!(response.messages[1].display_text(), "before refusal\nNo.");
        assert_eq!(
            response.messages[1].metadata["refusal_part_indices"],
            json!([1])
        );
    }
}
