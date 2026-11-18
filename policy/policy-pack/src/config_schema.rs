use proteus_contracts::domain::{ConfigField, ConfigValueSchema, ModuleConfigSchema};
pub fn config_schema(id: &str) -> ModuleConfigSchema {
    let mut fields = Vec::new();
    if id == "opencode_policy" {
        return ModuleConfigSchema {
            fields: vec![
                ConfigField::new(
                    "rules",
                    "Правила",
                    "Правила проверяются по порядку; последнее совпадение определяет действие.",
                    ConfigValueSchema::Array {
                        items: Box::new(ConfigValueSchema::Object {
                            fields: vec![
                                ConfigField::new(
                                    "permission",
                                    "Группа",
                                    "Имя группы разрешений.",
                                    ConfigValueSchema::text(),
                                )
                                .required(),
                                ConfigField::new(
                                    "pattern",
                                    "Шаблон",
                                    "Шаблон команды или пути.",
                                    ConfigValueSchema::text(),
                                )
                                .with_default("*"),
                                ConfigField::new(
                                    "action",
                                    "Действие",
                                    "Что делать при совпадении.",
                                    ConfigValueSchema::choices(&[
                                        ("allow", "Разрешить"),
                                        ("ask", "Спросить"),
                                        ("deny", "Запретить"),
                                    ]),
                                )
                                .required(),
                            ],
                        }),
                    },
                )
                .with_default(serde_json::json!([])),
                ConfigField::new(
                    "groups",
                    "Группы инструментов",
                    "Именованные группы: tools, pattern_args и split_commands.",
                    ConfigValueSchema::Json {},
                )
                .with_default(serde_json::json!({}))
                .advanced(),
            ],
        };
    }
    if id != "allow_all" {
        for (key, description) in [
            ("allow", "Инструменты, выполняемые без подтверждения."),
            ("ask_before", "Инструменты, требующие подтверждения."),
            ("deny", "Инструменты, выполнение которых запрещено."),
            (
                "allow_sandboxed",
                "Инструменты со своей изоляцией; расширение прав требует подтверждения.",
            ),
        ] {
            if id == "ask_write" && matches!(key, "deny" | "allow_sandboxed") {
                continue;
            }
            fields.push(
                ConfigField::new(key, key, description, ConfigValueSchema::strings())
                    .with_default(serde_json::json!([]))
                    .suggest_tools(),
            );
        }
    }
    ModuleConfigSchema { fields }
}
