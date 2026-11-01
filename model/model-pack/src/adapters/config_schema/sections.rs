//! Presentation groups do not add nesting to the provider's configuration.
use super::{Field, ModuleConfigSchema};
use proteus_contracts::domain::ConfigSection;

pub(super) fn group(mut fields: Vec<Field>) -> ModuleConfigSchema {
    for field in &mut fields {
        let metadata = match field.key.as_str() {
            "implementation" | "base_url" | "base_url_file" | "base_url_env"
            | "base_url_json_key" => (
                "connection",
                "Подключение",
                "Провайдер и адрес его API. Выбор модели находится в чате.",
            ),
            "auth_file" | "api_key_env" | "api_key_file" | "api_key_json_key" | "api_key"
            | "auth" | "oauth_issuer" => (
                "authentication",
                "Авторизация",
                "Основной источник авторизации и альтернативные способы передачи ключа.",
            ),
            "prompt_cache" | "prompt_cache_key" | "prompt_cache_retention" | "prompt_cache_ttl" => {
                (
                    "cache",
                    "Кэширование",
                    "Подсказки серверу для повторного использования входного контекста.",
                )
            }
            "max_input_tokens" | "verbosity" | "default_verbosity" | "service_tier" => (
                "response",
                "Ответ модели",
                "Локальный бюджет контекста и параметры ответа, принимаемые провайдером.",
            ),
            "capabilities" | "hosted_tools" | "api_version" => (
                "api_compatibility",
                "Совместимость API",
                "Ручные разрешения формата запросов и инструменты сервера; возможности модели автоматически не проверяются.",
            ),
            "quota_url"
            | "client_version"
            | "request_max_retries"
            | "stream_idle_timeout_ms"
            | "http1_only"
            | "stream_error_fallback"
            | "client_metadata"
            | "stream_delay_ms" => (
                "network",
                "Сеть и диагностика",
                "Транспорт, каталог моделей и технические параметры подключения.",
            ),
            _ => continue,
        };
        field.section = Some(ConfigSection {
            id: metadata.0.into(),
            title: metadata.1.into(),
            description: metadata.2.into(),
        });
    }
    // Preserve field order inside each group and put everyday settings first.
    fields.sort_by_key(
        |field| match field.section.as_ref().map(|section| section.id.as_str()) {
            Some("connection") => 0,
            Some("authentication") => 1,
            Some("cache") => 2,
            Some("response") => 3,
            Some("api_compatibility") => 4,
            Some("network") => 5,
            _ => 6,
        },
    );
    ModuleConfigSchema { fields }
}
