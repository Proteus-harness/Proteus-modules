# Происхождение И Проверенная Поверхность

Адаптер и тестовые fixtures написаны для Proteus, под лицензией корневого
репозитория (MIT). Сторонний runtime/код hooks сюда не скопирован.

## Runtime SDK

- Репозиторий: [Proteus-core](https://github.com/Proteus-harness/Proteus-core).
- Revision: `bee0e409e746e2df9db32748d4f8ea02201bd4af`.
- Public JS entry API: `examples/modules/hook-process`; `hook/v6`, wire v3.
- Лицензия: [MIT](https://github.com/Proteus-harness/Proteus-core/blob/bee0e409e746e2df9db32748d4f8ea02201bd4af/LICENSE).
- Helper/runtime digests: [sdk-pin.json](sdk-pin.json). Подключение — явный
  локальный путь; автоматического скачивания или npm install нет.

## Реальный Внешний Пример

- Источник: `anthropics/claude-plugins-official`.
- Revision: `58267944f92187b7d6a1c822165c96c81c864806`.
- Путь: `plugins/plugin-dev/skills/hook-development/examples/validate-bash.sh`.
- Git blob: `e36432441f6d541ac3a76a3cffdffa1edfa671d0`.
- SHA-256 UTF-8 bytes: `348c58f4cf38a2e7080ea5272935a7658c7122d83dd6c52d0a1f2ef599c176ab`.
- [Исходный файл](https://github.com/anthropics/claude-plugins-official/blob/58267944f92187b7d6a1c822165c96c81c864806/plugins/plugin-dev/skills/hook-development/examples/validate-bash.sh).
- [Лицензия plugin-dev, Apache-2.0](https://github.com/anthropics/claude-plugins-official/blob/58267944f92187b7d6a1c822165c96c81c864806/plugins/plugin-dev/LICENSE).

Прочитан как текст, **не исполнен**. Нужны Bash и jq. Скрипт использует
`tool_input.command`: возвращает пустой stdout или `continue:true` с exit 0;
exit 2 передаёт непустой stderr. Включая JSON-подобную строку с `ask` в stderr:
это причина блокировки по exit 2, а не просьба выдать разрешение. Эти форматы
покрыты собственными fixtures без исполнения upstream. Его исходные байты
владельцу менять не требуется. Пример JSON в этом каталоге написан нами, а не
выдан за опубликованный upstream manifest.

Это демонстрационный validator с ограниченными строковыми эвристиками;
совместимость формата не является доказательством безопасности команд.

## Источники Спецификации

Сверены 24 октября 2026:

- [Claude hook configuration, matcher и command fields](https://code.claude.com/docs/en/hooks).
- [Claude plugin manifest](https://code.claude.com/docs/en/plugins-reference).
- [Agent Plugins 1.0](https://agent-plugins.org/specification): стандартный импорт
  Skills/MCP — отдельная задача. Неизвестные client extensions игнорируются;
  этот адаптер не меняет importer и не исполняет plugin.json.

`strict_subset` намеренно отличается от upstream в обработке ошибок и
ограничении одного matching handler. Список границ в README является частью
контракта модуля; наличие общей формы JSON не означает полной совместимости.
