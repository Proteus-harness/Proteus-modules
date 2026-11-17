//! Model-owned sizing for opaque Responses input, adapted from Codex
//! 67cc3c3 codex-rs/core/src/context_manager/history.rs:656-674,699-712.
use anyhow::Result;

use crate::{
    contracts::{ModelInputEstimate, ModelInputEstimateRequest, ModelPartTokenEstimate},
    model_standard::ContentPart,
};

/// Native Codex estimates decoded payload bytes without decoding base64, then
/// removes its fixed encryption overhead and rounds model-visible bytes up.
/// Operation order matters for saturation and for short/zero-size payloads.
fn encrypted_tokens(encoded_bytes: usize) -> usize {
    let visible_bytes = (encoded_bytes.saturating_mul(3) / 4).saturating_sub(650);
    visible_bytes.div_ceil(4)
}

pub(super) fn estimate(request: &ModelInputEstimateRequest) -> Result<ModelInputEstimate> {
    let mut estimates = Vec::new();
    for part in &request.parts {
        let ContentPart::Opaque { format, data } = &part.payload else {
            // The public request validator rejects these before dispatch.
            anyhow::bail!("model input estimate accepts only opaque parts");
        };
        if format != super::compaction::FORMAT {
            // An unsupported format is unknown, never an invented zero.
            continue;
        }
        super::compaction::validate_item(data)?;
        let encoded_bytes = data["encrypted_content"]
            .as_str()
            .expect("validated checkpoint ciphertext")
            .len();
        estimates.push(ModelPartTokenEstimate {
            part_id: part.part_id,
            tokens: u32::try_from(encrypted_tokens(encoded_bytes)).unwrap_or(u32::MAX),
        });
    }
    Ok(ModelInputEstimate { parts: estimates })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::ModelRef,
        model_standard::{CanonicalPart, PartProvenance, PartScope},
    };
    use serde_json::json;

    fn request(ciphertext: String) -> ModelInputEstimateRequest {
        ModelInputEstimateRequest {
            model: ModelRef::new("native", "fixture"),
            parts: vec![CanonicalPart::new(
                PartProvenance::Compactor,
                PartScope::Conversation,
                ContentPart::Opaque {
                    format: super::super::compaction::FORMAT.to_owned(),
                    data: json!({"type":"compaction", "encrypted_content":ciphertext}),
                },
            )],
        }
    }

    #[test]
    fn native_encryption_overhead_rounding_zero_and_saturation_match_upstream() {
        for (bytes, expected) in [
            (0, 0),
            (1, 0),
            (866, 0),
            (867, 0),
            (868, 1),
            (869, 1),
            (871, 1),
            (872, 1),
            (873, 1),
            (874, 2),
            (900, 7),
            (1000, 25),
            (2000, 213),
            (3172, 433),
            (3980, 584),
            (10000, 1713),
        ] {
            assert_eq!(encrypted_tokens(bytes), expected, "encoded bytes {bytes}");
        }
        let expected = (usize::MAX / 4).saturating_sub(650).div_ceil(4);
        assert_eq!(encrypted_tokens(usize::MAX), expected);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(encrypted_tokens(usize::MAX), 1_152_921_504_606_846_814);
    }

    #[test]
    fn old_checkpoint_needs_no_hint_and_wrapper_fields_do_not_count_or_change() {
        let mut request = request("x".repeat(3172));
        let ContentPart::Opaque { data, .. } = &mut request.parts[0].payload else {
            unreachable!()
        };
        data["id"] = json!("cmp-fixture");
        data["status"] = json!("completed");
        data["future_field"] = json!("wrapper".repeat(10000));
        let before = request.clone();
        let result = estimate(&request).unwrap();
        assert_eq!(result.parts.len(), 1);
        assert_eq!(result.parts[0].part_id, request.parts[0].part_id);
        assert_eq!(result.parts[0].tokens, 433);
        assert_eq!(
            request, before,
            "provider-owned bytes and part identity stay intact"
        );
    }

    #[test]
    fn ciphertext_length_is_utf8_bytes_without_base64_or_json_escape_processing() {
        assert_eq!(
            estimate(&request("é".repeat(500))).unwrap().parts[0].tokens,
            25
        );
        assert_eq!(
            estimate(&request("\\".repeat(1000))).unwrap().parts[0].tokens,
            25
        );
        assert_eq!(
            estimate(&request("x".repeat(867))).unwrap().parts[0].tokens,
            0
        );
    }

    #[test]
    fn unknown_formats_are_omitted_and_known_malformed_checkpoints_fail() {
        let mut request = request("x".repeat(1000));
        let ContentPart::Opaque { format, .. } = &mut request.parts[0].payload else {
            unreachable!()
        };
        *format = "another.provider.opaque".into();
        assert!(estimate(&request).unwrap().parts.is_empty());
        let unknown = request.parts[0].clone();
        let ContentPart::Opaque { format, .. } = &mut request.parts[0].payload else {
            unreachable!()
        };
        *format = super::super::compaction::FORMAT.into();
        // Mixed requests preserve a partial map keyed by canonical identity.
        let mut mixed = request.clone();
        let mut unknown = unknown;
        unknown.part_id = crate::domain::new_part_id();
        mixed.parts.push(unknown);
        let result = estimate(&mixed).unwrap();
        assert_eq!(result.parts.len(), 1);
        assert_eq!(result.parts[0].part_id, request.parts[0].part_id);
        assert_eq!(result.parts[0].tokens, 25);
        for invalid in [
            json!({"type":"compaction","encrypted_content":""}),
            json!({"type":"compaction","encrypted_content":42}),
            json!({"type":"message","encrypted_content":"x"}),
            json!({"type":"compaction","encrypted_content":"x","id":42}),
        ] {
            let ContentPart::Opaque { data, .. } = &mut request.parts[0].payload else {
                unreachable!()
            };
            *data = invalid;
            assert!(estimate(&request).is_err());
        }
    }

    #[tokio::test]
    async fn native_api_codex_and_explicit_compatible_estimate_without_credentials_or_transport() {
        use crate::{adapters::openai::OpenAiResponsesClient, contracts::Model};
        for client in [
            OpenAiResponsesClient::from_provider_config(json!({})).unwrap(),
            OpenAiResponsesClient::from_codex_config(json!({
                "auth_file":"input-estimate-must-never-open-this-auth-file.json"
            }))
            .unwrap(),
            OpenAiResponsesClient::from_provider_config(json!({
                "base_url":"http://127.0.0.1:1/v1", "capabilities":{"supports_compaction":true}
            }))
            .unwrap(),
        ] {
            assert!(client.supports_input_estimate());
            let result = client
                .estimate_input(request("x".repeat(3172)))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.parts[0].tokens, 433);
            let mut duplicate = request("x".repeat(1000));
            duplicate.parts.push(duplicate.parts[0].clone());
            assert!(client.estimate_input(duplicate).await.is_err());
            let mut invalid = request("x".repeat(1000));
            invalid.parts[0].payload = ContentPart::Text {
                text: "ordinary text".into(),
            };
            assert!(client.estimate_input(invalid).await.is_err());
            let mut malformed = request("x".repeat(1000));
            let ContentPart::Opaque { data, .. } = &mut malformed.parts[0].payload else {
                unreachable!()
            };
            data["encrypted_content"] = json!(42);
            let error = client.estimate_input(malformed).await.unwrap_err();
            assert!(error.to_string().contains("encrypted_content"), "{error}");
        }
    }

    #[tokio::test]
    async fn unsupported_transports_do_not_advertise_or_invent_estimates() {
        use crate::{adapters::openai::OpenAiResponsesClient, contracts::Model};
        let client = OpenAiResponsesClient::from_provider_config(json!({
            "base_url":"http://127.0.0.1:1/v1"
        }))
        .unwrap();
        assert!(!client.supports_input_estimate());
        assert!(
            client
                .estimate_input(request("x".repeat(3172)))
                .await
                .unwrap()
                .is_none()
        );
    }
}
