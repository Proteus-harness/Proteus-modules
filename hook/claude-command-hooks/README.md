# Перенос Claude Command Hooks: `strict_subset`

Адаптер переносит **существующий command hook и его JSON-конфигурацию**, а не
просит переписать проверку на Proteus. Исходный script и строка `command`
сохраняются. Владелец отдельно выбирает события и задаёт соответствие имён
tools/аргументов. Модуль использует публичный `hook/v6` и существующий JS SDK
Proteus-core; Core, UI и Rust reference executable не изменены.

Это первый **ограниченный режим переноса**, не полная совместимость с Claude
Code, не загрузчик marketplace/plugins и не реализация Agent Plugins 1.0.
`compatible: true` означает совместимость прочитанной **конфигурации** с этим
режимом, а не аудит или успешное исполнение непрозрачного script.

## Поддержанная Граница

| Вход | Результат |
|---|---|
| `PreToolUse`, синхронный `type: command` | `before_tool` |
| `Stop`, синхронный `type: command` | `before_stop` |
| Shell-form `command` без `args` | неизменная строка в `/bin/sh -c`; явный `shell: bash` выбирает `/bin/bash` |
| Exec-form `command` + `args` | executable/argv, без shell-разбора; подстановка двух известных `${...}` placeholders |
| Exit 0, пустой stdout / `{}` / `{"continue":true}` | продолжить обычный путь Proteus |
| PreToolUse JSON `hookSpecificOutput.permissionDecision: deny` + причина | `block_tool` |
| PreToolUse `allow`, необязательный объект `updatedInput` | продолжить / заменить args; **не выдать разрешение** |
| Stop JSON `decision: block` + `reason` | `continue_turn` в том же root turn |
| Exit 2 + непустой stderr | `block_tool` / `continue_turn`; stderr целиком является причиной, не JSON-командой |

В `hookSpecificOutput` требуется `hookEventName: "PreToolUse"`. Arg mapping
применяется обратно к `updatedInput`; затем host повторно выполняет validation,
policy и approval. `allow` не обходит ни один из этих шагов.

**Почему режим назван `strict_subset`:** неизвестные выбранные поля, неподдержанный
output, ошибки запуска, прочие exit codes и timeout завершают hook ошибкой,
а не молча продолжают действие. Это намеренное отличие от ряда исходных
non-blocking ошибок Claude. В BeforeTool инструмент не запускается; ошибка
Stop не превращается в успешное принятие кандидата.

Другие отличия и ограничения:

- На одно событие допускается **только один совпавший handler**. Если совпали
  два, ошибка возникает до запуска любого из них. Claude выполняет matching
  hooks параллельно; здесь не имитируется неизвестный порядок агрегации решений.
  Поэтому многие реальные конфигурации пока не являются drop-in.
- Загружается один явно выбранный файл, без merge уровней settings, file watcher,
  автоматического поиска `.claude`, исполнения при импорте или установки npm.
- Поддержаны POSIX и Node.js **22.18+**; Windows не поддержан.
- `args` сохраняет границы argv. `${CLAUDE_PROJECT_DIR}` и
  `${CLAUDE_PLUGIN_ROOT}` подставляются буквально. `command` exec-form не должен
  начинаться с `-` или содержать `=` из-за границы launcher `/usr/bin/env`.
- `CLAUDE_PROJECT_DIR` равен `ctx.cwd`; `CLAUDE_PLUGIN_ROOT` — только явно
  заданному `plugin_root`. Нет глобальной правки `process.env` между invocations.
  `CLAUDE_PLUGIN_DATA` не создаётся; его явные ссылки в конфигурации отклоняются.
- stdin содержит `hook_event_name`, `cwd`, nullable `session_id`, nullable
  `turn_id`; PreToolUse добавляет `tool_name`, `tool_use_id`, `tool_input`,
  Stop — `stop_hook_active` и `last_assistant_message`.
  Не выдумываются `transcript_path`, `permission_mode`, `prompt_id`, model,
  scratchpad, background tasks или состояние Claude. Зависимость script от них
  требует отдельного переноса; инспектор не анализирует тела scripts.
- Нет HTTP/prompt/agent/MCP hooks, `if`, `onFailure`, `statusMessage`, background
  `async`/`asyncRewake`, user-config substitutions или lifecycle API. Активные
  неподдержанные поля отклоняются. `async:false`/`asyncRewake:false` допускаются.
- Нет plain-text stdout, `ask`, `continue:false`, `additionalContext`,
  `systemMessage`, `terminalSequence` или других невыразимых решений. Даже
  синтаксически правильный JSON с такими полями — явная ошибка.
- Stop не обходит лимит Core: максимум восемь продолжений, затем ошибка;
  отмена/общий deadline сохраняются. Нет собственного бесконечного retry.

## Проверить Без Исполнения

Создайте отдельный файл настроек адаптера; все пути внутри него абсолютные:

```json
{
  "mode": "strict_subset",
  "allow_execution": false,
  "config_path": "/work/foreign-plugin/hooks/hooks.json",
  "sdk_root": "/opt/Proteus-core/examples/modules/hook-process",
  "plugin_root": "/work/foreign-plugin",
  "events": ["PreToolUse", "Stop"],
  "tool_mappings": {
    "shell": {"name": "Bash", "arguments": {}},
    "write_file": {"name": "Write", "arguments": {"path": "file_path"}}
  }
}
```

```bash
node hook/claude-command-hooks/inspect.mjs /work/adapter-settings.json
```

Инспектор читает только JSON, не импортирует SDK или script, не вызывает shell
и не требует `allow_execution:true`. Возвращает JSON с `compatible`, списком
выбранных hooks и диагностикой с путями. Exit 0 — поддержанная конфигурация,
1 — несовместимость/ошибка чтения, 2 — неверная команда CLI.

Невыбранные события помечаются `not selected`, но не исполняются и не
валидируются как выбранные hooks. Другие поля settings не импортируются и
показываются в diagnostics. `disableAllHooks:true` в выбранном файле отключает
все handlers. Это не меняет стандартный импорт Skills/MCP в Core и не
нарушает правило Agent Plugins об игнорировании неизвестных extensions.

Matcher повторяет проверенную поверхность Claude: `*`, пустой/отсутствующий —
все; буквы/цифры/`_`/`-`/пробелы/`,`/`|` — точные альтернативы; иначе JS regexp
без неявных anchors. `Bash` не совпадает с `MyBash`. Stop игнорирует matcher и
выдаёт предупреждение. Mapping имён выполняется **до** matcher. Неуказанные
tools сохраняют свои имена; таблица mapping не является списком разрешений.

Arg mapping — только взаимно-однозначное переименование верхнеуровневых ключей.
Коллизии и неоднозначные уже занятые имена отклоняются, не теряя поля. Нельзя
выдать `apply_patch` за Claude `Edit`: схемы имеют различную семантику.

## Подключить После Проверки И Явного Выбора

SDK — **явная внешняя зависимость**, не встроенный новый process runtime.
Проверенная исходная ревизия Proteus-core —
[`bee0e409e746e2df9db32748d4f8ea02201bd4af`](https://github.com/Proteus-harness/Proteus-core/tree/bee0e409e746e2df9db32748d4f8ea02201bd4af/examples/modules/hook-process),
из которой взяты runtime-файлы SDK. Public entry API описан в его README.
`sdk-pin.json` фиксирует SHA-256 пяти runtime-файлов; инициализация проверяет их
до импорта helper API. Подходит любой checkout с теми же файлами: совпадение
Git HEAD с исходной ревизией не требуется. Это проверка совместимости,
не замена доверию к исполняемому коду/профилю. SDK не скачивается автоматически.

Ниже фрагмент для профиля без других hooks. Если `[modules]` уже существует,
**отредактируйте его массив**, добавив `"claude-command-hooks"` после имеющихся
ID; не вставляйте второй `[modules]` и не заменяйте существующий массив.
Например, `hooks = ["existing-reviewer", "claude-command-hooks"]`, где
`existing-reviewer` — настоящий ID из вашего профиля, не новый export.

```toml
[modules]
hooks = ["claude-command-hooks"]

[components.claude-command-hooks]
command = "node"
args = ["/opt/Proteus-core/examples/modules/hook-process/worker.mjs"]

[components.claude-command-hooks.exports.hook.claude-command-hooks]
timeout_ms = 610000

[module_config.hook.claude-command-hooks]
entry = "/opt/Proteus-modules/hook/claude-command-hooks/src/entry.mjs"

[module_config.hook.claude-command-hooks.settings]
mode = "strict_subset"
allow_execution = true
config_path = "/work/foreign-plugin/hooks/hooks.json"
sdk_root = "/opt/Proteus-core/examples/modules/hook-process"
plugin_root = "/work/foreign-plugin"
events = ["PreToolUse", "Stop"]

[module_config.hook.claude-command-hooks.settings.tool_mappings.shell]
name = "Bash"
arguments = {}
```

Выбор trusted profile + `allow_execution=true` разрешает запуск команд при
соответствующих событиях. **Command hook не является sandbox** и не проходит
`host.tools.execute`/approval как tool: это код доверенного компонента с его
OS-доступом и унаследованным окружением. Не включайте непрочитанный plugin.
Ни инспектор, ни настройка не считают произвольный сторонний script безопасным.

Пределы этого режима: конфигурация и stdin до 1 MiB, суммарный stdout/stderr до
1 MiB (SDK), не более 64 выбранных handlers, timeout каждого >0 и <=600 секунд
(по умолчанию 600, как проверенный command contract). Matcher <=1024 символа,
regexp input <=256 и вычисление <=25 ms. Export/parent deadline может быть
короче. Timeout и cancellation передаются SDK через invocation AbortSignal;
SDK завершает POSIX process group, включая потомков, и дожидается cleanup.
Это lifecycle cleanup, не защита от враждебного script, покинувшего группу.

## Переносимая Поставка

Portable Proteus включает SDK, адаптер, исходники, документацию, pin/provenance
и MIT-лицензии. Для запуска нужен системный Node.js **22.18+**. Пути ниже
указаны относительно корня переносимой папки:

| Назначение | Путь |
|---|---|
| Общий worker любых JS hook entries | `bin/proteus-hook-worker` |
| Инспектор без исполнения hooks | `bin/proteus-claude-hooks-inspect` |
| `entry` адаптера | `share/modules/claude-command-hooks/src/entry.mjs` |
| `sdk_root` | `share/modules/hook-process` |

В профиле укажите абсолютный путь `command` к `bin/proteus-hook-worker`
(без `args` с путём worker), абсолютные `entry`, `sdk_root` и собственный
`config_path`. Относительные пути component разрешаются из рабочего проекта,
а не корня portable; после перемещения папки обновите абсолютные пути профиля.
Launchers сами находят поставляемые файлы после перемещения, независимо от cwd.
Инспектор принимает путь к явно подготовленному JSON настроек:

```bash
/absolute/Proteus/bin/proteus-claude-hooks-inspect /work/adapter-settings.json
```

Поставка не создаёт конфигурацию сторонних hooks, не включает export в профиль
и не разрешает исполнение автоматически. `allow_execution=true` и выбранный
trusted profile по-прежнему задаются владельцем явно.

## Реальный Перенос И Происхождение

В [examples/validate-bash.hooks.json](examples/validate-bash.hooks.json) находится
**наша конфигурация** для неизменённого официального
[`validate-bash.sh`](https://github.com/anthropics/claude-plugins-official/blob/58267944f92187b7d6a1c822165c96c81c864806/plugins/plugin-dev/skills/hook-development/examples/validate-bash.sh).
Укажите `plugin_root` на `plugins/plugin-dev` проверенного checkout, выберите
PreToolUse и mapping `shell → Bash` (`command` остаётся `command`).
Подробные pin, hash, лицензия и границы проверки — [PROVENANCE.md](PROVENANCE.md).

Скрипт здесь **не скопирован, не скачивается адаптером и не запускался** при
разработке. Он иллюстрирует перенос формата, а не надёжный security product:
его строковые эвристики не заменяют sandbox/policy. Scripts, требующие
transcript или других API, не становятся совместимыми от одного loader.

## Тесты

```bash
node --test hook/claude-command-hooks/tests/*.test.mjs
PROTEUS_HOOK_SDK_ROOT=/opt/Proteus-core/examples/modules/hook-process \
  node --test hook/claude-command-hooks/tests/*.test.mjs
```

Без SDK при отдельном Node-запуске остаются parser/matcher проверки;
integration tests явно пропускаются. Для полного отдельного Node gate задайте
SDK с runtime-файлами, соответствующими `sdk-pin.json`.

Из корня Modules `python3 scripts/test.py full` и
`python3 scripts/test.py -p proteus-reference-module` запускают эти Node-тесты
вместе с Rust gate. Runner получает SDK из закреплённого Cargo dependency
Core и задаёт `PROTEUS_HOOK_SDK_ROOT`, поэтому integration tests выполняются
без ручной настройки пути. Отсутствующий или несовместимый SDK завершает gate
ошибкой.

Tests используют только собственные
безопасные fixtures; проверяют JSON/mapping, veto/allow, Stop, ошибки,
timeout/cancel, отсутствие команд при инспекции/инициализации и реальный
worker protocol. Node-тесты сами по себе не проверяют Rust host или portable build.
`components/process-module/tests/claude_command_hooks.rs` дополнительно запускает
этот worker через настоящий Rust host: veto и ошибки не дают файлового эффекта,
`updatedInput` проходит обычное подтверждение, а Stop продолжает тот же turn
и завершает его с сохранённым `TurnSettled::Success`. Данные и команды синтетические;
настоящие сторонние hooks этими проверками не исполняются.
