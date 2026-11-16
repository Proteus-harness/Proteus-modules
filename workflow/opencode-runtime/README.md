# OpenCode Native Adapters

Процессные exports импортируют pinned npm `@opencode/core@2.0.22` и Effect
`4.0.0-rc.112`. Source pin — `527f0b931d1f9b3ebd34e106c51b31ce5db5b075`.
Локальный Node Linux x64 24.12.0 нужен для upstream `await using`.

```sh
python3 workflow/opencode-runtime/install.py --prefix /absolute/ready/Proteus
cd workflow/opencode-runtime && npm test
```

Установка сохраняет относительные импорты в
`share/opencode-runtime/{workflow/opencode-runtime,model/opencode-provider,components/module-runtime-js,components/model-runtime-js}`.
Launcher — `bin/proteus-opencode-module`. Installer повторно копирует все
handlers, общий JSON-RPC broker, model bridge и общий Go quota helper.
Он устанавливает pinned npm dependencies; Rust/Core/Desktop для этого не собираются.
После обновления перезапустите backend, чтобы worker загрузил новые файлы.

## Model Export

`model/opencode` реализует `model/v13` и optional `model.catalog.v1`.
Для `provider="opencode-go"` дополнительно объявляется `model.quota.v1`
(Go usage windows и USD). Каталог и inference используют непосредственно
`ModelsDev`, `ModelResolver` и `LLMClient` pinned SDK. Полный OpenCode agent,
его prompt, tools, context и compactor не запускаются. Поэтому этот model slot
можно подключить к профилю Codex, не меняя его остальные slots. Pi не нужен.

Пример подключения к существующему профилю:

```toml
active_provider = "go"

[providers.go]
provider = "opencode"
model = "deepseek-v4.1-flash"
stream = true
[providers.go.reasoning]
effort = "high"
summary = true

[components.opencode-model]
command = "proteus-opencode-module"
env_allowlist = ["HOME", "XDG_DATA_HOME", "PATH"]
[components.opencode-model.exports.model.opencode]

[module_config.model.opencode]
provider = "opencode-go"

# Для Codex patch нужен function binding этого transport.
[module_config.tool.codex_patch]
freeform = false
```

Список моделей в конфиге не нужен: `model.catalog.v1` сообщает Core модели
выбранного provider, display names, hidden status и exact effort variants.
В chat-каталог входят модели с text input/output и положительными context/output
limits; доступность image input и tools берётся из native metadata.
`max_input_tokens` использует native input limit, если он задан, иначе context
limit. Effort проверяется по variant ID без aliases: например, `xhigh` не
превращается в `max`. При отсутствии canonical output limit SDK получает запрос
без `max_tokens`/`max_completion_tokens`; дополнительный cap не добавляется.

Каталог обновляет `ModelsDev`. Адаптер объединяет одновременные запросы и
удерживает один provider snapshot на пять минут. После TTL следующий запрос
выполняет refresh; фонового refresh fiber нет. На удержание действует логический
бюджет metadata 4 MiB / 65 536 nodes, а не оценка heap/RSS. Более крупный каталог
возвращается без кеширования. Ошибки refresh сохраняют native error semantics;
статический список aliases не подставляется.

Авторизация использует существующую OpenCode v2 credential DB: по умолчанию
`$XDG_DATA_HOME/opencode/opencode.db` при абсолютном XDG-пути, иначе
`~/.local/share/opencode/opencode.db`. DB открывается только для чтения, без
миграций и создания отсутствующего файла. SQLite может создавать служебные
WAL/SHM sidecars для согласованного чтения; committed изменения из WAL видны,
сама credential DB не редактируется и не checkpoint-ится. Выбирается active/latest сохранённый
credential этого provider, затем API key из environment names native provider.
Неверный сохранённый credential вызывает ошибку, а не переключение на другой
ключ. Поддержаны только credentials типа `key`; OAuth явно отклоняется, поскольку
этому ограниченному adapter не принадлежит native refresh lifecycle. Pi auth и
legacy `auth.json` не читаются. Для другого ПК подключите provider в OpenCode
или передайте его API-key environment variable в allowlist процесса.

Optional config keys:

| Параметр | Значение |
|---|---|
| `database_path` | Другой путь к OpenCode v2 DB; поддерживает `~/`, `$HOME/`, `${HOME}/` |
| `models_file` | Локальный native ModelsDev metadata файл; без HTTP refresh |
| `models_url` | Другой native ModelsDev metadata endpoint |

Это transport surface SDK: function tools передаются с native `strict=false`,
argument recovery выполняется native SDK. Freeform и provider-executed tools,
structured response format и чужие opaque history parts явно неподдержаны.
Canonical decoded `args` имеют приоритет над сохранённым `raw_arguments`.
Visible reasoning передаётся отдельно; native provider metadata не отображается
как текст. Input/output usage уже включают cache/reasoning соответственно:
эти отдельные счётчики нельзя повторно прибавлять к totals. После обрыва
незавершённый текст не возвращается как завершённое сообщение; валидный completed
message сохраняется, если transport сломался уже после finish.

`npm test` запускает native SDK тесты с локальными HTTP/SSE fixtures, временной
SQLite DB и настоящим Node v3 process boundary: каталог/TTL, resolver, stream,
reasoning, usage/cache, отмена и ошибки. Ready-Core HTTP проверки выбора
модели/effort, сохранения и quota выполняются при наличии соседнего portable
Core; без готового бинарника только эти проверки явно пропускаются. Тесты не
собирают Core и не делают платных inference-запросов.

## Agent Slots

Реальные upstream пути исполнения:

- `hook/opencode_prompt`: `SessionSystemPrompt.make` и `OptimizePlugin.Plugins`
  для модели и выбранных tools перед каждым запросом. Поддерживает native GPT,
  Astra, Anthropic и explicit `agent_system`. Используется upstream
  `Model.Info.default(provider,id)` для подготовки prompt; это отдельный путь
  от metadata catalog нового model export.
- `context/opencode_context`: `Project.root`, `ConfigInstructionPlugin`,
  `InstructionDiscovery`, `InstructionBuiltIns` и `Instructions.read/diff/renderInitial`.
  Загружаются native AGENTS, date/env, а не Codex project-instruction aliases.
  Context собирается заново при вызове; FS watcher заменён пустым потоком.
- `tool/opencode_tools`: native read/write/edit/patch/shell/glob/grep Plugin.effect,
  настоящий `ToolInputRepairPlugin` и `ToolRuntime.execute` (decode/encode,
  input errors, optional null repairs, result content normalization). Реальный
  InputRepairPlugin также вызывается workflow перед binding checkpoint, чтобы
  Core проверял и разрешал уже исправленные execution arguments.
  Настоящий PatchTool context hook выбирает patch для GPT (кроме gpt-oss/gpt-4),
  а для остальных моделей edit/write; неактивные tools удаляются из request.
  Работают native FileAccess/FileMutation, BOM/diff/edit matcher, read formats,
  Ripgrep, Shell/ShellSelect/Job и process spawner. Native foreground timeout,
  env, combined output, nonzero result, truncation сохранены.
- Nested AGENTS из read обрабатывает настоящий `SessionInstructions`.
  Native synthetic events сохраняются в canonical ToolResult metadata и
  проецируются BeforeModel через native `toLLMMessages` один раз. Дедупликация
  native service читает только invocation-bound Core conversation snapshot.
  Состояние переживает compaction в canonical checkpoint metadata.
- `hook/opencode_compact`: настоящий `SessionCompaction.layer` и config plugin.
  Summary вызывает только Core `host.model.complete`; native threshold/split,
  prompt/nudge/retry и checkpoint/recent handling покрыты fake-host tests.
- `workflow/opencode_loop`: native `SessionStep` для model/tool continuation.
  Для временных model failures используется настоящий `SessionRunnerRetry.policy`
  с десятью повторами и native backoff/jitter. Каждый повтор создаёт отдельный
  model exchange через Core, с прежней подтверждённой историей инструментов.
  Core checkpoints и dispatch остаются через workflow host capabilities.

Core policy проверяется до tool invocation. Permission backend принимает уже
разрешённые операции внутри native project root и отклоняет дополнительные
`external_directory` approvals. Global dirs и shell captures изолированы под
`PROTEUS_OPENCODE_RUNTIME_DIR` либо `XDG_STATE_HOME/proteus/opencode-runtime`
(`~/.local/state` по умолчанию), вне workspace. Native `Global.layerWith` создаёт их.
Native Step fan-out проходит через invocation-scoped barrier по ToolSpec:
вызовы с `supports_parallel_tool_calls=false` образуют последовательный fence.
Отдельные вызовы не делят скрытое mutable состояние FileMutation/SessionInstructions. Read-only specs сохраняют
правильный safety; shell имеет `RunsCommands`.

Материальные ограничения относительно всего vanilla OpenCode: background
shell jobs требуют durable notification/resume host; `background=true`
отклоняется до spawn. Native provider remote compaction не заменяется локальным
summary, а возвращает явную ошибку. Native formatter registries пусты; extensions,
network/MCP, skills/subagents и оставшаяся tool registry не включены. Normal
model/tool continuation использует Native Step. Native retry schedule применяется
к типизированным временным model failures. Core передаёт только завершённые model
items; незавершённый ответ не запускает tools и не дописывается в sampling history.
Типизированный `context_window_exceeded` до завершённого ответа запускает
выбранную подготовку контекста с причиной overflow: штатный `SessionCompaction`
выполняет forced compaction через Core. Повтор разрешён только при изменённой
истории и durable checkpoint ACK; прежние подтверждённые tools не исполняются
заново. Как в pinned `SessionRunner.runStep`, на один model step допускается
одна такая recovery; следующий завершённый ответ вновь разрешает восстановление.
Отключённый/отсутствующий compactor, ошибка summary/ACK и отмена не запускают
скрытый повтор. Provider remote compaction остаётся явно неподдержанной.
Нативные Continue с partial assistant text, специфичный timeout budget и
полный session recovery lifecycle остаются отдельными границами. Context initial text
проецируется в effective request; session instruction delta lifecycle всего
OpenCode DB не воспроизводится. Поэтому этот профиль нельзя объявлять полной
эквивалентностью всей vanilla упряжке; это перенесённые upstream slots с явно
ограниченной поверхностью. Строгая OS containment задаётся launch configuration.

### Граница tool-result media

Общая projection `nativeContext` для workflow estimate и compaction сохраняет
`ToolResult.output`, text blocks и inline `image/*` как native `Tool.Content`
(text либо file с data URI). Обратная projection запроса compaction в Core
сохраняет их как типизированный `ToolResult`, включая failed status и error
message. В output переносится только первый блок, если он text; image-first
content не переупорядочивается. Multi-text native content теперь возвращается
как output + text blocks, а не JSON-строка массива. Error без native envelope
сохраняет прежний scalar/JSON fallback.

Это модельно-видимое преобразование, не exact DTO round-trip: служебные native
names/metadata не восстанавливаются, а upstream может убрать пустой text или
намеренно flatten/prune историю и media при уменьшении compaction request.
Эти upstream policy не заменяются. Поддержка MIME image/* сохраняет MIME/data,
но не обещает принятие каждого формата выбранным provider. Binary/PDF и Json
в tool content остаются явно неподдержанными; non-inline user attachments не
изменены. Payload не читается из внешнего URI и не превращается молча в текст.

Новые targeted controls находятся в tests/{compaction,workflow,files}.test.mjs:
оба направления text/image/error, image-first ordering, PNG continuation без
compactor и с auto:false, и настоящий native PNG reader → projection. Они
требуют точных штатных dependencies и не заменяют full package/Core gate.
