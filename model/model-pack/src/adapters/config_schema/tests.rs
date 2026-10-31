use super::{Field, Kind, config_schema};
use serde_json::{Value, json};

const CAPABILITY_KEYS: [&str; 6] = [
    "supports_image_input",
    "supports_parallel_tool_calls",
    "supports_freeform_tools",
    "supports_json_schema",
    "supports_reasoning_config",
    "support_verbosity",
];

fn schema_fields(config: &Value) -> Vec<Field> {
    config_schema(config).fields
}

fn capability_fields(fields: &[Field]) -> &[Field] {
    let group = fields
        .iter()
        .find(|field| field.key == "capabilities")
        .unwrap();
    assert!(group.advanced);
    let Kind::Object { fields } = &group.value else {
        panic!("capabilities should remain an object");
    };
    fields
}

fn field<'a>(fields: &'a [Field], key: &str) -> &'a Field {
    fields.iter().find(|field| field.key == key).unwrap()
}

#[test]
fn nested_capabilities_show_the_effective_existing_values_without_flat_duplicates() {
    for implementation in ["openai", "openai_compatible", "openai_codex"] {
        let mut config = json!({"implementation": implementation, "capabilities": {}});
        for (index, key) in CAPABILITY_KEYS.into_iter().enumerate() {
            config[key] = json!(index % 2 == 0);
            config["capabilities"][key] = json!(index % 2 != 0);
        }
        let original = config.clone();
        let fields = schema_fields(&config);
        let nested = capability_fields(&fields);
        for (index, key) in CAPABILITY_KEYS.into_iter().enumerate() {
            assert!(fields.iter().all(|field| field.key != key));
            assert_eq!(field(nested, key).default, Some(json!(index % 2 != 0)));
        }
        assert_eq!(
            config, original,
            "presentation must not rewrite stored config"
        );
    }
}

#[test]
fn flat_capability_values_remain_visible_as_nested_defaults_until_overridden() {
    let mut config = json!({"implementation": "openai_codex", "capabilities": {}});
    for key in CAPABILITY_KEYS {
        config[key] = json!(true);
    }
    let fields = schema_fields(&config);
    for key in CAPABILITY_KEYS {
        assert_eq!(
            field(capability_fields(&fields), key).default,
            Some(json!(true))
        );
    }
    let defaults = schema_fields(&json!({"implementation":"openai_codex"}));
    for key in CAPABILITY_KEYS {
        assert_eq!(
            field(capability_fields(&defaults), key).default,
            Some(json!(false))
        );
    }
}

#[test]
fn an_invalid_explicit_nested_value_is_not_masked_by_flat_fallback() {
    let config = json!({
        "implementation": "openai_codex",
        "supports_image_input": true,
        "capabilities": {"supports_image_input": null}
    });
    let fields = schema_fields(&config);
    assert_eq!(
        field(capability_fields(&fields), "supports_image_input").default,
        Some(Value::Null)
    );
}

#[test]
fn hosted_tool_capabilities_and_tool_configuration_remain_distinct() {
    let config = json!({
        "implementation": "openai_codex",
        "capabilities": {"hosted_tools": ["web_search"]},
        "hosted_tools": {"web_search": {"include_sources": true}}
    });
    let original = config.clone();
    let fields = schema_fields(&config);
    assert_eq!(
        field(capability_fields(&fields), "hosted_tools").default,
        Some(json!(["web_search"]))
    );
    assert!(matches!(
        field(&fields, "hosted_tools").value,
        Kind::Object { .. }
    ));
    assert_eq!(config, original);

    // A top-level array is not an accepted capability fallback: the runtime
    // interprets that key exclusively as the hosted-tool settings object.
    let no_nested =
        schema_fields(&json!({"implementation": "openai", "hosted_tools": ["file_search"]}));
    assert_eq!(
        field(capability_fields(&no_nested), "hosted_tools").default,
        Some(json!([]))
    );
}

#[test]
fn technical_transport_authentication_and_cache_metadata_stay_advanced() {
    for implementation in ["openai", "openai_compatible", "openai_codex", "anthropic"] {
        let fields = schema_fields(&json!({"implementation":implementation}));
        for key in [
            "base_url",
            "auth_file",
            "api_key_env",
            "auth",
            "prompt_cache_key",
            "client_metadata",
            "request_max_retries",
            "http1_only",
        ] {
            if let Some(field) = fields.iter().find(|field| field.key == key) {
                assert!(field.advanced, "{implementation}.{key} should be advanced");
            }
        }
        if implementation == "anthropic" {
            assert!(fields.iter().all(|field| field.key != "capabilities"));
        }
    }
}
