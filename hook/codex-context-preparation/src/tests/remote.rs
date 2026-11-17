use super::*;
use proteus_contracts::{
    contracts::{
        ExecutionAttribution, HookEvent, HookInput, HookResponse,
        validate_context_preparation_output,
    },
    domain::ImageRef,
    model_standard::{
        CanonicalPart, ModelCapabilities, ModelFailure, ModelFailureKind, ModelOperation,
        PartProvenance, PartScope,
    },
    process_module::HookModule,
};

pub(super) fn checkpoint(value: &str) -> CanonicalModelResponse {
    CanonicalModelResponse::new(
        CanonicalMessage::new(
            MessageRole::Assistant,
            vec![ContentPart::Opaque {
                format: "fixture.compaction".into(),
                data: json!({"checkpoint":value}),
            }],
        ),
        vec![],
        FinishReason::Stop,
    )
}

pub(super) fn remote(
    input: TestInput,
    host: &mut dyn HookModuleHost,
) -> Result<ContextPreparationOutput, ProcessModuleError> {
    let config = crate::config::CompactorConfig::parse(&input.config).unwrap();
    compact_algorithm(input.preparation(), &config, true, host)
}

fn image(id: &str) -> ContentPart {
    ContentPart::Image {
        image: ImageRef {
            id: id.into(),
            name: "synthetic.png".into(),
            mime_type: "image/png".into(),
            path: format!("/synthetic/{id}.png").into(),
        },
    }
}

#[test]
fn remote_keeps_request_controls_and_tools_without_the_local_summary_prompt() {
    let user = CanonicalMessage::text(MessageRole::User, "work");
    let input = input(vec![user.clone()], 500);
    let expected = input.request.clone();
    let original = input.preparation();
    let mut host = TestHost::with_model_response(checkpoint("encrypted"));
    let output = remote(input, &mut host).unwrap();
    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.operation, ModelOperation::Compact);
    assert_eq!(request.model, expected.model);
    assert_eq!(request.messages, vec![user.clone()]);
    assert_eq!(request.tools, expected.tools);
    assert_eq!(request.instructions, expected.instructions);
    assert_eq!(request.reasoning, expected.reasoning);
    assert_eq!(request.sampling, expected.sampling);
    assert_eq!(request.cache, expected.cache);
    assert_eq!(request.client_metadata, expected.client_metadata);
    assert_eq!(request.metadata["existing"], "preserved");
    assert_eq!(request.metadata["suppress_stream_deltas"], true);
    assert_eq!(request.response_format, ResponseFormat::Text);
    assert_eq!(request.tool_choice, ToolChoice::Auto);
    assert_eq!(
        request.limits.max_input_tokens,
        expected.limits.max_input_tokens
    );
    assert_eq!(request.limits.max_output_tokens, None);
    assert_eq!(output.messages[0], user);
    assert!(output.messages.last().unwrap().display_text().is_empty());
    assert_eq!(output.summary, None);
    assert_eq!(output.summary_source.as_deref(), Some("model"));
    validate_context_preparation_output(&original, &output).unwrap();
}

#[test]
fn native_capability_selects_remote_and_unsupported_models_keep_local_summary() {
    let user = CanonicalMessage::text(MessageRole::User, "work");
    for supported in [false, true] {
        let model_response = if supported {
            checkpoint("cipher")
        } else {
            CanonicalModelResponse::new(
                CanonicalMessage::text(MessageRole::Assistant, "summary"),
                vec![],
                FinishReason::Stop,
            )
        };
        let mut host = TestHost::with_model_response(model_response);
        let input = input(vec![user.clone()], 500).preparation();
        let hook = HookInput {
            event: HookEvent::BeforeContext {
                output: ContextPreparationOutput::unchanged(input.messages.clone()),
                input,
                target: None,
                model_capabilities: Some(ModelCapabilities::empty().with_compaction(supported)),
            },
            attribution: ExecutionAttribution::detached(
                proteus_contracts::domain::new_execution_id(),
            ),
            cwd: "/repo".into(),
            conversation: None,
        };
        let module = crate::CodexPreparationHook(
            crate::config::CompactorConfig::parse(&json!({"trigger_tokens":100})).unwrap(),
        );
        let result = module
            .invoke_json(serde_json::to_string(&hook).unwrap(), &mut host)
            .unwrap();
        let HookResponse::ContextPreparation { output } = serde_json::from_str(&result).unwrap()
        else {
            panic!("expected preparation")
        };
        assert!(output.changed);
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests[0].operation == ModelOperation::Compact, supported);
        assert_eq!(output.summary.is_none(), supported);
    }
}

#[test]
fn repeated_remote_compaction_consumes_old_checkpoint_but_installs_only_the_new_one() {
    let user = CanonicalMessage::text(MessageRole::User, "work");
    let mut host = TestHost::with_model_response(checkpoint("old"));
    let first = remote(input(vec![user.clone()], 500), &mut host).unwrap();
    let old = first.messages.last().unwrap().clone();
    let mut host = TestHost::with_model_response(checkpoint("new"));
    let second = remote(input(first.messages.clone(), 500), &mut host).unwrap();
    assert!(host.requests.lock().unwrap()[0].messages.contains(&old));
    assert_eq!(second.messages[0], user);
    assert!(!second.messages.contains(&old));
    let opaque = second
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter(|part| matches!(part.payload, ContentPart::Opaque { .. }))
        .count();
    assert_eq!(opaque, 1);
    assert!(
        matches!(&second.messages.last().unwrap().parts[0].payload, ContentPart::Opaque { data, .. } if data["checkpoint"]=="new")
    );
}

#[test]
fn remote_reinjects_ephemeral_context_before_last_user_without_retaining_assistant_turns() {
    let earlier = CanonicalMessage::text(MessageRole::User, "first");
    let last = CanonicalMessage::text(MessageRole::User, "latest");
    let context = CanonicalMessage::from_parts(
        MessageRole::User,
        vec![CanonicalPart::new(
            PartProvenance::ContextBuilder,
            PartScope::Request,
            ContentPart::Text {
                text: "ephemeral".into(),
            },
        )],
    );
    let assistant = CanonicalMessage::text(MessageRole::Assistant, "intermediate");
    let mut host = TestHost::with_model_response(checkpoint("new"));
    let output = remote(
        input(
            vec![earlier.clone(), assistant, context.clone(), last.clone()],
            500,
        ),
        &mut host,
    )
    .unwrap();
    assert_eq!(&output.messages[..3], &[earlier, context, last]);
    assert_eq!(output.messages.len(), 4);
}

#[test]
fn remote_has_64k_user_budget_and_keeps_images_with_original_identity_when_they_fit() {
    let older = CanonicalMessage::text(MessageRole::User, "x".repeat(100_000));
    let latest = CanonicalMessage::new(
        MessageRole::User,
        vec![
            image("board"),
            ContentPart::Text {
                text: "latest".into(),
            },
        ],
    );
    let mut host = TestHost::with_model_response(checkpoint("new"));
    let output = remote(
        input(vec![older.clone(), latest.clone()], 100_000),
        &mut host,
    )
    .unwrap();
    assert_eq!(
        output.messages[0], older,
        "25k text must survive the remote64k budget"
    );
    assert_eq!(output.messages[1], latest);
    assert!(output.user_message_replacements.is_empty());
}

#[test]
fn remote_image_boundary_retains_later_parts_atomically_and_does_not_backfill() {
    let old = CanonicalMessage::text(MessageRole::User, "old");
    let boundary = CanonicalMessage::new(
        MessageRole::User,
        vec![
            ContentPart::Text {
                text: "prefix".into(),
            },
            ContentPart::Text {
                text: "<image name=\"synthetic\">".into(),
            },
            image("oversized"),
            ContentPart::Text {
                text: "</image>".into(),
            },
            ContentPart::Text {
                text: "tail".into(),
            },
        ],
    );
    let selected = crate::remote_history::select_recent_users(&[old.clone(), boundary.clone()], 2);
    assert_eq!(selected.messages.len(), 1);
    assert_eq!(selected.messages[0].display_text(), "tail");
    assert_eq!(selected.replacements[0].source_message_id, boundary.id);
    assert!(
        selected.messages[0]
            .parts
            .iter()
            .all(|part| matches!(part.payload, ContentPart::Text { .. }))
    );
    let empty_boundary = CanonicalMessage::new(MessageRole::User, vec![image("too-large")]);
    let selected = crate::remote_history::select_recent_users(&[old, empty_boundary], 10);
    assert!(
        selected.messages.is_empty(),
        "no older backfill after an oversized last image"
    );
}

#[test]
fn remote_boundary_keeps_a_fitting_image_and_truncates_unicode_with_fresh_identities() {
    let user = CanonicalMessage::new(
        MessageRole::User,
        vec![
            ContentPart::Text {
                text: "界".repeat(200),
            },
            image("keep"),
        ],
    );
    let selected = crate::remote_history::select_recent_users(&[user.clone()], 1845);
    assert_eq!(selected.messages.len(), 1);
    let replacement = &selected.messages[0];
    assert_ne!(replacement.id, user.id);
    assert!(
        replacement
            .parts
            .iter()
            .any(|part| matches!(part.payload, ContentPart::Image { .. }))
    );
    assert!(
        replacement
            .parts
            .iter()
            .all(|part| part.provenance == PartProvenance::Compactor
                && part.scope == PartScope::Conversation)
    );
    let mut output = ContextPreparationOutput::changed(selected.messages, None);
    output.user_message_replacements = selected.replacements;
    validate_context_preparation_output(&ContextPreparationInput::new(vec![user], None), &output)
        .unwrap();
}

#[test]
fn malformed_checkpoint_responses_are_terminal_without_local_fallback() {
    let good = checkpoint("good");
    let mut duplicate = good.clone();
    duplicate.messages.extend(good.messages.clone());
    let mut mixed = good.clone();
    mixed.messages[0].parts.push(CanonicalPart::new(
        PartProvenance::Model,
        PartScope::Conversation,
        ContentPart::Text {
            text: "not opaque".into(),
        },
    ));
    let mut wrong_role = good.clone();
    wrong_role.messages[0].role = MessageRole::User;
    let mut wrong_finish = good.clone();
    wrong_finish.finish_reason = FinishReason::Length;
    for response in [
        CanonicalModelResponse::new(
            CanonicalMessage::text(MessageRole::Assistant, "plain summary"),
            vec![],
            FinishReason::Stop,
        ),
        duplicate,
        mixed,
        wrong_role,
        wrong_finish,
    ] {
        let mut host = TestHost::with_results(vec![Ok(response), Ok(good.clone())]);
        assert!(
            remote(
                input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500),
                &mut host
            )
            .is_err()
        );
        assert_eq!(host.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn remote_retries_only_transient_errors_at_most_twice_and_honors_zero_delay_advice() {
    let transient = || {
        Err(ProcessModuleError::from_model_failure(ModelFailure::new(
            ModelFailureKind::Retryable {
                retry_delay_ms: Some(0),
            },
            "transient",
        )))
    };
    let mut host = TestHost::with_results(vec![transient(), transient(), Ok(checkpoint("new"))]);
    remote(
        input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500),
        &mut host,
    )
    .unwrap();
    assert_eq!(host.requests.lock().unwrap().len(), 3);
    let mut host = TestHost::with_results(vec![
        transient(),
        transient(),
        transient(),
        Ok(checkpoint("unused")),
    ]);
    let input = input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500)
        .with_config(json!({"trigger_tokens":100, "stream_max_retries":100}));
    assert!(remote(input, &mut host).is_err());
    assert_eq!(host.requests.lock().unwrap().len(), 3);
}

#[test]
fn remote_waits_for_provider_retry_advice_instead_of_using_the_local_backoff() {
    let mut host = TestHost::with_results(vec![
        Err(ProcessModuleError::from_model_failure(ModelFailure::new(
            ModelFailureKind::Retryable {
                retry_delay_ms: Some(300),
            },
            "wait",
        ))),
        Ok(checkpoint("new")),
    ]);
    let started = std::time::Instant::now();
    remote(
        input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500),
        &mut host,
    )
    .unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(300));
}

#[test]
fn remote_terminal_failures_and_cancellation_do_not_install_or_retry() {
    for kind in [
        ModelFailureKind::ContextWindowExceeded,
        ModelFailureKind::Other,
        ModelFailureKind::SessionBudgetExceeded,
        ModelFailureKind::Interrupted,
    ] {
        let mut host = TestHost::with_results(vec![
            Err(ProcessModuleError::from_model_failure(ModelFailure::new(
                kind, "terminal",
            ))),
            Ok(checkpoint("unused")),
        ]);
        assert!(
            remote(
                input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500),
                &mut host
            )
            .is_err()
        );
        assert_eq!(host.requests.lock().unwrap().len(), 1);
    }
    struct CancelAfterResponse(TestHost);
    impl HookModuleHost for CancelAfterResponse {
        fn is_cancelled(&self) -> Result<bool, ProcessModuleError> {
            Ok(!self.0.requests.lock().unwrap().is_empty())
        }
        fn complete_model_json(&self, request: String) -> Result<String, ProcessModuleError> {
            self.0.complete_model_json(request)
        }
    }
    let mut host = CancelAfterResponse(TestHost::with_model_response(checkpoint("discard")));
    let error = remote(
        input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500),
        &mut host,
    )
    .unwrap_err();
    assert_eq!(
        error.model_failure.unwrap().kind,
        ModelFailureKind::Interrupted
    );
    assert_eq!(host.0.requests.lock().unwrap().len(), 1);
}

#[test]
fn remote_zero_retry_budget_and_preexisting_cancel_make_no_extra_model_calls() {
    let mut host = TestHost::with_results(vec![
        Err(ProcessModuleError::from_model_failure(ModelFailure::new(
            ModelFailureKind::Retryable {
                retry_delay_ms: Some(0),
            },
            "transient",
        ))),
        Ok(checkpoint("unused")),
    ]);
    let configured = input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500)
        .with_config(json!({"trigger_tokens":100,"stream_max_retries":0}));
    assert!(remote(configured, &mut host).is_err());
    assert_eq!(host.requests.lock().unwrap().len(), 1);
    let mut host = TestHost {
        cancelled: true,
        ..TestHost::default()
    };
    assert!(
        remote(
            input(vec![CanonicalMessage::text(MessageRole::User, "work")], 500),
            &mut host
        )
        .is_err()
    );
    assert!(host.requests.lock().unwrap().is_empty());
}

#[test]
fn disconnected_checkpoint_is_discarded_when_the_same_request_is_retried() {
    let partial = checkpoint("discard").messages.remove(0);
    let failure = ModelFailure::new(ModelFailureKind::StreamDisconnected, "closed")
        .with_completed_messages(vec![partial.clone()]);
    let mut host = TestHost::with_results(vec![
        Err(ProcessModuleError::from_model_failure(failure)),
        Ok(checkpoint("complete")),
    ]);
    let user = CanonicalMessage::text(MessageRole::User, "work");
    let output = remote(input(vec![user.clone()], 500), &mut host).unwrap();
    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    assert!(!requests[1].messages.contains(&partial));
    assert!(!output.messages.contains(&partial));
    assert_eq!(output.messages[0], user);
    assert!(
        matches!(&output.messages.last().unwrap().parts[0].payload, ContentPart::Opaque {data,..} if data["checkpoint"]=="complete")
    );
}

#[test]
fn fitting_image_labels_stay_together_in_a_truncated_boundary_message() {
    let source = CanonicalMessage::new(
        MessageRole::User,
        vec![
            ContentPart::Text {
                text: "界".repeat(3000),
            },
            ContentPart::Text {
                text: "<image>".into(),
            },
            image("kept"),
            ContentPart::Text {
                text: "</image>".into(),
            },
            ContentPart::Text {
                text: "last".into(),
            },
        ],
    );
    let selected = crate::remote_history::select_recent_users(&[source], 1900);
    let parts = &selected.messages[0].parts;
    let index = parts
        .iter()
        .position(|part| matches!(part.payload, ContentPart::Image { .. }))
        .unwrap();
    assert!(matches!(&parts[index-1].payload,ContentPart::Text {text} if text=="<image>"));
    assert!(matches!(&parts[index+1].payload,ContentPart::Text {text} if text=="</image>"));
    assert!(matches!(&parts.last().unwrap().payload,ContentPart::Text {text} if text=="last"));
}

#[test]
fn remote_output_estimate_charges_checkpoint_payload_and_empty_user_anchors_survive() {
    let user = CanonicalMessage::text(MessageRole::User, "");
    let mut host = TestHost::with_model_response(checkpoint(&"cipher".repeat(1000)));
    let output = remote(input(vec![user.clone()], 500), &mut host).unwrap();
    assert_eq!(output.messages[0], user);
    assert!(output.token_estimate.unwrap() > 1000);
}

#[test]
fn remote_overflow_preflight_trims_only_trailing_results_without_changing_source_history() {
    let user = CanonicalMessage::text(MessageRole::User, "work");
    let call = CanonicalMessage::new(
        MessageRole::Assistant,
        vec![ContentPart::ToolCall {
            call: ToolCall::new("call", "write_file", json!({})),
        }],
    );
    let result = CanonicalMessage::new(
        MessageRole::Tool,
        vec![ContentPart::ToolResult {
            result: ToolResult::ok("call".into(), "x".repeat(10_000)),
        }],
    );
    let source = vec![user, call, result.clone()];
    let mut host = TestHost::with_model_response(checkpoint("new"));
    remote(
        input(source.clone(), 500).with_window_tokens(Some(100)),
        &mut host,
    )
    .unwrap();
    let request = host.requests.lock().unwrap();
    assert_eq!(request[0].messages.len(), source.len());
    assert_eq!(&request[0].messages[..2], &source[..2]);
    assert!(
        matches!(&request[0].messages[2].parts[0].payload,ContentPart::ToolResult {result} if result.call_id=="call" && result.output=="Output exceeded the available model context and was truncated")
    );
    assert_eq!(source[2], result);
}

#[test]
fn remote_preflight_charges_inline_tool_images_and_does_not_trim_past_a_user_message() {
    let result = CanonicalMessage::new(
        MessageRole::Tool,
        vec![ContentPart::ToolResult {
            result: ToolResult::ok("fixture".into(), "small caption").with_content(vec![
                proteus_contracts::domain::ToolContent::Image {
                    mime_type: "image/png".into(),
                    data: "synthetic-inline-image".into(),
                },
            ]),
        }],
    );
    let mut messages = vec![result.clone()];
    crate::remote_window::trim_trailing_tool_outputs(&mut messages, &[], Some(1000));
    assert!(
        matches!(&messages[0].parts[0].payload,ContentPart::ToolResult {result} if result.content.is_empty())
    );
    let mut messages = vec![
        result.clone(),
        CanonicalMessage::text(MessageRole::User, "stop"),
    ];
    let before = messages.clone();
    crate::remote_window::trim_trailing_tool_outputs(&mut messages, &[], Some(1));
    assert_eq!(messages, before);
    let mut messages = vec![result.clone()];
    crate::remote_window::trim_trailing_tool_outputs(&mut messages, &[], None);
    assert_eq!(messages, vec![result]);
}
