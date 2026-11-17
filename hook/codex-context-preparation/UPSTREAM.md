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
Перед выдачей модели устаревший пример `apply_patch` с `command:[...]` из
fallback-шаблона переводится в фактический binding из `request.tools`:
function с `patch:string` получает JSON arguments, freeform с `input:string` —
пример raw patch без JSON wrapper. При отсутствии такого binding или старого
примера текст остаётся прежним. Это локальная адаптация model-facing текста;
файлы upstream и их SHA256 не меняются, выбор по model ID не определяет
форму вызова tool.
Конфигурация hook явная, она не импортирует обновляемые remote instructions.
Каталог native CLI, полученный с сервера, может отличаться от этого публичного
pin. Равенство установленному CLI проверяется отдельным differential gate.

`codex_compact` продолжает реализовывать локальный алгоритм предыдущего pin.
Текущая native remote compaction и lifecycle code-mode-host имеют отдельные
границы исполнения и не подтверждаются тестом выбора base instructions.

## Локальная И Удалённая Компактация

Уже compactor pin `67cc3c3` выбирает удалённую V2-компактацию для OpenAI/Azure;
feature `remote_compaction_v2` включена по умолчанию. При её выключении тот же
provider использует прежний `POST responses/compact`, а provider без этой
capability — локальный summary. Источники: [выбор ветки](https://github.com/openai/codex/blob/67cc3c318dc8b5532db6ade4182b1dc6f3870889/codex-rs/core/src/tasks/compact.rs#L41-L76),
[capability провайдера](https://github.com/openai/codex/blob/67cc3c318dc8b5532db6ade4182b1dc6f3870889/codex-rs/model-provider/src/provider.rs#L353-L364)
и [default feature](https://github.com/openai/codex/blob/67cc3c318dc8b5532db6ade4182b1dc6f3870889/codex-rs/features/src/lib.rs#L1656-L1661).
Локальная реализация Proteus — выбранная ветка этого pin, а не полный паритет
компактации нативного ChatGPT-профиля Codex.

V2 добавляет последним input item `compaction_trigger` в обычный Responses
запрос и получает `compaction` с `encrypted_content`; это непрозрачный
checkpoint для следующих запросов, а не текстовый summary. Такой explicit
trigger также описан в [официальном Responses API](https://developers.openai.com/api/docs/guides/compaction#request-compaction-with-an-explicit-trigger).
Наш [summary request](src/summary.rs) делает обычный completion без tools.
У `openai_codex` уже есть OAuth-транспорт `chatgpt.com/backend-api/codex/responses`,
но удалённая компактация требует операции/capability в model contract,
кодирования trigger, чтения и повторной отправки checkpoint через model adapter.
Сейчас OpenAI adapter отклоняет canonical `Opaque` на входе и не декодирует
compaction item на выходе; напрямую вызывать HTTP из hook в обход model boundary
нельзя.

Для DeepSeek через OpenCode Go поддержка remote-компактации Codex не заявлена;
локальная ветка соответствует fallback для provider без этой capability.
Зашифрованный OpenAI checkpoint нельзя прозрачно подставить вместо summary
в запрос DeepSeek. Наличие OAuth-маршрута и описанного протокола само по себе
не подтверждает доступ remote-компактации для конкретного аккаунта и модели.
