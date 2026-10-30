use super::*;
use serde::ser::{SerializeSeq, Serializer};

fn legacy_estimate(tools: &[ToolSpec]) -> usize {
    tools
        .iter()
        .filter_map(|tool| serde_json::to_string(tool).ok())
        .map(|json| json.len() / 4)
        .sum()
}

#[test]
fn byte_count_matches_compact_json_for_tool_fields_and_surfaces() {
    let samples = [
        Value::Null,
        json!(true),
        json!(i64::MIN),
        json!(u64::MAX),
        json!(-0.0),
        json!(1.25e-100),
        json!("Кириллица 中文 😀 \"\\\n\r\t\u{0000}\u{0008}\u{001f}"),
        json!({"quote\"\\\n": ["😀", null, false, 1.5, {"nested": "é"}]}),
    ];
    let mut tools = samples
        .into_iter()
        .enumerate()
        .map(|(index, sample)| {
            ToolSpec::new(
                format!("tool_{index}"),
                "Описание \"\\\n\t😀",
                json!({
                    "type": "object",
                    "properties": {"quoted\"key": {"enum": [sample.clone()]}},
                    "required": ["quoted\"key"],
                }),
                ToolSafety::ReadOnly,
            )
            .with_metadata(json!({"payload": sample}))
        })
        .collect::<Vec<_>>();
    tools.push(hosted_web_search_spec());
    tools.push(
        spec("freeform", "grammar", ToolSafety::RunsCommands)
            .with_surface(ToolSurface::freeform_lark("start: \"中\"\n"))
            .with_timeout(u64::MAX),
    );
    tools.push(
        spec("strict", "output schema", ToolSafety::ReadOnly).with_surface(ToolSurface::Function {
            strict: true,
            output_schema: Some(json!({"type": "string", "enum": ["😀", "\n"]})),
        }),
    );
    tools.push(
        spec("workflow", "direct", ToolSafety::WritesFiles)
            .with_surface(ToolSurface::workflow_function())
            .with_parallel_tool_calls(true),
    );

    // Insertion-ordered and sorted serde_json builds use their own unchanged
    // serializer in both paths; counting must not rebuild or mutate the map.
    let mut map = Map::new();
    map.insert("z-first".to_owned(), json!("line\n"));
    map.insert("a-second".to_owned(), json!("中"));
    tools.push(spec("ordered", "", ToolSafety::Dangerous).with_metadata(Value::Object(map)));

    for tool in &tools {
        let before = serde_json::to_string(tool).unwrap();
        assert_eq!(serialized_json_len(tool).unwrap(), before.len());
        assert_eq!(serde_json::to_string(tool).unwrap(), before);
    }
    assert_eq!(estimate_tool_schema_tokens(&tools), legacy_estimate(&tools));
    assert_eq!(estimate_tool_schema_tokens(&[]), 0);
}

#[test]
fn estimate_rounds_each_tool_before_summing() {
    let mut tool = spec("rounding", "", ToolSafety::ReadOnly);
    let initial_len = serde_json::to_string(&tool).unwrap().len();
    tool.description = "x".repeat((3 + 4 - initial_len % 4) % 4);
    let bytes = serde_json::to_string(&tool).unwrap().len();
    assert_eq!(bytes % 4, 3);

    let tools = vec![tool; 4];
    let actual = estimate_tool_schema_tokens(&tools);
    assert_eq!(actual, legacy_estimate(&tools));
    assert_eq!(actual, (bytes / 4) * 4);
    assert_ne!(actual, (bytes * 4) / 4);
}

#[test]
fn selection_metadata_keeps_before_after_and_saved_estimates() {
    let candidates = vec![
        spec("zeta", "line\n😀", ToolSafety::ReadOnly)
            .with_metadata(json!({"nested": ["quote\"", "中"]})),
        spec("alpha", "short", ToolSafety::ReadOnly),
    ];
    let before = legacy_estimate(&candidates);
    let output = select("", 1, candidates.clone());
    let after = legacy_estimate(&output.tools);

    assert_eq!(output.tools, vec![candidates[1].clone()]);
    assert_eq!(
        output.metadata["estimated_schema_tokens_before"],
        json!(before)
    );
    assert_eq!(
        output.metadata["estimated_schema_tokens_after"],
        json!(after)
    );
    assert_eq!(
        output.metadata["estimated_schema_tokens_saved"],
        json!(before.saturating_sub(after))
    );
}

struct FailsAfterPrefix;

impl serde::Serialize for FailsAfterPrefix {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(2))?;
        sequence.serialize_element("already written")?;
        Err(serde::ser::Error::custom(
            "intentional serialization failure",
        ))
    }
}

#[test]
fn serialization_errors_are_not_converted_to_partial_lengths() {
    let expected = serde_json::to_string(&FailsAfterPrefix).unwrap_err();
    let actual = serialized_json_len(&FailsAfterPrefix).unwrap_err();
    assert_eq!(actual.classify(), expected.classify());
    assert_eq!(actual.to_string(), expected.to_string());

    let invalid_keys = std::collections::BTreeMap::from([(vec![1], "value")]);
    let expected = serde_json::to_string(&invalid_keys).unwrap_err();
    let actual = serialized_json_len(&invalid_keys).unwrap_err();
    assert_eq!(actual.classify(), expected.classify());
    assert_eq!(actual.to_string(), expected.to_string());
}

#[test]
fn counter_accepts_all_bytes_and_preserves_capacity_overflow_as_panic() {
    let mut counter = JsonByteCounter(0);
    assert_eq!(counter.write("中😀".as_bytes()).unwrap(), 7);
    assert_eq!(counter.0, 7);
    assert_eq!(counter.write(b"").unwrap(), 0);
    assert!(counter.flush().is_ok());

    let mut counter = JsonByteCounter(isize::MAX as usize - 1);
    assert_eq!(counter.write(b"x").unwrap(), 1);
    assert_eq!(counter.0, isize::MAX as usize);
    assert_eq!(counter.write(b"").unwrap(), 0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| counter.write(b"x")));
    assert!(
        result.is_err(),
        "capacity overflow must not become an ignored error"
    );
    assert_eq!(counter.0, isize::MAX as usize);

    // Also exercise arithmetic overflow directly without allocating huge input.
    let mut counter = JsonByteCounter(usize::MAX);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| counter.write(b"x")));
    assert!(result.is_err());
    assert_eq!(counter.0, usize::MAX);
}
