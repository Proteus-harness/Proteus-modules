use proteus_contracts::domain::{ConfigField, ConfigValueSchema, ModuleConfigSchema};
pub fn config_schema() -> ModuleConfigSchema {
    let defaults = crate::config::DynamicSelectionConfig::default();
    ModuleConfigSchema {
        fields: vec![
            ConfigField::new(
                "max_hot_tools",
                "Активные инструменты",
                "Размер набора инструментов, доступного модели на текущем шаге.",
                ConfigValueSchema::integer(1),
            )
            .with_default(defaults.max_hot_tools as u64),
            ConfigField::new(
                "always_include",
                "Всегда доступные",
                "Имена инструментов, включаемых независимо от поиска.",
                ConfigValueSchema::strings(),
            )
            .suggest_tools()
            .with_default(serde_json::json!(defaults.always_include)),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proteus_contracts::domain::ConfigSuggestions;

    #[test]
    fn exact_always_include_names_discover_registered_tools() {
        let schema = config_schema();
        schema.validate().unwrap();
        let names = schema
            .fields
            .iter()
            .find(|field| field.key == "always_include")
            .unwrap();
        assert_eq!(names.suggestions, Some(ConfigSuggestions::Tools {}));
        assert!(matches!(names.value, ConfigValueSchema::Array { .. }));
    }
}
