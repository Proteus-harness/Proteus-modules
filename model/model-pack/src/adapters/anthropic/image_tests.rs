use super::request::to_anthropic_request;
use crate::{
    domain::{ImageRef, ModelRef, ToolCall, ToolContent, ToolResult},
    model_standard::{CanonicalMessage, CanonicalModelRequest, ContentPart, MessageRole},
};

#[test]
fn local_image_is_encoded_as_base64_image_block_before_text() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("image");
    std::fs::write(&path, STANDARD.decode(data).unwrap()).unwrap();
    let request = CanonicalModelRequest::new(
        ModelRef::new("anthropic", "vision"),
        vec![CanonicalMessage::new(
            MessageRole::User,
            vec![
                ContentPart::Image {
                    image: ImageRef {
                        id: "fixture".into(),
                        name: "board.png".into(),
                        mime_type: "image/png".into(),
                        path,
                    },
                },
                ContentPart::Text {
                    text: "Describe".into(),
                },
            ],
        )],
    );
    let body = to_anthropic_request(&request).unwrap();
    assert_eq!(
        body["messages"][0]["content"][0],
        serde_json::json!({"type":"image", "source":{"type":"base64","media_type":"image/png","data":data}})
    );
    assert_eq!(body["messages"][0]["content"][1]["text"], "Describe");
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

fn tool_result_request(result: ToolResult, role: MessageRole) -> CanonicalModelRequest {
    CanonicalModelRequest::new(
        ModelRef::new("anthropic", "vision"),
        vec![
            CanonicalMessage::new(
                MessageRole::Assistant,
                vec![ContentPart::ToolCall {
                    call: ToolCall::new("image-call", "inspect", serde_json::json!({})),
                }],
            ),
            CanonicalMessage::new(role, vec![ContentPart::ToolResult { result }]),
            CanonicalMessage::new(
                MessageRole::User,
                vec![ContentPart::Text {
                    text: "Continue".into(),
                }],
            ),
        ],
    )
}

#[test]
fn tool_images_preserve_text_order_and_result_grouping_in_both_message_paths() {
    for role in [MessageRole::Tool, MessageRole::User] {
        let result = ToolResult::ok("image-call".into(), "Tool caption").with_content(vec![
            inline_image("image/png", TOOL_PNG),
            ToolContent::Binary {
                mime_type: "application/pdf".into(),
                data: "not an image".into(),
            },
            inline_image("image/gif", TOOL_GIF),
        ]);
        let body = to_anthropic_request(&tool_result_request(result, role)).unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(
            body["messages"][1]["content"],
            serde_json::json!([
                {
                    "type": "tool_result", "tool_use_id": "image-call", "is_error": false,
                    "content": [
                        { "type": "text", "text": "Tool caption" },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": TOOL_PNG } },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/gif", "data": TOOL_GIF } }
                    ]
                },
                { "type": "text", "text": "Continue" }
            ])
        );
    }
}

#[test]
fn image_results_keep_error_precedence_and_content_only_fallback() {
    let mut error = ToolResult::error("image-call".into(), "Tool error");
    error.output = "Lower-priority output".into();
    let cases = [error, ToolResult::ok("image-call".into(), "")];
    for (result, expected) in cases
        .into_iter()
        .zip(["Tool error", "[image tool content: image/png]"])
    {
        let is_error = !result.ok;
        let result = result.with_content(vec![inline_image("image/png", TOOL_PNG)]);
        let body = to_anthropic_request(&tool_result_request(result, MessageRole::Tool)).unwrap();
        let block = &body["messages"][1]["content"][0];
        assert_eq!(block["is_error"], is_error);
        assert_eq!(
            block["content"][0],
            serde_json::json!({ "type": "text", "text": expected })
        );
        assert_eq!(block["content"][1]["source"]["data"], TOOL_PNG);
        assert_eq!(block["content"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn non_image_results_keep_string_wire_shape_and_text_semantics() {
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
        let is_error = !result.ok;
        let body = to_anthropic_request(&tool_result_request(result, MessageRole::Tool)).unwrap();
        assert_eq!(
            body["messages"][1]["content"][0],
            serde_json::json!({
                "type": "tool_result", "tool_use_id": "image-call", "content": expected, "is_error": is_error,
            })
        );
    }
}
