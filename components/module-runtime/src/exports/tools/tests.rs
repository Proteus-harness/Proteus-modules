use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use proteus_contracts::{
    contracts::{ProcessToolDefinition, ToolUserCommand},
    domain::{ToolResult, ToolSafety, ToolSpec},
    process_module::{ProcessModuleResult, ToolModule, ToolModuleHost, ToolModuleObject},
};
use serde_json::json;

use super::ToolCatalog;

struct Probe {
    definition: ProcessToolDefinition,
    raw_spec: Option<String>,
    reads: Arc<AtomicUsize>,
}

impl ToolModule for Probe {
    fn spec_json(&self) -> String {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.raw_spec
            .clone()
            .unwrap_or_else(|| serde_json::to_string(&self.definition.spec).unwrap())
    }

    fn model_visible(&self) -> bool {
        self.definition.model_visible
    }

    fn user_command(&self) -> Option<ToolUserCommand> {
        self.definition.user_command.clone()
    }

    fn invoke_json(
        &self,
        _call_json: String,
        _context_json: String,
        _host: &mut dyn ToolModuleHost,
    ) -> ProcessModuleResult<String> {
        Ok(serde_json::to_string(&ToolResult::ok(
            "probe-call".into(),
            self.definition.spec.description.clone(),
        ))
        .unwrap())
    }
}

struct NoHost;

impl ToolModuleHost for NoHost {
    fn is_cancelled(&self) -> ProcessModuleResult<bool> {
        Ok(false)
    }
}

fn invoke_at(tools: &[ToolModuleObject], index: usize) -> ToolResult {
    let output = tools[index]
        .invoke_json("{}".into(), "{}".into(), &mut NoHost)
        .unwrap();
    serde_json::from_str(&output).unwrap()
}

fn definition(name: &str, description: &str) -> ProcessToolDefinition {
    ProcessToolDefinition {
        spec: ToolSpec::new(
            name,
            description,
            json!({"type": "object"}),
            ToolSafety::ReadOnly,
        ),
        model_visible: true,
        user_command: None,
    }
}

fn probe(
    definition: ProcessToolDefinition,
    raw_spec: Option<String>,
    reads: &Arc<AtomicUsize>,
) -> ToolModuleObject {
    Box::new(Probe {
        definition,
        raw_spec,
        reads: Arc::clone(reads),
    })
}

fn uncached_index(tools: &[ToolModuleObject], name: &str) -> Option<usize> {
    // The previous dispatch algorithm, retained only for deterministic work
    // comparison. This is not a wall-clock benchmark.
    tools.iter().position(|tool| {
        serde_json::from_str::<ToolSpec>(&tool.spec_json()).is_ok_and(|spec| spec.name == name)
    })
}

#[test]
fn catalog_preserves_order_metadata_and_exact_name_lookup() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut hidden = definition("zeta", "hidden user command");
    hidden.model_visible = false;
    hidden.spec.input_schema = json!({
        "type": "object",
        "properties": {"arguments": {"type": "string"}},
        "required": ["arguments"]
    });
    hidden.user_command = Some(ToolUserCommand {
        name: "zeta".into(),
        description: "Run zeta".into(),
        arguments: "[ARGS]".into(),
    });
    hidden.spec = hidden.spec.with_parallel_tool_calls(true);
    let expected = vec![
        hidden,
        definition("Alpha", "case sensitive"),
        definition("alpha", "lowercase"),
        definition(" alpha", "not trimmed"),
    ];
    let tools = expected
        .iter()
        .cloned()
        .map(|definition| probe(definition, None, &reads))
        .collect::<Vec<_>>();
    let catalog = ToolCatalog::new(&tools);

    assert_eq!(catalog.definitions().unwrap(), expected);
    assert_eq!(catalog.definitions().unwrap(), expected);
    for (index, definition) in expected.iter().enumerate() {
        assert_eq!(catalog.index(&definition.spec.name), Some(index));
    }
    assert_eq!(catalog.index("ALPHA"), None);
    assert_eq!(catalog.index("alpha "), None);
    assert_eq!(catalog.index("missing"), None);
    assert_eq!(reads.load(Ordering::Relaxed), expected.len());
}

#[test]
fn duplicates_remain_in_list_and_dispatch_keeps_the_first_match() {
    let reads = Arc::new(AtomicUsize::new(0));
    let expected = vec![
        definition("repeat", "first handler"),
        definition("other", "middle handler"),
        definition("repeat", "last handler"),
    ];
    let tools = expected
        .iter()
        .cloned()
        .map(|definition| probe(definition, None, &reads))
        .collect::<Vec<_>>();
    let catalog = ToolCatalog::new(&tools);

    // Do not hide duplicates from Core's existing registration rejection.
    assert_eq!(catalog.definitions().unwrap(), expected);
    assert_eq!(catalog.index("repeat"), Some(0));
    assert_eq!(catalog.index("repeat"), uncached_index(&tools, "repeat"));
    assert_eq!(catalog.index("other"), Some(1));
    let selected = catalog.index("repeat").unwrap();
    assert_eq!(invoke_at(&tools, selected).output, "first handler");
}

#[test]
fn malformed_specs_still_fail_list_but_are_not_dispatch_candidates() {
    for raw_spec in [
        "{".to_owned(),
        {
            let mut value = serde_json::to_value(definition("repeat", "invalid").spec).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .remove("supports_parallel_tool_calls");
            value.to_string()
        },
        {
            let mut value = serde_json::to_value(definition("repeat", "invalid").spec).unwrap();
            value["unexpected"] = json!(true);
            value.to_string()
        },
    ] {
        let reads = Arc::new(AtomicUsize::new(0));
        let expected_error = serde_json::from_str::<ToolSpec>(&raw_spec)
            .unwrap_err()
            .to_string();
        let tools = vec![
            probe(definition("repeat", "invalid"), Some(raw_spec), &reads),
            probe(definition("repeat", "valid"), None, &reads),
            probe(
                definition("other", "later invalid"),
                Some("null".into()),
                &reads,
            ),
        ];
        let catalog = ToolCatalog::new(&tools);

        assert_eq!(
            catalog.definitions().unwrap_err().to_string(),
            expected_error
        );
        assert_eq!(
            catalog.definitions().unwrap_err().to_string(),
            expected_error
        );
        assert_eq!(catalog.index("repeat"), Some(1));
        assert_eq!(catalog.index("repeat"), uncached_index(&tools, "repeat"));
    }
}

#[test]
fn cache_does_not_relocate_host_schema_validation() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut invalid_schema = definition("probe", "host rejects this schema");
    invalid_schema.spec.input_schema = json!({"type": "not-a-schema-type"});
    let tools = vec![probe(invalid_schema.clone(), None, &reads)];
    let catalog = ToolCatalog::new(&tools);

    assert_eq!(catalog.definitions().unwrap(), vec![invalid_schema.clone()]);
    assert_eq!(catalog.index("probe"), Some(0));
    let validation = proteus_contracts::domain::tool_validation::validate_tool_input_schema(
        &invalid_schema.spec,
    );
    assert!(validation.is_err());
}

#[test]
fn each_loaded_export_owns_its_catalog() {
    let reads = Arc::new(AtomicUsize::new(0));
    let first = definition("same", "first config");
    let mut second = definition("same", "second config");
    second.model_visible = false;
    second.spec = second.spec.with_parallel_tool_calls(true);
    let first_tools = vec![probe(first.clone(), None, &reads)];
    let second_tools = vec![probe(second.clone(), None, &reads)];
    let first_catalog = ToolCatalog::new(&first_tools);
    let second_catalog = ToolCatalog::new(&second_tools);

    assert_eq!(first_catalog.definitions().unwrap(), vec![first]);
    assert_eq!(second_catalog.definitions().unwrap(), vec![second]);
    assert_eq!(first_catalog.index("same"), Some(0));
    assert_eq!(second_catalog.index("same"), Some(0));
    assert_eq!(reads.load(Ordering::Relaxed), 2);
}

#[test]
fn repeated_lookup_and_list_do_not_regenerate_or_reparse_specs() {
    const WIDTH: usize = 32;
    const REPEATS: usize = 100;
    let reads = Arc::new(AtomicUsize::new(0));
    let tools = (0..WIDTH)
        .map(|index| probe(definition(&format!("tool-{index}"), "probe"), None, &reads))
        .collect::<Vec<_>>();
    let last = format!("tool-{}", WIDTH - 1);

    // One original list plus a last-entry hit and a miss per iteration.
    for tool in &tools {
        serde_json::from_str::<ToolSpec>(&tool.spec_json()).unwrap();
    }
    for _ in 0..REPEATS {
        let selected = uncached_index(&tools, &last).unwrap();
        assert_eq!(selected, WIDTH - 1);
        assert_eq!(invoke_at(&tools, selected).output, "probe");
        assert_eq!(uncached_index(&tools, "missing"), None);
    }
    let uncached_reads = reads.swap(0, Ordering::Relaxed);
    assert_eq!(uncached_reads, WIDTH * (1 + 2 * REPEATS));

    let catalog = ToolCatalog::new(&tools);
    assert_eq!(catalog.definitions().unwrap().len(), WIDTH);
    for _ in 0..REPEATS {
        let selected = catalog.index(&last).unwrap();
        assert_eq!(selected, WIDTH - 1);
        assert_eq!(invoke_at(&tools, selected).output, "probe");
        assert_eq!(catalog.index("missing"), None);
    }
    assert_eq!(catalog.definitions().unwrap().len(), WIDTH);
    assert_eq!(reads.load(Ordering::Relaxed), WIDTH);
}

#[test]
fn empty_catalog_remains_empty() {
    let catalog = ToolCatalog::new(&[]);
    assert!(catalog.definitions().unwrap().is_empty());
    assert_eq!(catalog.index("missing"), None);
}
