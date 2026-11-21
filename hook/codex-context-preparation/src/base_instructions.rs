//! Public Codex model-specific templates pinned to rust-v0.167.0.
use std::borrow::Cow;

use proteus_contracts::{
    contracts::{HookEvent, HookInput, HookResponse, ModelCallOrigin},
    domain::{ToolSpec, ToolSurface},
    model_standard::{InstructionBlock, InstructionKind, ModelOperation},
    process_module::{HookModule, HookModuleHost, ProcessModuleError},
};

pub struct CodexBaseInstructions;

fn template(model: &str) -> &'static str {
    match model {
        "gpt-6.1-sol" => include_str!("upstream/0.167.0/gpt-6.1-sol.md"),
        "gpt-6.1-terra" => include_str!("upstream/0.167.0/gpt-6.1-terra.md"),
        "gpt-5.6-luna" => include_str!("upstream/0.167.0/gpt-5.6-luna.md"),
        _ => include_str!("upstream/0.167.0/default.md"),
    }
}

/// Translate only the pinned legacy invocation example to the actual binding.
/// The source templates and their provenance remain unchanged.
fn model_instructions(model: &str, tools: &[ToolSpec]) -> Cow<'static, str> {
    let source = template(model);
    let Some(legacy) = source.lines().find(|line| {
        line.starts_with("- Use the `apply_patch` tool to edit files (")
            && line.contains("{\"command\":")
    }) else {
        return Cow::Borrowed(source);
    };
    let Some(tool) = tools.iter().find(|tool| tool.name == "apply_patch") else {
        return Cow::Borrowed(source);
    };
    let (instruction, _) = legacy
        .split_once(": {\"command\":")
        .expect("selected legacy patch invocation");
    let example = "*** Begin Patch\n*** Add File: example.txt\n+example\n*** End Patch";
    let guidance = match &tool.surface {
        ToolSurface::Function { .. } | ToolSurface::WorkflowFunction { .. }
            if tool.input_schema["properties"]["patch"]["type"] == "string" =>
        {
            format!(
                "{instruction}. Pass the patch in the `patch` string argument. Arguments: {}",
                serde_json::json!({"patch": example})
            )
        }
        ToolSurface::Freeform { .. }
            if tool.input_schema["properties"]["input"]["type"] == "string" =>
        {
            format!(
                "{instruction}. Its freeform input is raw patch text, without a JSON wrapper:\n{example}"
            )
        }
        _ => return Cow::Borrowed(source),
    };
    Cow::Owned(source.replacen(legacy, &guidance, 1))
}

impl HookModule for CodexBaseInstructions {
    fn invoke_json(
        &self,
        input: String,
        host: &mut dyn HookModuleHost,
    ) -> Result<String, ProcessModuleError> {
        let input: HookInput = serde_json::from_str(&input).map_err(super::error)?;
        let estimates = match &input.event {
            HookEvent::BeforeModel { request, .. }
                if request.operation == ModelOperation::Compact =>
            {
                crate::input_estimates::for_messages(&request.model, &request.messages, host)?
            }
            _ => Default::default(),
        };
        serde_json::to_string(&prepare_with_estimates(input.event, &estimates))
            .map_err(super::error)
    }
}

#[cfg(test)]
fn prepare(event: HookEvent) -> HookResponse {
    prepare_with_estimates(event, &Default::default())
}

fn prepare_with_estimates(
    event: HookEvent,
    estimates: &proteus_contracts::model_standard::input_estimates::ModelInputEstimate,
) -> HookResponse {
    match event {
        HookEvent::BeforeModel {
            origin,
            mut request,
        } if origin == ModelCallOrigin::Direct || request.operation == ModelOperation::Compact => {
            request.instructions.insert(
                0,
                InstructionBlock::new(
                    InstructionKind::Developer,
                    model_instructions(&request.model.model, &request.tools),
                    100,
                ),
            );
            if request.operation == ModelOperation::Compact {
                // The initial compaction hook sees the pending request before
                // this configured transform. Recheck with the actual base
                // instructions and the final host-shaped context limit.
                crate::remote_window::trim_trailing_tool_outputs_with_estimates(
                    &mut request.messages,
                    &request.instructions,
                    request.limits.max_input_tokens,
                    estimates,
                );
            }
            HookResponse::ModelContext {
                messages: request.messages,
                instructions: request.instructions,
            }
        }
        _ => HookResponse::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auxiliary_compaction_uses_the_same_model_instructions_as_direct_completion() {
        use proteus_contracts::{domain::ModelRef, model_standard::CanonicalModelRequest};
        let request = CanonicalModelRequest::new(ModelRef::new("fixture", "gpt-6.1-sol"), vec![]);
        let direct = prepare(HookEvent::BeforeModel {
            origin: ModelCallOrigin::Direct,
            request: request.clone(),
        });
        let auxiliary = prepare(HookEvent::BeforeModel {
            origin: ModelCallOrigin::Auxiliary,
            request: request.with_operation(ModelOperation::Compact),
        });
        assert_eq!(direct, auxiliary);
    }

    #[test]
    fn compaction_preflight_accounts_for_the_injected_base_instructions() {
        use proteus_contracts::{
            domain::{ModelRef, ToolResult},
            model_standard::{CanonicalMessage, CanonicalModelRequest, ContentPart, MessageRole},
        };
        let result = CanonicalMessage::new(
            MessageRole::Tool,
            vec![ContentPart::ToolResult {
                result: ToolResult::ok("fixture-call".into(), "x".repeat(2000)),
            }],
        );
        let mut request = CanonicalModelRequest::new(
            ModelRef::new("fixture", "gpt-6.1-sol"),
            vec![result.clone()],
        )
        .with_operation(ModelOperation::Compact);
        let base = crate::budget::estimate_text_tokens(template("gpt-6.1-sol"));
        request.limits.max_input_tokens = Some(u32::try_from(base + 50).unwrap());
        assert!(
            crate::remote_history::estimate_messages(&request.messages)
                < request.limits.max_input_tokens.unwrap()
        );
        let HookResponse::ModelContext {
            mut messages,
            instructions,
        } = prepare(HookEvent::BeforeModel {
            origin: ModelCallOrigin::Auxiliary,
            request,
        })
        else {
            panic!("expected model context")
        };
        assert_ne!(messages[0], result);
        assert!(
            matches!(&messages[0].parts[0].payload, ContentPart::ToolResult {result} if result.output=="Output exceeded the available model context and was truncated")
        );
        assert!(crate::remote_history::estimate_messages(&messages) as usize + base <= base + 50);
        let once = messages.clone();
        crate::remote_window::trim_trailing_tool_outputs(
            &mut messages,
            &instructions,
            Some((base + 50) as u32),
        );
        assert_eq!(
            messages, once,
            "a second preflight must be a no-op when the final request fits"
        );
    }

    #[test]
    fn compaction_keeps_fitting_tool_outputs_and_plain_auxiliary_requests_unchanged() {
        use proteus_contracts::{
            domain::{ModelRef, ToolResult},
            model_standard::{CanonicalMessage, CanonicalModelRequest, ContentPart, MessageRole},
        };
        let result = CanonicalMessage::new(
            MessageRole::Tool,
            vec![ContentPart::ToolResult {
                result: ToolResult::ok("fixture-call".into(), "short"),
            }],
        );
        let mut request = CanonicalModelRequest::new(
            ModelRef::new("fixture", "gpt-6.1-sol"),
            vec![result.clone()],
        );
        request.limits.max_input_tokens = Some(100_000);
        assert_eq!(
            prepare(HookEvent::BeforeModel {
                origin: ModelCallOrigin::Auxiliary,
                request: request.clone(),
            }),
            HookResponse::Continue
        );
        let HookResponse::ModelContext { messages, .. } = prepare(HookEvent::BeforeModel {
            origin: ModelCallOrigin::Auxiliary,
            request: request.with_operation(ModelOperation::Compact),
        }) else {
            panic!("expected model context")
        };
        assert_eq!(messages, vec![result]);
    }

    #[test]
    fn legacy_patch_example_follows_the_actual_function_or_freeform_binding() {
        use proteus_contracts::{
            domain::{ModelRef, ToolCall, ToolCallSurface, ToolSafety, validate_tool_call_args},
            model_standard::CanonicalModelRequest,
        };
        let function = ToolSpec::new(
            "apply_patch",
            "Patch files",
            serde_json::json!({"type":"object", "properties":{"patch":{"type":"string"}},
                "required":["patch"], "additionalProperties":false}),
            ToolSafety::WritesFiles,
        );
        let mut freeform = function.clone();
        freeform.input_schema = serde_json::json!({"type":"object", "properties":{"input":{"type":"string"}},
            "required":["input"], "additionalProperties":false});
        freeform.surface = ToolSurface::freeform_lark("start: /[\\s\\S]+/");
        for tool in [function, freeform] {
            let request =
                CanonicalModelRequest::new(ModelRef::new("fake", "deepseek-v4.1-flash"), vec![])
                    .with_tools(vec![tool.clone()]);
            let HookResponse::ModelContext { instructions, .. } = prepare(HookEvent::BeforeModel {
                origin: ModelCallOrigin::Direct,
                request,
            }) else {
                panic!("expected model context")
            };
            let text = &instructions[0].text;
            assert!(!text.contains("{\"command\":[\"apply_patch\""));
            let call = match tool.surface {
                ToolSurface::Freeform { .. } => {
                    let raw = text
                        .split_once("without a JSON wrapper:\n")
                        .unwrap()
                        .1
                        .split("\n\n")
                        .next()
                        .unwrap();
                    assert!(raw.starts_with("*** Begin Patch\n"));
                    assert!(raw.ends_with("*** End Patch"));
                    ToolCall::new("test", "apply_patch", serde_json::json!({"input":raw}))
                        .with_surface(ToolCallSurface::Freeform)
                }
                _ => {
                    let json = text
                        .lines()
                        .find_map(|line| line.split_once("Arguments: ").map(|(_, value)| value))
                        .unwrap();
                    let arguments: serde_json::Value = serde_json::from_str(json).unwrap();
                    assert!(
                        arguments["patch"]
                            .as_str()
                            .unwrap()
                            .starts_with("*** Begin Patch\n")
                    );
                    ToolCall::new("test", "apply_patch", arguments)
                }
            };
            assert_eq!(validate_tool_call_args(&call, &tool), None);
        }
    }

    #[test]
    fn template_is_preserved_without_a_supported_patch_binding_or_legacy_example() {
        use proteus_contracts::domain::ToolSafety;
        let unknown = ToolSpec::new(
            "apply_patch",
            "Custom binding",
            serde_json::json!({}),
            ToolSafety::WritesFiles,
        );
        assert_eq!(
            model_instructions("unknown", &[unknown]),
            template("unknown")
        );
        let tool = ToolSpec::new(
            "apply_patch",
            "Patch files",
            serde_json::json!({"properties":{"patch":{"type":"string"}}}),
            ToolSafety::WritesFiles,
        );
        assert_eq!(
            model_instructions("gpt-6.1-sol", &[tool]),
            template("gpt-6.1-sol")
        );
        assert_eq!(model_instructions("unknown", &[]), template("unknown"));
    }

    #[test]
    fn selection_preserves_user_messages_and_custom_instructions() {
        use proteus_contracts::{
            domain::ModelRef,
            model_standard::{CanonicalMessage, CanonicalModelRequest, MessageRole},
        };
        for model in [
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-5.6-luna",
            "unlisted-model",
        ] {
            let custom =
                InstructionBlock::new(InstructionKind::UserPreference, "test preference", 150);
            let request = CanonicalModelRequest::new(
                ModelRef::new("fake", model),
                vec![CanonicalMessage::text(MessageRole::User, "test task")],
            )
            .with_instructions(vec![custom.clone()]);
            let expected_messages = request.messages.clone();
            let response = prepare(HookEvent::BeforeModel {
                origin: ModelCallOrigin::Direct,
                request,
            });
            let HookResponse::ModelContext {
                messages,
                instructions,
            } = response
            else {
                panic!("expected model context")
            };
            assert_eq!(messages, expected_messages);
            assert_eq!(instructions.len(), 2);
            assert_eq!(instructions[0].text, template(model));
            assert_eq!(instructions[0].kind, InstructionKind::Developer);
            assert_eq!(instructions[1], custom);
        }
        let request = CanonicalModelRequest::new(ModelRef::new("fake", "gpt-6.1-sol"), vec![]);
        assert_eq!(
            prepare(HookEvent::BeforeModel {
                origin: ModelCallOrigin::Auxiliary,
                request
            }),
            HookResponse::Continue
        );
    }
}
