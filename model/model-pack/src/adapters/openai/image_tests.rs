use super::{
    OpenAiPromptCacheConfig,
    model_profile::OpenAiModelProfile,
    request::{to_openai_request, to_openai_request_with_cache},
};
use crate::{
    domain::{ImageRef, ModelRef, ToolCall, ToolCallSurface, ToolContent, ToolResult},
    model_standard::{CanonicalMessage, CanonicalModelRequest, ContentPart, MessageRole},
};

#[test]
fn local_image_is_encoded_as_image_input_and_missing_file_fails_explicitly() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("image");
    std::fs::write(&path, STANDARD.decode(data).unwrap()).unwrap();
    let request = CanonicalModelRequest::new(
        ModelRef::new("openai", "vision"),
        vec![CanonicalMessage::new(
            MessageRole::User,
            vec![
                ContentPart::Image {
                    image: ImageRef {
                        id: "fixture".into(),
                        name: "board.png".into(),
                        mime_type: "image/png".into(),
                        path: path.clone(),
                    },
                },
                ContentPart::Text {
                    text: "Describe".into(),
                },
            ],
        )],
    );
    let body = to_openai_request(&request).unwrap();
    assert_eq!(body["input"][0]["content"][0]["type"], "input_image");
    assert_eq!(
        body["input"][0]["content"][0]["image_url"],
        format!("data:image/png;base64,{data}")
    );
    assert_eq!(body["input"][0]["content"][1]["text"], "Describe");
    std::fs::remove_file(path).unwrap();
    assert!(
        format!("{:#}", to_openai_request(&request).unwrap_err())
            .contains("cannot read attached image")
    );
}

const TOOL_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
const TOOL_GIF: &str = "R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";

fn inline_image(mime_type: &str, data: &str) -> ToolContent {
    ToolContent::Image {
        mime_type: mime_type.into(),
        data: data.into(),
    }
}

fn tool_result_body(
    result: ToolResult,
    surface: ToolCallSurface,
    supports_image_input: bool,
) -> anyhow::Result<serde_json::Value> {
    let profile = OpenAiModelProfile::from_provider_config(&serde_json::json!({
        "capabilities": { "supports_image_input": supports_image_input, "supports_freeform_tools": true }
    }))?;
    let request = CanonicalModelRequest::new(
        ModelRef::new("openai", "vision"),
        vec![
            CanonicalMessage::new(
                MessageRole::Assistant,
                vec![ContentPart::ToolCall {
                    call: ToolCall::new(
                        "image-call",
                        "inspect",
                        serde_json::json!({ "input": "inspect" }),
                    )
                    .with_surface(surface),
                }],
            ),
            CanonicalMessage::new(MessageRole::Tool, vec![ContentPart::ToolResult { result }]),
        ],
    );
    to_openai_request_with_cache(&request, &OpenAiPromptCacheConfig::default(), &profile)
}

#[test]
fn function_and_custom_outputs_keep_text_and_image_order() {
    for (surface, output_type) in [
        (ToolCallSurface::Function, "function_call_output"),
        (ToolCallSurface::Freeform, "custom_tool_call_output"),
    ] {
        let result = ToolResult::ok("image-call".into(), "Tool caption").with_content(vec![
            inline_image("image/png", TOOL_PNG),
            ToolContent::Binary {
                mime_type: "application/pdf".into(),
                data: "not an image".into(),
            },
            inline_image("image/gif", TOOL_GIF),
        ]);
        let body = tool_result_body(result, surface, true).unwrap();
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert_eq!(
            body["input"][1],
            serde_json::json!({
                "type": output_type, "call_id": "image-call",
                "output": [
                    { "type": "input_text", "text": "Tool caption" },
                    { "type": "input_image", "image_url": format!("data:image/png;base64,{TOOL_PNG}"), "detail": "auto" },
                    { "type": "input_image", "image_url": format!("data:image/gif;base64,{TOOL_GIF}"), "detail": "auto" }
                ]
            })
        );
    }
}

#[test]
fn image_results_keep_error_precedence_and_content_only_fallback() {
    for surface in [ToolCallSurface::Function, ToolCallSurface::Freeform] {
        let mut error = ToolResult::error("image-call".into(), "Tool error");
        error.output = "Lower-priority output".into();
        let cases = [error, ToolResult::ok("image-call".into(), "")];
        for (result, expected) in cases
            .into_iter()
            .zip(["Tool error", "[image tool content: image/png]"])
        {
            let result = result.with_content(vec![inline_image("image/png", TOOL_PNG)]);
            let body = tool_result_body(result, surface, true).unwrap();
            let output = &body["input"][1]["output"];
            assert_eq!(
                output[0],
                serde_json::json!({ "type": "input_text", "text": expected })
            );
            assert_eq!(
                output[1]["image_url"],
                format!("data:image/png;base64,{TOOL_PNG}")
            );
            assert_eq!(output.as_array().unwrap().len(), 2);
        }
    }
}

#[test]
fn disabled_image_capability_rejects_both_tool_output_surfaces() {
    for surface in [ToolCallSurface::Function, ToolCallSurface::Freeform] {
        let result = ToolResult::ok("image-call".into(), "Tool caption")
            .with_content(vec![inline_image("image/png", TOOL_PNG)]);
        let error = tool_result_body(result, surface, false).unwrap_err();
        assert!(error.to_string().contains("image input is disabled"));
    }
}

#[test]
fn non_image_results_keep_string_wire_shape_without_image_capability() {
    for (surface, output_type) in [
        (ToolCallSurface::Function, "function_call_output"),
        (ToolCallSurface::Freeform, "custom_tool_call_output"),
    ] {
        let mut error = ToolResult::error("image-call".into(), "Tool error");
        error.output = "Lower-priority output".into();
        let structured = ToolResult::ok("image-call".into(), "").with_content(vec![
            ToolContent::Text {
                text: "Text block".into(),
            },
            ToolContent::Json {
                value: serde_json::json!({"count": 2}),
            },
            ToolContent::Binary {
                mime_type: "image/png".into(),
                data: TOOL_PNG.into(),
            },
        ]);
        for result in [
            ToolResult::ok("image-call".into(), "Output"),
            ToolResult::ok("image-call".into(), ""),
            ToolResult::error("image-call".into(), ""),
            error,
            structured,
        ] {
            let expected = result.text_or_status();
            let body = tool_result_body(result, surface, false).unwrap();
            assert_eq!(
                body["input"][1],
                serde_json::json!({
                    "type": output_type, "call_id": "image-call", "output": expected,
                })
            );
        }
    }
}
