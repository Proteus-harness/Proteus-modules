//! Provider configuration presentation stays with the HTTP implementations.
use proteus_contracts::domain::{
    ConfigField as Field, ConfigValueSchema as Kind, ModuleConfigSchema,
};
use serde_json::Value;

pub fn config_schema(config: &Value) -> ModuleConfigSchema {
    let implementation = config
        .get("implementation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut fields = vec![Field::new("implementation","Подключение","Способ подключения модели. После сохранения форма обновится для выбранного подключения.",Kind::choices(&[
        ("openai","OpenAI API"),("openai_compatible","OpenAI-совместимый API"),("openai_codex","Подписка ChatGPT"),("anthropic","Anthropic API"),("fake","Тестовая модель"),
    ])).required()];
    if implementation == "fake" {
        fields.push(
            Field::new(
                "stream_delay_ms",
                "Задержка ответа",
                "Задержка между фрагментами тестового ответа.",
                Kind::integer(0),
            )
            .unit("мс"),
        );
        return ModuleConfigSchema { fields };
    }
    let codex = implementation == "openai_codex";
    let anthropic = implementation == "anthropic";
    fields.push(
        Field::new(
            "base_url",
            "Адрес сервера",
            "Базовый адрес API для этого подключения; не определяет выбранную модель.",
            Kind::text(),
        )
        .with_default(if codex {
            super::codex_auth::CODEX_BASE_URL
        } else if anthropic {
            "https://api.anthropic.com"
        } else {
            "https://api.openai.com/v1"
        })
        .advanced(),
    );
    if codex {
        fields.push(
            Field::new(
                "auth_file",
                "Файл авторизации",
                "Без явного пути используется стандартное хранилище авторизации Proteus.",
                Kind::text(),
            )
            .advanced(),
        );
        fields.push(
            Field::new(
                "quota_url",
                "Адрес лимитов",
                "Адрес API расхода подписки.",
                Kind::text(),
            )
            .advanced(),
        );
        fields.push(
            Field::new(
                "client_version",
                "Версия клиента для каталога",
                "Версия Codex, с которой запрашивается список моделей. Новые модели \
                 видны только клиентам новее определённой версии.",
                Kind::text(),
            )
            .with_default(super::codex_auth::CODEX_CLIENT_VERSION)
            .advanced(),
        );
        fields.push(
            Field::new(
                "oauth_issuer",
                "Сервер авторизации",
                "Без явного значения используется стандартный сервер авторизации.",
                Kind::text(),
            )
            .advanced(),
        );
    } else {
        fields.push(
            Field::new(
                "api_key_env",
                "Переменная с ключом",
                "Имя переменной окружения процесса модуля.",
                Kind::text(),
            )
            .with_default(if anthropic {
                "ANTHROPIC_API_KEY"
            } else {
                "OPENAI_API_KEY"
            })
            .advanced(),
        );
        fields.push(
            Field::new(
                "api_key_file",
                "Файл с ключом",
                "JSON-файл с ключом API.",
                Kind::text(),
            )
            .advanced(),
        );
        fields.push(
            Field::new(
                "api_key_json_key",
                "Поле ключа в JSON",
                "Имя поля в файле с ключом.",
                Kind::text(),
            )
            .with_default("api_key")
            .advanced(),
        );
        fields.push(
            Field::new(
                "api_key",
                "Ключ API",
                "Явное значение имеет приоритет над файлом и переменной.",
                Kind::String {
                    multiline: false,
                    secret: true,
                },
            )
            .advanced(),
        );
        for (key, title) in [
            ("base_url_file", "Файл с адресом сервера"),
            ("base_url_env", "Переменная с адресом сервера"),
            ("base_url_json_key", "Поле адреса в JSON"),
        ] {
            fields.push(
                Field::new(
                    key,
                    title,
                    "Используется при отсутствии явного base_url.",
                    Kind::text(),
                )
                .advanced(),
            );
        }
    }
    fields.push(
        Field::new(
            "prompt_cache",
            "Кэш запросов",
            "Передавать провайдеру подсказки кэширования. Попадание в кэш определяет сервер; включение не гарантирует cache hit.",
            Kind::Boolean {},
        )
        .with_default(true),
    );
    if anthropic {
        fields.push(
            Field::new(
                "auth",
                "Авторизация",
                "Заголовок для передачи ключа.",
                Kind::choices(&[("x-api-key", "X-API-Key"), ("bearer", "Bearer")]),
            )
            .with_default("x-api-key")
            .advanced(),
        );
        fields.push(
            Field::new(
                "api_version",
                "Версия API",
                "Версия протокола Anthropic.",
                Kind::text(),
            )
            .with_default("2023-06-01")
            .advanced(),
        );
        fields.push(
            Field::new(
                "prompt_cache_ttl",
                "Время хранения кэша",
                "Без явного значения срок выбирает провайдер.",
                Kind::text(),
            )
            .advanced(),
        );
    } else {
        fields.push(
            Field::new(
                "max_input_tokens",
                "Бюджет входного контекста",
                "Локальный предел входных токенов, объявляемый адаптером. Используется для бюджета контекста в Proteus; не увеличивает окно модели на сервере. Без значения адаптер не объявляет свой предел.",
                Kind::Integer {
                    minimum: Some(1),
                    maximum: Some(u32::MAX as i64),
                },
            )
            .unit("токенов")
            .advanced(),
        );
        fields.push(Field::new("request_max_retries","Повторы запроса","Повторы при транспортных ошибках и HTTP 5xx; значения выше 100 ограничиваются сотней.",Kind::integer(0)).with_default(4).advanced());
        fields.push(
            Field::new(
                "stream_idle_timeout_ms",
                "Ожидание фрагмента",
                "Срок ожидания следующего события ответа.",
                Kind::integer(0),
            )
            .with_default(300_000)
            .unit("мс")
            .advanced(),
        );
        fields.push(
            Field::new(
                "http1_only",
                "Только HTTP/1.1",
                "Ограничить протокол соединения с сервером.",
                Kind::Boolean {},
            )
            .with_default(false)
            .advanced(),
        );
        if !codex {
            fields.push(
                Field::new(
                    "stream_error_fallback",
                    "Повтор после ошибки потока",
                    "Диагностический режим: повторить полный запрос после ошибки SSE.",
                    Kind::Boolean {},
                )
                .with_default(false)
                .advanced(),
            );
        }
        fields.push(
            Field::new(
                "prompt_cache_key",
                "Ключ кэша",
                "Явный ключ, передаваемый провайдеру для маршрутизации кэша. Без значения адаптер формирует ключ из запроса; совпадение ключа не гарантирует попадание в кэш.",
                Kind::text(),
            )
            .advanced(),
        );
        if !codex {
            fields.push(
                Field::new(
                    "prompt_cache_retention",
                    "Хранение кэша",
                    "Значение, передаваемое провайдеру.",
                    Kind::text(),
                )
                .advanced(),
            );
        }
        let capabilities = config.get("capabilities");
        let mut nested: Vec<_> = [
            (
                "supports_image_input",
                "Изображения",
                "Разрешить адаптеру передавать изображения во входном запросе.",
            ),
            (
                "supports_parallel_tool_calls",
                "Параллельные инструменты",
                "Разрешить параметр параллельных вызовов инструментов в запросе.",
            ),
            (
                "supports_freeform_tools",
                "Инструменты с текстовым вводом",
                "Разрешить описание инструментов со свободным текстовым вводом вместо JSON.",
            ),
            (
                "supports_json_schema",
                "Структурированный ответ",
                "Разрешить формат структурированного ответа по JSON Schema.",
            ),
            (
                "supports_reasoning_config",
                "Настройка рассуждений",
                "Разрешить передачу параметров рассуждений в запросе.",
            ),
            (
                "support_verbosity",
                "Настройка подробности",
                "Разрешить передачу verbosity и default_verbosity провайдеру.",
            ),
        ]
        .into_iter()
        .map(|(key, title, description)| {
            // Match the adapter's existing precedence without changing config:
            // an explicit nested value wins, otherwise the existing flat key.
            let default = capabilities
                .and_then(|capabilities| capabilities.get(key))
                .or_else(|| config.get(key))
                .cloned()
                .unwrap_or(Value::Bool(false));
            Field::new(key, title, description, Kind::Boolean {}).with_default(default)
        })
        .collect();
        let hosted_kinds = capabilities
            .and_then(|capabilities| capabilities.get("hosted_tools"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]));
        nested.push(Field::new(
            "hosted_tools",
            "Поддерживаемые инструменты провайдера",
            "Вручную объявленные семейства инструментов. Их параметры задаются отдельно в разделе инструментов провайдера; доступность на сервере автоматически не проверяется.",
            Kind::Array {
                items: Box::new(Kind::choices(&[
                    ("web_search", "Поиск в интернете"),
                    ("file_search", "Поиск в файлах"),
                ])),
            },
        ).with_default(hosted_kinds));
        fields.push(
            Field::new(
                "capabilities",
                "Формат запросов",
                "Ручные настройки формата запросов адаптера. Это не автоматически определённые способности выбранной модели: включайте только параметры, которые принимает ваш сервер и модель.",
                Kind::Object { fields: nested },
            )
            .advanced(),
        );
        for (key, title) in [
            ("verbosity", "Подробность ответа"),
            ("default_verbosity", "Подробность по умолчанию"),
        ] {
            fields.push(
                Field::new(
                    key,
                    title,
                    "Требует включённой настройки подробности.",
                    Kind::choices(&[
                        ("low", "Кратко"),
                        ("medium", "Обычно"),
                        ("high", "Подробно"),
                    ]),
                )
                .advanced(),
            );
        }
        fields.push(
            Field::new(
                "service_tier",
                "Уровень обслуживания",
                "Значение, передаваемое провайдеру.",
                Kind::text(),
            )
            .advanced(),
        );
        fields.push(
            Field::new(
                "client_metadata",
                "Метаданные клиента",
                "Объект с произвольными строковыми значениями.",
                Kind::Json {},
            )
            .with_default(serde_json::json!({}))
            .advanced(),
        );
        fields.push(Field::new("hosted_tools", "Инструменты провайдера", "Для включения инструмента задайте его параметры и соответствующую возможность модели.", Kind::Object { fields: vec![
            Field::new("max_tool_calls", "Число вызовов", "Общий максимум вызовов инструментов провайдера за запрос.", Kind::Integer { minimum: Some(1), maximum: Some(u32::MAX as i64) }),
            Field::new("web_search", "Поиск в интернете", "Настройки поиска на стороне провайдера.", Kind::Object { fields: vec![
                Field::new("search_context_size", "Объём поиска", "Размер поискового контекста.", Kind::choices(&[("low","Небольшой"),("medium","Обычный"),("high","Большой")])),
                Field::new("allowed_domains", "Разрешённые домены", "Ограничить поиск этими доменами.", Kind::strings()).with_default(serde_json::json!([])),
                Field::new("blocked_domains", "Исключённые домены", "Домены, которые поиск должен пропустить.", Kind::strings()).with_default(serde_json::json!([])),
                Field::new("external_web_access", "Внешний интернет", "Разрешить обращение к актуальным интернет-источникам.", Kind::Boolean {}),
                Field::new("include_sources", "Показывать источники", "Включать сведения об источниках в ответ.", Kind::Boolean {}).with_default(false),
            ] }),
            Field::new("file_search", "Поиск в файлах", "Настройки поиска в хранилищах провайдера.", Kind::Object { fields: vec![
                Field::new("vector_store_ids", "Хранилища", "Идентификаторы vector stores, по которым выполняется поиск.", Kind::strings()).required(),
                Field::new("max_num_results", "Число результатов", "Максимум найденных фрагментов.", Kind::Integer { minimum: Some(1), maximum: Some(u32::MAX as i64) }),
                Field::new("include_results", "Включать результаты", "Возвращать найденные фрагменты в ответе.", Kind::Boolean {}).with_default(false),
            ] }),
        ] }).advanced());
    }
    ModuleConfigSchema { fields }
}

#[cfg(test)]
mod tests;
