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
прямого model call и auxiliary-запроса с typed operation `Compact`. Обычный
auxiliary summary этот hook пропускает. Пользовательские instructions и messages
сохраняются.
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

`codex_compact` реализует локальную и remote V2 ветки compactor pin `67cc3c3`.
Lifecycle code-mode-host имеет отдельную границу исполнения и не подтверждается
тестом выбора base instructions.

## Локальная И Удалённая Компактация

Уже compactor pin `67cc3c3` выбирает удалённую V2-компактацию для OpenAI/Azure;
feature `remote_compaction_v2` включена по умолчанию. При её выключении тот же
provider использует прежний `POST responses/compact`, а provider без этой
capability — локальный summary. Источники: [выбор ветки](https://github.com/openai/codex/blob/67cc3c318dc8b5532db6ade4182b1dc6f3870889/codex-rs/core/src/tasks/compact.rs#L41-L76),
[capability провайдера](https://github.com/openai/codex/blob/67cc3c318dc8b5532db6ade4182b1dc6f3870889/codex-rs/model-provider/src/provider.rs#L353-L364)
и [default feature](https://github.com/openai/codex/blob/67cc3c318dc8b5532db6ade4182b1dc6f3870889/codex-rs/features/src/lib.rs#L1656-L1661).
Proteus выбирает remote V2 по typed capability `supports_compaction`, которую
selected model передаёт в host-owned событии `BeforeContext`. Модель без этой
capability использует прежнюю локальную ветку. Выбор не зависит от имени
провайдера. Legacy `responses/compact` и speculative/background compaction
нового pin не реализуются этим hook.

V2 добавляет последним input item `compaction_trigger` в обычный Responses
запрос и получает `compaction` с `encrypted_content`; это непрозрачный
checkpoint для следующих запросов, а не текстовый summary. Такой explicit
trigger также описан в [официальном Responses API](https://developers.openai.com/api/docs/guides/compaction#request-compaction-with-an-explicit-trigger).
Локальный [summary request](src/summary.rs) делает обычный completion без tools.
Удалённый [request](src/remote.rs) сохраняет исходные messages, model-visible
тулы, instructions, reasoning/cache/client metadata; выбирает `Compact`,
`tool_choice:auto`, текстовый response format без output schema и не добавляет
локальный summary prompt. Операция проходит существующий bound model callback:
его owner, отмена, journal и учёт auxiliary usage сохраняются. HTTP и OAuth
принадлежат model adapter; hook не вызывает сеть напрямую.

Успешный результат — `Stop` без tool calls и ровно один assistant `Opaque`.
Model adapter дополнительно отвечает за terminal `response.completed`, точное
число completed checkpoint items и проверку своего opaque format/schema.
Зашифрованный payload сохраняется без преобразования в текст. Частичный ответ,
ошибка протокола, отмена или исчерпание бюджета не устанавливают replacement.
Remote ошибка не включает локальный fallback; повторяются только retryable
ошибки/оборванный stream, не более `min(stream_max_retries,2)`, с provider retry
advice и прерываемым backoff.

Replacement сохраняет real user сообщения с бюджетом64k, вставляет request-scoped
контекст перед последним пользователем и оставляет новый checkpoint последним.
Старый checkpoint участвует во входе следующего compact, затем заменяется новым.
Изображения сохраняются с native auto-detail оценкой1844 токена; на границе
изображение и соседние `<image>` labels атомарны, более поздние части сообщения
имеют приоритет, старые сообщения после не поместившегося изображения не
добавляются. Обрезанный user получает новые canonical identities и явную связь
с исходным user anchor. Локальный text-only бюджет20k не меняется.
Для непрозрачных частей hook запрашивает необязательную локальную оценку
`model.input_estimate.v1`. Формат checkpoint и расчёт его размера принадлежат
model adapter; hook не разбирает зашифрованный payload. Нулевой результат
допустим. Для неподдерживаемых частей остаётся консервативный serialized fallback.
Оценка передаётся по canonical part ID в metadata preparation report/request,
не меняя сообщения и payload; её использует workflow для категории `context`.
Старые сохранённые checkpoint оцениваются лениво при следующей preparation.
Provider usage, уже покрывающий checkpoint, остаётся точкой отсчёта и не
складывается с его оценкой повторно. Новый хвост истории учитывается отдельно.
Продолжение использует записанный report без повторения модельного запроса.

При известном переполненном окне [preflight](src/remote_window.rs) заменяет вывод
только непрерывного хвоста tool results на pinned placeholder, сохраняя call IDs
и результат success/error. Он не удаляет вызовы и не меняет исходную durable
историю. При включённом `codex_base_instructions` проверка повторяется после
вставки фактических base instructions с итоговым host-shaped context limit.
Все изменения становятся историей только после успешной проверки
всего результата model call и обычного commit context preparation.

Для DeepSeek через OpenCode Go поддержка remote-компактации Codex не заявлена;
локальная ветка соответствует fallback для provider без этой capability.
Зашифрованный OpenAI checkpoint нельзя прозрачно подставить вместо summary
в запрос DeepSeek. Наличие OAuth-маршрута и описанного протокола само по себе
не подтверждает доступ remote-компактации для конкретного аккаунта и модели.
