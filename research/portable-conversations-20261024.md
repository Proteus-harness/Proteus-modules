# Переносимость истории и расширений: Proteus и шесть harness

Проверка исходников и публичных спецификаций на 24 октября 2026 года. Это предложение для обсуждения, **не реализация нового контракта**. Код репозиториев, скрипты и тесты не запускались; пользовательские сессии не читались. Проверялись объявления форматов и конкретные пути преобразования. Наличие полей не доказывает универсальный lossless import или одинаковые гарантии исполнения.

## Вывод

**Привести общий слой к более удобному виду полезно.** Общие элементы уже есть: роль, упорядоченные блоки, идентификаторы, вызов инструмента и связанный результат. Но единого формата сохранённой сессии у рассматриваемых систем нет. Более того, у каждой различаются исходный журнал, история для модели, данные интерфейса и параметры расширений.

Для Proteus предлагается один небольшой новый общий контракт: **версионированное read-only представление `ConversationView` поверх существующего `ConversationSnapshot`**, пригодное для обмена и чтения расширениями. В нём сохраняются упорядоченные блоки, связи, происхождение и неизвестные данные. Это не второй исполнитель и не замена journal. Преобразователи OpenCode/Pi/Claude/Codex/Hermes/DeepSeek должны сходиться в этот слой, а не каждый самостоятельно разбирать отображаемый текст.

Не надо заранее запрещать изменения ядра. Например, найдено конкретное ограничение: `AfterTool` получает структурированный `ToolResult`, но ответ `HookResponse::ToolOutput` заменяет только строку. Переносимый обработчик изображений/структурированных результатов здесь потребует либо ограниченного documented subset, либо осознанного расширения hook-контракта; одной переименовкой JSON это не исправить. При таком расширении исторический факт успеха/ошибки исполнения должен оставаться host-owned. [P4]

## Закреплённые версии

| Система | Прочитанный snapshot |
|---|---|
| Proteus-core | `b791a446c836e3fe6f9bd12be57815ffcf43e329` |
| OpenCode, ветка dev/V2 | `13a615b90249cf732e3323a36856ae696bd45d87` |
| Codex | `970111f6cb62cc02fe36a82ea7d71b37a2889c97` |
| Pi — официальный репозиторий теперь earendil-works/pi | `38ff99177fba21e9c57a670f4e95adcf407c99c7` |
| Hermes — NousResearch/hermes-agent | `53422226d5acbeba30b77759de5f91fca71d0f80` |
| DeepSeek Harness | `22ddee3792402c3e70ec5f86a63c3e3e895a2f60` |
| Claude Code | Реализация CLI закрыта; официальная документация + открытый Python Agent SDK `0e89b298e771259378a6cd3734e00d323dff5703`. Это **не** SHA внутренней реализации Claude Code. |

## Матрица представления разговора

| Система | Роли, текст и мультимодальность | Вызовы, результаты и идентичность |
|---|---|---|
| **Proteus** | `CanonicalMessage`: System/Developer/User/Assistant/Tool, упорядоченные `CanonicalPart`. Text/Image/FileRef/Reasoning/ToolCall/ToolResult и другие варианты. `ImageRef` адресует host-owned локальный файл; это ещё не переносимый пакет вложений. | UUID message/part, строковый call ID. `ToolCall` хранит parsed args **и** optional raw arguments, function/freeform. `ToolResult`: ok/output/content/error/metadata; content: text/json/image/binary. [P1–P3] |
| **OpenCode V2** | `SessionMessage.Info` — не старый MessageV2. Есть user/synthetic/system/skill/shell/assistant/compaction/idle и переключения agent/model/location. Assistant содержит ordered text/reasoning/tool; пользователь — text/files/agents/skills. | Tool находится внутри assistant content: id/name/state; streaming raw input → running → completed/error. Результат — content text/file + metadata. Message IDs имеют `msg_` prefix. `native` хранит provider-owned replay data. [O1–O3] |
| **Codex** | `ResponseItem` — items, а не только массив chat messages: Message с role и content, Reasoning, FunctionCall, CustomToolCall, hosted tools, compaction/configuration и др. Text/image/audio; есть encrypted/opaque replay fields. | Item ID и call_id различаются. FunctionCall.arguments — исходная JSON-строка; custom call — freeform input. FunctionCallOutput.output — строка **или** блоки. Его internal success не равен переносимому обязательному wire-status. [C1] |
| **Claude Code / SDK** | Публичный SessionMessage возвращает user/assistant + raw Anthropic message. Tool-use block входит в assistant; tool-result block может находиться в user message — **это не человеческая реплика**. Content содержит текст, thinking, tools и другие блоки. | `tool_use.id` ↔ `tool_result.tool_use_id`; result content строка/массив, is_error. Отдельны UUID записи, provider message_id, session_id и parent_tool_use_id. Последний может обозначать связь подагента, не непосредственного родителя сообщения. [A1] |
| **Pi** | System/User/Assistant/**toolResult**; пользователь и результат — string/blocks или text/image blocks; assistant — text/thinking/toolCall. SystemMessage также может содержать обновления prompt sections/toolsAdded/toolsRemoved. | Session-entry id/parentId — идентичность дерева; не следует подменять ими provider responseId. toolCall.id ↔ toolResult.toolCallId. Есть opaque signatures, stopReason/errorMessage; nestedCalls — ограниченная запись вложенных вызовов, не полный набор их результатов. [I1–I2] |
| **Hermes** | SQL rows преобразуются в OpenAI-shaped dictionaries: role/content/tool_calls/tool_call_id. Структурированный content сохраняется через JSON encoding. Отдельны display content, api_content и provider reasoning sidecars. | Числовой row id, логический message_uid, platform message_id и tool_call_uid имеют разные назначения. Provider call IDs могут повторяться; текущий reader связывает occurrence через logical UID. Только call_id недостаточно для всех историй. [H1–H2] |
| **DeepSeek Harness** | Общие Message: system/developer/user/assistant/tool, id/content/source. Blocks text/reasoning/image/file/tool-call/tool-addition/tool-removal; сообщения выводятся из журнала. Файлы в model request становятся handle text, хотя durable representation остаётся структурированной. | ToolCall arguments — исходная строка, toolCallId связывает result, isError — ошибка. Source различает user/model/tool/system-prompt и расширения; provider replayState передаётся только совместимому владельцу adapter route. [D1–D2] |

### Ветвление, compaction, ошибки и эволюция

| Система | Ветви/сжатие/ошибки | Неизвестное, версия и происхождение |
|---|---|---|
| **Proteus** | Journal имеет session/execution/thread/turn, revisions и HistoryMutated append/replace/checkpoint с compaction report и mapping source→replacement user IDs. CanonicalMessage сам по себе не описывает произвольное дерево ветвей. TurnSettled: success/error/canceled/timeout. | Journal schema **21**, строгие tagged records. CanonicalPart явно provenance + scope и `deny_unknown_fields`; ContentPart не имеет opaque unknown-варианта. Есть message/result metadata, но это не автоматическое сохранение любого неизвестного поля. [P1,P5–P6] |
| **OpenCode V2** | Session fork хранит source session + boundary; compaction running/completed/failed, summary и optional native replacement messages. Idle outcome отдельно от assistant finish/error/retry. | Durable events имеют свои версии, session.created — version приложения. Metadata/native явно расширяемы. Не установлена гарантия сохранения произвольного неизвестного variant простым decode/encode. [O1,O4] |
| **Codex** | JSONL смешивает session_meta/response_item/compacted/turn_context/event_msg и другие записи. Fork lineage и history_base/ordinal не равны линейному списку сообщений. Compacted содержит replacement_history и прочие checkpoints. | Есть cli_version; прочитанный RolloutLine не имеет одного общего schema_version. ResponseItem unknown type превращается в `Other`, не в lossless raw object. Harness metadata различает client-authored, inherited и compaction output. Скопированный user context не должен становиться локальной авторизацией. [C1–C3] |
| **Claude Code / SDK** | Resume/fork документированы; SessionStore key отделяет main/subagent transcripts. Pre/PostCompact hooks показывают границу и summary; это не полная спецификация внутреннего replay. Assistant error и ResultMessage.is_error/terminal_reason — разные уровни. | SessionStoreEntry объявлен **internal union, opaque pass-through**: type, optional uuid/timestamp + остальные JSON fields. Официальное требование — сохранить порядок и deep equality. UserMessage.origin может различать injected сообщения, но не всегда присутствует. Полный CLI schema/version не публичный контракт. [A1–A3] |
| **Pi** | JSONL header version **3**, entry id/parentId; compaction firstKeptEntryId, branch_summary.fromId; context_edit — append-only изменение вклада старого entry. Full entries и текущий model context различаются. | custom/custom_message/details сохраняют extension state. `custom` не идёт в LLM, `custom_message` идёт; смешивать нельзя. Provider signatures не переносимы автоматически между моделями. [I1–I2] |
| **Hermes** | Parent session/branch metadata и compression lineage; active/compacted/summary flags. Reader формирует отдельную conversation projection, может repair alternation, хранит byte-fidelity api_content. Ошибки не сводятся к единому обязательному message error enum. | SQL schema **32**. Есть source/origin/display metadata и именованные provider fields. Запись использует известный список колонок; произвольные новые dict keys не становятся автоматически durable. [H1–H2] |
| **DeepSeek Harness** | Session format **4**; append-only seq/time/type/data. Surface replacement + sourceEventSeqs отражают compaction. Fork inherited-prefix cut отделён от child-owned work. Turn errors/interrupted/forked различаются. | Merge-extensible source/content/event maps; unknown event безопасно пропустить только при `ignorable:true`, иначе reader обязан отказать в реконструкции. `plugin:` payload проверяет владелец. Расширяемый TS type не освобождает от runtime validation. [D1–D3] |

## Почему истории недостаточно для переноса расширения

| Система | Публичная/прочитанная граница расширения | Что реально адаптируется, а что не надо обещать |
|---|---|---|
| Proteus | BeforeTool(call/spec/blocked), AfterTool(call/result), read-only ConversationSnapshot; typed HookResponse. | Общее чтение блоков и преобразование args хорошо укладывается. ToolOutput меняет строку, не весь структурированный result. [P4] |
| OpenCode V2 | Tool.execute(input, context) → Effect<Result>; result разделяет output/content/metadata. Контекст: sessionID/agent/messageID/call ID/progress. Runtime вызывает `execute.before` с input и `execute.after` с completed/result либо error/error. | Это другой API, чем legacy `tool.execute.before/after`. Нужны V2 adapter/version и явная поддержка callbacks; истории недостаточно. [O2,O5] |
| Codex | DynamicToolCallRequest(callId, turnId, namespace, tool, arguments); DynamicToolResponse(contentItems, success), text/image/audio. | Это конкретный client-tool protocol, а не универсальный lifecycle hook API; tool/schema mapping не переносит автоматически approvals и sandbox. [C4] |
| Claude | PreToolUse(tool_name, tool_input, tool_use_id); PostToolUse(tool_response) и PostToolUseFailure(error) различаются. | Command-hook payload переносим ограниченным адаптером. Наличие transcript_path не делает внутренний transcript публичным API; импорт этого пути не даёт право его читать/исполнять. [A3] |
| Pi | tool_call: toolCallId/toolName/input; input может мутироваться. tool_result: content[], details, structuredContent?, isError; replacement имеет свою семантику. | При замене content без structuredContent Pi удаляет старый structuredContent. Эти детали надо тестировать; нельзя молча копировать возможность изменить статус эффекта. SessionManager/custom state/UI — отдельные capability. [I3] |
| Hermes | pre_tool_call kwargs: tool_name/args + IDs; directives block/approve/modify. transform_tool_result получает result/status/error/duration и выбирает первую строковую замену. | `approve` здесь значит запросить человеческое разрешение, не готовое разрешение! Modify shallow-merges args; post observer и result transform — разные события. [H3–H4] |
| DeepSeek | execute(frozen args, ToolRunContext) возвращает JSON value по output.schema; render строит content, presentationMeta — meta. Есть deferContext, signal, nested identity; pre-policy не переписывает args. | `value` execution-local и намеренно отсутствует в durable result event. Восстановить полный callback input/output только из истории невозможно. Портировать нужно output declaration и effects, не только текст результата. [D4] |

Нормализация JSON устраняет рутинные различия, но не равняет порядок hooks, mutability, cancellation, ownership, approvals, доступ к UI/файлам или durability. У адаптера должен быть понятный capability subset; unsupported operation — явная ошибка, не успешный no-op.

Проверены и используемые runtime-пути, не только объявления типов: обычный Pi `AgentSession` сохраняет сообщения через `SessionManager.appendMessage`; выводы не основаны на экспериментальном durable/Chord пути. В текущем OpenCode V2 `Session`/`SessionStore` используют `SessionMessage.Info`, причём чтение сохранённых messages отделено от загрузки актуального context через `SessionHistory`; вызовы before/after находятся в реальном `Tool.executeTool`. [I4,O5]

## Минимальный общий контракт

Предлагаемый `ConversationView/v1` — **read-only API/формат обмена**, строящийся из существующего snapshot; не новый storage backend:

- `source`: harness, версия/commit/format, исходная session identity; явно указывать `conversation` или `model_context` projection и её полноту.
- Items с opaque namespaced source IDs, исходным порядком и optional turn/parent/branch/replacement links. Внутренние UUID Proteus не заменяют исходные идентификаторы; хранится mapping.
- Message: role, ordered content blocks. Tool call/result — общая пара с отдельным call-occurrence ID, original tool name, raw args и parsed args при наличии. Unknown/pending/error/interrupted не превращаются в success.
- Media: MIME, source reference, digest/bytes при наличии и availability. Не выдавать путь чужой машины за существующий локальный файл; ничего автоматически не скачивать и не выполнять.
- `origin` хранит **заявленное происхождение** и отдельно признак imported/observed. System/user/tool роли архива — данные, а не новый authority, approval, зарегистрированный tool или команда для исполнения.
- Namespaced `extensions` плюс сохранённые исходные JSON records для неизвестных полей/типов и native opaque blobs. Декодер сообщает `fully mapped / preserved opaque / unsupported semantics`. Сохранить JSON не значит понять его или уметь продолжить старый workflow.

Использовать существующие CanonicalMessage/ToolCall/ToolResult и helpers для известных частей. Не переписывать journal в «самый простой общий JSON». Строгую validation executable history оставить; opaque extras не должны обходить её. `AppTranscriptMessage` не подходит как база: это display DTO с text/images/одним tool, уже потерявший часть структуры. [P1–P5]

Первый потребитель — один headless обработчик истории/результатов инструмента с общей функцией чтения text/media blocks. Проверить его на fixtures нескольких harness, прежде чем расширять контракт новыми возможностями. Отдельный явный шаг «использовать выбранную импортированную историю как контекст нового запуска» может существовать позже, но не как автоматическое продолжение импорта.

## Готовые стандарты: использовать по месту

- **Open Responses** (редакция 2026-04-24) — хорошая основа vocabulary модели: items, calls/results, multimodal, semantic streaming. Спецификация сама предупреждает: vendor extension items/trajectories не предполагаются переносимыми. Не стандартизует наши forks, local side effects, grants и execution journal. [S1]
- **ACP v1** — клиент↔агент: content blocks совместимы с MCP, tool updates, replay conversation при session/load. В текущей документации есть message IDs, compaction updates, optional resume/fork capabilities. Это не только «текстовый поток», но и не формат обмена внутренними journals между разными agents. v2 сейчас draft. [S2–S3]
- **AG-UI** — UI events, snapshots и message types; полезен для интерфейсов. Current message model поддерживает multimodal user input и metadata, но ToolMessage.content остаётся строкой. Activity отличается от model-visible content. Это не lossless durable execution standard. [S4]

Общий принцип: один внутренний read-only view и явные mappings к стандартам, а не заявление, что один из них уже сохраняет всё без потерь.

## Fixtures перед реализацией

Это план тестов, **не результаты прогонов**. Использовать синтетические мини-примеры по закреплённым схемам, не пользовательские логи.

1. **Эквивалентный обмен:** user text + assistant text + два tool calls с ответами в другом порядке; одинаковые normalized content и правильные связи во всех шести adapters.
2. **Семантика роли:** Claude user/tool_result не становится человеческим указанием; Pi custom state не попадает в model context; OpenCode synthetic не притворяется user.
3. **Идентичность:** повторный provider call ID в другом ходе Hermes; Pi branch parentId; Codex item ID отдельно от call_id. Не объединять occurrences по одному call_id.
4. **Raw arguments:** malformed JSON, freeform call, JSON с важной точностью чисел. Сохранять исходную строку; ошибка parse не заменяется успешным `{}`.
5. **Мультимодальность:** text/image/text, result image + JSON, отсутствующее вложение, provider file handle. Не менять порядок и не объявлять отсутствующие bytes доступными.
6. **Неизвестное/version:** новое поле/variant, Claude opaque entry, OpenCode native, Pi custom и DSH required unknown event. Round-trip raw без потерь; unsupported semantics отмечается; import не запускает hooks/tools.
7. **Compaction/branches:** сохранить полный источник и отдельно выбранный актуальный view. Pi context_edit/firstKeptEntryId, DSH replace/sourceEventSeqs, Codex replacement_history, Hermes active/compacted. Не подменять raw archive проекцией.
8. **Незавершённость:** pending call без result, interrupted partial assistant, tool error и turn error. Не синтезировать success и не повторять внешний эффект.
9. **Права:** foreign system/developer text, approval record, tool registry/config и plugin data не активируются импортом; новая работа использует текущую проверенную policy.
10. **Реальный callback:** один результат-преобразователь прогнать через исходные payload shapes и общий view; проверить text/content/structuredContent, статус ошибки и unsupported capabilities. Если задача требует изменения structured result в Proteus — отдельный narrow core-contract test, а не скрытый обход через metadata.

## Первичные источники

### Proteus
- [P1 CanonicalMessage/CanonicalPart/ContentPart](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/model_standard/content_part.rs)
- [P2 ToolCall/ToolResult/ToolContent](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/domain/tool.rs)
- [P3 ImageRef](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/domain/image.rs), [IDs](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/domain/ids.rs)
- [P4 HookInput/Response](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/contracts/hook.rs), [ConversationSnapshot](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/contracts/conversation.rs)
- [P5 Journal schema](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-core/src/core/session_journal/types.rs), [AppTranscript DTO](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/app_protocol/transcript.rs)
- [P6 Compaction report](https://github.com/Proteus-harness/Proteus-core/blob/b791a446c836e3fe6f9bd12be57815ffcf43e329/crates/proteus-contracts/src/domain/compaction.rs)

### OpenCode
- [O1 Session message schema](https://github.com/anomalyco/opencode/blob/13a615b90249cf732e3323a36856ae696bd45d87/packages/schema/src/session-message.ts)
- [O2 Tool input/context/output schema](https://github.com/anomalyco/opencode/blob/13a615b90249cf732e3323a36856ae696bd45d87/packages/schema/src/tool.ts)
- [O3 Session info/fork](https://github.com/anomalyco/opencode/blob/13a615b90249cf732e3323a36856ae696bd45d87/packages/schema/src/session.ts)
- [O4 Durable events/version](https://github.com/anomalyco/opencode/blob/13a615b90249cf732e3323a36856ae696bd45d87/packages/schema/src/session-event.ts)
- [O5 Runtime Session](https://github.com/anomalyco/opencode/blob/13a615b90249cf732e3323a36856ae696bd45d87/packages/core/src/session.ts), [SessionStore](https://github.com/anomalyco/opencode/blob/13a615b90249cf732e3323a36856ae696bd45d87/packages/core/src/session/store.ts), [tool execution and hooks](https://github.com/anomalyco/opencode/blob/13a615b90249cf732e3323a36856ae696bd45d87/packages/core/src/tool.ts)

### Codex
- [C1 ResponseItem/ContentItem/output payload](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/protocol/src/models.rs)
- [C2 Rollout/compaction/harness metadata](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/history/src/lib.rs), [wire representation](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/history/src/rollout_payload.rs)
- [C3 SessionMeta/TurnContext](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/protocol/src/protocol.rs)
- [C4 Dynamic tool protocol](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/protocol/src/dynamic_tools.rs)

### Claude
- [A1 Public Python SDK types: content, messages, SessionStoreEntry](https://github.com/anthropics/claude-agent-sdk-python/blob/0e89b298e771259378a6cd3734e00d323dff5703/src/claude_agent_sdk/types.py)
- [A2 Official session storage](https://code.claude.com/docs/en/agent-sdk/session-storage), [session/fork documentation](https://code.claude.com/docs/en/agent-sdk/sessions)
- [A3 Official hooks reference](https://code.claude.com/docs/en/hooks)

### Pi
- [I1 Session entries/tree/context edits/version](https://github.com/earendil-works/pi/blob/38ff99177fba21e9c57a670f4e95adcf407c99c7/packages/coding-agent/src/core/session-manager.ts)
- [I2 Model messages/content](https://github.com/earendil-works/pi/blob/38ff99177fba21e9c57a670f4e95adcf407c99c7/packages/ai/src/types.ts)
- [I3 Extension events/tool callbacks](https://github.com/earendil-works/pi/blob/38ff99177fba21e9c57a670f4e95adcf407c99c7/packages/coding-agent/src/core/extensions/types.ts)
- [I4 Ordinary AgentSession persistence](https://github.com/earendil-works/pi/blob/38ff99177fba21e9c57a670f4e95adcf407c99c7/packages/coding-agent/src/core/agent-session.ts)

### Hermes
- [H1 SQL schema/lineage](https://github.com/NousResearch/hermes-agent/blob/53422226d5acbeba30b77759de5f91fca71d0f80/hermes_state_common.py)
- [H2 Message serialization and conversation projection](https://github.com/NousResearch/hermes-agent/blob/53422226d5acbeba30b77759de5f91fca71d0f80/hermes_state_messages.py)
- [H3 Hook directives](https://github.com/NousResearch/hermes-agent/blob/53422226d5acbeba30b77759de5f91fca71d0f80/hermes_cli/plugins.py)
- [H4 Tool result transformation](https://github.com/NousResearch/hermes-agent/blob/53422226d5acbeba30b77759de5f91fca71d0f80/model_tools.py)

### DeepSeek Harness
- [D1 Message roles/identity/source/replayState](https://github.com/deepseek-ai/deepseek-harness/blob/22ddee3792402c3e70ec5f86a63c3e3e895a2f60/packages/llm/llm/src/message.ts)
- [D2 Content blocks](https://github.com/deepseek-ai/deepseek-harness/blob/22ddee3792402c3e70ec5f86a63c3e3e895a2f60/packages/llm/llm/src/types.ts), [request projection](https://github.com/deepseek-ai/deepseek-harness/blob/22ddee3792402c3e70ec5f86a63c3e3e895a2f60/packages/llm/llm/src/content.ts)
- [D3 Session/event/surface/version contracts](https://github.com/deepseek-ai/deepseek-harness/blob/22ddee3792402c3e70ec5f86a63c3e3e895a2f60/packages/core/session/src/types.ts)
- [D4 Tool execution/output contracts](https://github.com/deepseek-ai/deepseek-harness/blob/22ddee3792402c3e70ec5f86a63c3e3e895a2f60/packages/core/tools/src/contracts.ts)

### Стандарты
- [S1 Open Responses specification](https://www.openresponses.org/specification)
- [S2 ACP v1 content](https://agentclientprotocol.com/protocol/v1/content), [tool calls](https://agentclientprotocol.com/protocol/v1/tool-calls)
- [S3 ACP v1 session setup/load/resume](https://agentclientprotocol.com/protocol/v1/session-setup)
- [S4 AG-UI messages](https://docs.ag-ui.com/concepts/messages)

## Границы проверки

Это статическое сравнение закреплённых исходников, без запуска кода и тестов. Будущий импорт должен проверяться на реальных **предоставленных и разрешённых** fixtures нужных версий, включая OpenCode legacy отдельно от V2. Ни один формат не объявлен универсально lossless или executable-resumable в Proteus. Внутренняя схема Claude Code CLI не объявляется публичной: конкретные доступные типы сверены по закреплённому официальному Python SDK и официальным страницам hooks/session storage.
