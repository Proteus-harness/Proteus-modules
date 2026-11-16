//! Public Codex model-specific templates pinned to rust-v0.167.0.
use std::borrow::Cow;

use proteus_contracts::{
    contracts::{HookEvent, HookInput, HookResponse, ModelCallOrigin},
    domain::{ToolSpec, ToolSurface},
    model_standard::{InstructionBlock, InstructionKind},
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
        _host: &mut dyn HookModuleHost,
    ) -> Result<String, ProcessModuleError> {
        let input: HookInput = serde_json::from_str(&input).map_err(super::error)?;
        serde_json::to_string(&prepare(input.event)).map_err(super::error)
    }
}

fn prepare(event: HookEvent) -> HookResponse {
    match event {
        HookEvent::BeforeModel {
            origin: ModelCallOrigin::Direct,
            request,
        } => {
            let mut instructions = request.instructions;
            instructions.insert(
                0,
                InstructionBlock::new(
                    InstructionKind::Developer,
                    model_instructions(&request.model.model, &request.tools),
                    100,
                ),
            );
            HookResponse::ModelContext {
                messages: request.messages,
                instructions,
            }
        }
        _ => HookResponse::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
