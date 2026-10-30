use proteus_contracts::process_module::*;
use serde_json::json;

use super::*;

struct Registry {
    config: Value,
    tools: Vec<ToolModuleObject>,
}
impl ModuleRegistry for Registry {
    fn module_config(&self) -> &Value {
        &self.config
    }
    fn register_tool(&mut self, tool: ToolModuleObject) -> ProcessModuleResult<()> {
        self.tools.push(tool);
        Ok(())
    }
    fn register_model(&mut self, _: String, _: ModelModuleObject) -> ProcessModuleResult<()> {
        unreachable!()
    }
    fn register_policy(&mut self, _: String, _: PolicyModuleObject) -> ProcessModuleResult<()> {
        unreachable!()
    }
    fn register_context_provider(
        &mut self,
        _: String,
        _: ContextProviderModuleObject,
    ) -> ProcessModuleResult<()> {
        unreachable!()
    }
    fn register_context(
        &mut self,
        _: String,
        _: ContextBuilderModuleObject,
    ) -> ProcessModuleResult<()> {
        unreachable!()
    }
    fn register_hook(&mut self, _: String, _: HookModuleObject) -> ProcessModuleResult<()> {
        unreachable!()
    }
    fn register_workflow(&mut self, _: String, _: WorkflowModuleObject) -> ProcessModuleResult<()> {
        unreachable!()
    }
}

fn configured(directory: &std::path::Path) -> Value {
    json!({"source_capture": {"mode":"ephemeral_file", "directory":directory,
        "max_capture_bytes":1024, "max_captures":2, "retention_seconds":300}})
}

#[test]
fn default_binding_has_exact_original_three_tools_and_no_retention() {
    let mut registry = Registry {
        config: json!({}),
        tools: Vec::new(),
    };
    assert!(CaptureConfig::parse(&registry.config).unwrap().is_none());
    crate::register_modules(&mut registry).unwrap();
    let specs: Vec<Value> = registry
        .tools
        .iter()
        .map(|tool| serde_json::from_str(&tool.spec_json()).unwrap())
        .collect();
    assert_eq!(
        specs
            .iter()
            .map(|spec| spec["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["shell", "exec_command", "write_stdin"]
    );
    assert!(
        !specs[1]["description"]
            .as_str()
            .unwrap()
            .contains("retains")
    );
}

#[test]
fn enabled_catalog_adds_only_readonly_range_tool_with_no_new_slot() {
    let directory = tempfile::tempdir().unwrap();
    let mut registry = Registry {
        config: configured(directory.path()),
        tools: Vec::new(),
    };
    crate::register_modules(&mut registry).unwrap();
    let specs: Vec<Value> = registry
        .tools
        .iter()
        .map(|tool| serde_json::from_str(&tool.spec_json()).unwrap())
        .collect();
    assert_eq!(specs.len(), 4);
    assert_eq!(specs[3]["name"], "read_exec_capture");
    assert_eq!(specs[3]["safety"], "ReadOnly");
    assert_eq!(specs[1]["safety"], "RunsCommands");
    assert!(
        specs[1]["description"]
            .as_str()
            .unwrap()
            .contains("ephemeral")
    );
    // Binding alone creates neither writer nor a file; admission is per command.
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn invalid_optin_rejects_before_partial_tool_registration() {
    let directory = tempfile::tempdir().unwrap();
    for bad in [json!(null), json!({}), json!({"mode":"ephemeral_file"})] {
        let mut registry = Registry {
            config: json!({"source_capture":bad}),
            tools: Vec::new(),
        };
        assert!(crate::register_modules(&mut registry).is_err());
        assert!(registry.tools.is_empty());
    }
    for (key, bad) in [
        ("max_capture_bytes", json!(0)),
        ("max_captures", json!(17)),
        ("retention_seconds", json!(1801)),
        ("directory", json!("relative")),
        ("mode", json!("durable")),
    ] {
        let mut config = configured(directory.path());
        config["source_capture"][key] = bad;
        assert!(CaptureConfig::parse(&config).is_err(), "{key}");
    }
    let mut config = configured(directory.path());
    config["source_capture"]["max_capture_bytes"] = json!(64 * MIB);
    config["source_capture"]["max_captures"] = json!(16);
    assert!(CaptureConfig::parse(&config).is_err());
    config["source_capture"]["max_captures"] = json!(4);
    assert!(CaptureConfig::parse(&config).is_ok());
}

#[test]
fn schema_describes_optional_object_without_silently_enabling_defaults() {
    let schema = config_schema();
    schema.validate().unwrap();
    assert_eq!(schema.fields.len(), 1);
    let capture = &schema.fields[0];
    assert_eq!(capture.key, "source_capture");
    assert!(!capture.required);
    assert!(capture.default.is_none());
    let ConfigValueSchema::Object { fields } = &capture.value else {
        panic!("capture object schema")
    };
    assert_eq!(fields.len(), 5);
    assert!(
        fields
            .iter()
            .all(|field| field.required && field.default.is_none())
    );
}
