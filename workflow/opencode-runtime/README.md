# OpenCode Native Adapters

Процессные exports импортируют pinned npm `@opencode/core@2.0.22` и Effect
`4.0.0-rc.112`. Source pin — `527f0b931d1f9b3ebd34e106c51b31ce5db5b075`.
Локальный Node Linux x64 24.12.0 нужен для upstream `await using`.

```sh
python3 workflow/opencode-runtime/install.py --prefix /absolute/ready/Proteus
cd workflow/opencode-runtime && npm test
```

Установка сохраняет относительные импорты в
`share/opencode-runtime/{workflow/opencode-runtime,components/module-runtime-js}`.
Launcher — `bin/proteus-opencode-module`. Installer повторно копирует все
handlers, общий JSON-RPC broker и canonical message helper, включая изменения workflow.

Реальные upstream пути исполнения:

- `hook/opencode_prompt`: `SessionSystemPrompt.make` и `OptimizePlugin.Plugins`
  для модели и выбранных tools перед каждым запросом. Поддерживает native GPT,
  Astra, Anthropic и explicit `agent_system`. Используется upstream
  `Model.Info.default(provider,id)`: полного native model catalog в Core нет.
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
- `workflow/opencode_loop`: native `SessionStep` для model/tool continuation,
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
model/tool continuation использует Native Step, но все native stream retry,
overflow и session recovery branches не перенесены. Context initial text
проецируется в effective request; session instruction delta lifecycle всего
OpenCode DB не воспроизводится. Поэтому этот профиль нельзя объявлять полной
эквивалентностью всей vanilla упряжке; это перенесённые upstream slots с явно
ограниченной поверхностью. Строгая OS containment задаётся launch configuration.
