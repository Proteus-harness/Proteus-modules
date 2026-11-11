# Источник Шаблонов

`src/upstream/compact_prompt.md` и `src/upstream/summary_prefix.md` скопированы
без изменений из `codex-rs/prompts/templates/compact/{prompt.md,summary_prefix.md}`
OpenAI Codex на commit `67cc3c318dc8b5532db6ade4182b1dc6f3870889`.

[Исходный код](https://github.com/openai/codex/tree/67cc3c318dc8b5532db6ade4182b1dc6f3870889)
распространяется под Apache-2.0. Копии [LICENSE](src/upstream/LICENSE) и
[NOTICE](src/upstream/NOTICE) включены рядом с шаблонами.

## Model-Specific Base Instructions

`src/upstream/0.167.0/{gpt-6.1-sol,gpt-6.1-terra,gpt-5.6-luna}.md` содержат
точные `model_messages.instructions_template` из публичного
`codex-rs/models-manager/models.json` на `rust-v0.167.0`, commit
`086ccc833bc9cf01024ad2dd6af7dbd421b6bf9a`. `default.md` взят из
`codex-rs/protocol/src/prompts/base_instructions/default.md` того же commit.
Содержимое не сокращено и не извлечено из персональных инструкций сессии.
SHA256 сохранены в `provenance.json`; Apache-2.0 LICENSE находится рядом.

`codex_base_instructions` выбирает шаблон по точному model ID на `BeforeModel`
прямого model call. Пользовательские instructions и messages сохраняются.
Конфигурация hook явная, она не импортирует обновляемые remote instructions.
Каталог native CLI, полученный с сервера, может отличаться от этого публичного
pin. Равенство установленному CLI проверяется отдельным differential gate.

`codex_compact` продолжает реализовывать локальный алгоритм предыдущего pin.
Текущая native remote compaction и lifecycle code-mode-host имеют отдельные
границы исполнения и не подтверждаются тестом выбора base instructions.
