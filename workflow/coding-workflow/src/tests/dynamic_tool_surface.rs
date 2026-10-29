use super::*;

#[test]
fn proteus_tool_describe_returns_policy_visible_hidden_schema() {
    let input = workflow_input("describe hidden tool");
    let git_log = test_tool("git_log", "Show commit history", ToolSafety::ReadOnly);
    let mut host = FakeHost::default().with_tools(vec![git_log], Vec::new());
    let call = ToolCall::new(
        new_call_id(),
        dynamic_tools::TOOL_DESCRIBE,
        json!({ "name": "git_log" }),
    );

    let result = dynamic_tools::handle_meta_tool_call(&mut host, &input, &call, "execute").unwrap();
    let output: Value = serde_json::from_str(&result.output).expect("describe output json");

    assert!(result.ok);
    assert_eq!(result.call_id, call.id);
    assert_eq!(output["name"], "git_log");
    assert_eq!(output["required_args"], Value::Null);
    assert_eq!(output["input_schema"]["required"], json!(["path"]));
}

#[test]
fn proteus_tool_search_returns_compact_policy_visible_matches() {
    let input = workflow_input("search hidden tools");
    let git_log = test_tool("git_log", "Show commit history", ToolSafety::ReadOnly);
    let shell = test_tool("shell", "Run terminal commands", ToolSafety::RunsCommands);
    let mut host = FakeHost::default().with_tools(vec![git_log, shell], Vec::new());
    let call = ToolCall::new(
        new_call_id(),
        dynamic_tools::TOOL_SEARCH,
        json!({ "query": "commit history", "limit": 3 }),
    );

    let result = dynamic_tools::handle_meta_tool_call(&mut host, &input, &call, "execute").unwrap();
    let output: Value = serde_json::from_str(&result.output).expect("search output json");

    assert!(result.ok);
    assert_eq!(result.call_id, call.id);
    assert_eq!(output["matches"][0]["name"], "git_log");
    assert_eq!(output["matches"][0]["input_schema"], Value::Null);
    assert_eq!(output["matches"][0]["required_args"], json!(["path"]));
    for args in [
        json!({"query":"history","limit":"3"}),
        json!({"query":"history","safety":"typo"}),
    ] {
        let invalid = ToolCall::new(new_call_id(), dynamic_tools::TOOL_SEARCH, args);
        let result =
            dynamic_tools::handle_meta_tool_call(&mut host, &input, &invalid, "execute").unwrap();
        assert!(!result.ok);
        assert_eq!(result.metadata["validation_error"], true);
    }
}

#[test]
fn proteus_tool_call_preserves_execution_identity_and_actual_result() {
    let outer_call = ToolCall::new(
        new_call_id(),
        dynamic_tools::TOOL_CALL,
        json!({
            "name": "hidden_echo",
            "args": { "path": "README.md" }
        }),
    );
    let input = workflow_input("call hidden tool");
    let input_json = serde_json::to_string(&input).expect("input json");
    let hidden_echo = test_tool("hidden_echo", "Echo hidden file", ToolSafety::ReadOnly);
    let mut host = FakeHost::with_responses(vec![
        tool_call_response(outer_call.clone()),
        CanonicalModelResponse::new(
            CanonicalMessage::text(MessageRole::Assistant, "final"),
            Vec::new(),
            FinishReason::Stop,
        ),
    ])
    .with_tools(vec![hidden_echo], Vec::new());

    let output_json = CodingSingleLoopWorkflow::default()
        .run_json(input_json, &mut host)
        .expect("workflow succeeds");
    let output: WorkflowModuleOutput =
        serde_json::from_str(output_json.as_str()).expect("output json");

    let executed_calls = host.executed_calls.lock().expect("executed calls");
    assert_eq!(executed_calls.len(), 1);
    assert_eq!(executed_calls[0].name, "hidden_echo");
    assert_eq!(executed_calls[0].id, outer_call.id);

    let result = output
        .new_messages
        .iter()
        .find_map(|message| {
            message.parts.iter().find_map(|part| match &part.payload {
                ContentPart::ToolResult { result } => Some(result),
                _ => None,
            })
        })
        .expect("tool result");
    assert_eq!(result.call_id, outer_call.id);
    assert_eq!(result.metadata, json!({"inner": true}));
    let checkpoints = host.checkpoints.lock().unwrap();
    assert_eq!(
        checkpoints[0].tool_results[0].execution_call,
        executed_calls[0]
    );
}

#[test]
fn proteus_tool_call_rejects_meta_tool_recursion_without_execution() {
    let input = workflow_input("bad recursive call");
    let mut host = FakeHost::default();
    let call = ToolCall::new(
        new_call_id(),
        dynamic_tools::TOOL_CALL,
        json!({
            "name": dynamic_tools::TOOL_SEARCH,
            "args": { "query": "anything" }
        }),
    );

    let result = dynamic_tools::handle_meta_tool_call(&mut host, &input, &call, "execute").unwrap();

    assert!(!result.ok);
    assert_eq!(result.call_id, call.id);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("cannot call Proteus meta-tools")
    );
    assert!(
        host.executed_calls
            .lock()
            .expect("executed calls")
            .is_empty()
    );
}

#[test]
fn tool_search_preserves_field_weights_filters_and_case_sensitive_ties() {
    let input = workflow_input("search scoring regression");
    let make = |name: &str, description: &str| {
        ToolSpec::new(name, description, json!({}), ToolSafety::ReadOnly)
    };
    let candidates = vec![
        make("zeta", "needle"),
        make("alpha", "needle"),
        make("Alpha", "needle"),
        make("description_only", "needle needle"),
        make("needle", ""),
        make("metadata_only", "").with_metadata(json!({
            "needle": ["NEEDLE", {"tags": ["needle"]}]
        })),
        ToolSpec::new(
            "schema_only",
            "",
            json!({"type": "object", "properties": {"needle": {"type": "string"}}}),
            ToolSafety::ReadOnly,
        ),
        make(dynamic_tools::TOOL_SEARCH, "needle"),
        ToolSpec::new("writer", "needle", json!({}), ToolSafety::WritesFiles),
    ];
    let mut host = FakeHost::default().with_tools(candidates, Vec::new());
    let query = "  Needle NEEDLE needle  ";
    for limit in [3, 20] {
        let call = ToolCall::new(
            new_call_id(),
            dynamic_tools::TOOL_SEARCH,
            json!({"query": query, "limit": limit, "safety": "read_only"}),
        );
        let result =
            dynamic_tools::handle_meta_tool_call(&mut host, &input, &call, "execute").unwrap();
        assert!(result.ok);
        let output: Value = serde_json::from_str(&result.output).unwrap();
        let actual = output["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["name"].as_str().unwrap(),
                    item["score"].as_f64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let expected = [
            ("needle", 5.0),
            ("metadata_only", 2.0),
            ("Alpha", 1.5),
            ("alpha", 1.5),
            ("description_only", 1.5),
            ("zeta", 1.5),
            ("schema_only", 0.5),
        ];
        assert_eq!(actual, expected[..limit.min(expected.len())]);
        assert_eq!(output["query"], query);
        assert_eq!(output["searched"], 9);
    }
}

#[test]
fn tool_search_preserves_unicode_empty_and_underscore_queries() {
    let input = workflow_input("search tokenizer regression");
    let candidates = ["zeta", "alpha"]
        .into_iter()
        .map(|name| {
            ToolSpec::new(
                name,
                "École 中 stanbul ___ read_file",
                json!({}),
                ToolSafety::ReadOnly,
            )
        })
        .collect();
    let mut host = FakeHost::default().with_tools(candidates, Vec::new());

    // Token length is UTF-8 bytes, underscores stay in tokens, and lowercase
    // expansion is split with the same Unicode predicate as before.
    for (query, matches) in [
        ("", false),
        (" \t\n", false),
        ("é ab __ !", false),
        ("read file", false),
        ("ÉCOLE école", true),
        ("中 中", true),
        ("İSTANBUL", true),
        ("READ_FILE read_file", true),
        ("___", true),
    ] {
        let call = ToolCall::new(
            new_call_id(),
            dynamic_tools::TOOL_SEARCH,
            json!({"query": query}),
        );
        let result =
            dynamic_tools::handle_meta_tool_call(&mut host, &input, &call, "execute").unwrap();
        assert!(result.ok, "{query:?}");
        let output: Value = serde_json::from_str(&result.output).unwrap();
        let actual = output["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["name"].as_str().unwrap(),
                    item["score"].as_f64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let expected = if matches {
            vec![("alpha", 1.5), ("zeta", 1.5)]
        } else {
            vec![]
        };
        assert_eq!(actual, expected, "{query:?}");
        assert_eq!(output["query"], query);
        assert_eq!(output["searched"], 2);
    }
}

#[test]
fn proteus_tool_call_rejects_provider_hosted_tool_without_execution() {
    let input = workflow_input("bad hosted call");
    let web_search = test_tool("web_search", "Search the web", ToolSafety::Network).with_surface(
        ToolSurface::provider_hosted(HostedToolConfig::WebSearch {
            config: WebSearchHostedToolConfig::default(),
        }),
    );
    let mut host = FakeHost::default().with_tools(vec![web_search], Vec::new());
    let call = ToolCall::new(
        new_call_id(),
        dynamic_tools::TOOL_CALL,
        json!({ "name": "web_search", "args": {} }),
    );

    let result = dynamic_tools::handle_meta_tool_call(&mut host, &input, &call, "execute").unwrap();

    assert!(!result.ok);
    assert_eq!(result.call_id, call.id);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("provider-hosted tool 'web_search' cannot be invoked")
    );
    assert!(
        host.executed_calls
            .lock()
            .expect("executed calls")
            .is_empty()
    );
}

#[test]
fn proteus_tool_call_rejects_non_readonly_hidden_tool_in_plan_phase() {
    let input = workflow_input("plan write");
    let write_file = test_tool("write_file", "Write a file", ToolSafety::WritesFiles);
    let mut host = FakeHost::default().with_tools(vec![write_file], Vec::new());
    let call = ToolCall::new(
        new_call_id(),
        dynamic_tools::TOOL_CALL,
        json!({
            "name": "write_file",
            "args": { "path": "README.md" }
        }),
    );

    let result = dynamic_tools::handle_meta_tool_call(&mut host, &input, &call, "plan").unwrap();

    assert!(!result.ok);
    assert_eq!(result.call_id, call.id);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("phase")
    );
    assert!(
        host.executed_calls
            .lock()
            .expect("executed calls")
            .is_empty()
    );
}
