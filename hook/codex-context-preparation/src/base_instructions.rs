//! Public Codex model-specific templates pinned to rust-v0.167.0.
use proteus_contracts::{
    contracts::{HookEvent, HookInput, HookResponse, ModelCallOrigin},
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
                    template(&request.model.model),
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
