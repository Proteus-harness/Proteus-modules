# Pi Tools

Процессный export `tool/pi_tools` использует неизменённые реализации `read`,
`bash`, `edit`, `write` из npm `@earendil-works/pi-coding-agent@1.5.2`.
Версии зависимостей и integrity hashes закреплены в `package-lock.json`.
Требуются Node.js 22.19+ и Python 3 для installer/launcher.

```sh
python3 tool/pi-tools/install.py --prefix /absolute/ready/Proteus
```

Installer выполняет `npm ci --ignore-scripts --omit=dev`, устанавливает JS,
node_modules и соседние workflow adapters под `share/pi-runtime`, а launcher
`bin/proteus-pi-module` вычисляет путь относительно своей директории.
В config укажите command `proteus-pi-module` и export `tool/pi_tools`.

Схемы аргументов и descriptions берутся из upstream definitions. `cwd`
передаётся из invocation Core, отмена — как AbortSignal. Файловые пути,
нормализация edit, чтение изображений, лимиты и bash subprocess исполняет Pi.
Core по-прежнему выполняет policy/approval до вызова инструмента, а OS-права
задаёт launch configuration процесса. Upstream bash работает без встроенной
Proteus shell-песочницы. Для изолированных evals применяется внешняя песочница.

Текст результата переносится в `ToolResult.output`, изображения — в typed
`content`, `details` — в metadata `pi/details`, structuredContent — в
`pi/structured_content`. Workflow adapter должен
восстанавливать Pi tool-result messages из этих полей. Исключение upstream
становится `ok:false` с исходным текстом ошибки. Streaming onUpdate пока не
передаётся в Core UI, итоговый content сохраняется. Core tool invocation не
предоставляет полный Pi ExtensionContext с model, thinkingLevel и session file.
Инструмент получает upstream optional ctx как undefined и использует bound cwd
из своей definition. Pi session environment variables в bash не экспортируются.

Wire worker связывает каждую invocation с конкретным export и передаёт
`hostCall(method,params)` с её invocation_id. Набор exports не объединяет
authority. Для workflow/context/hook он импортирует соседний
`workflow/pi-runtime/exports.mjs`, который предоставляет `createExport(binding)`.

`workflow/pi_loop` и `hook/pi_compact` принимают необязательный
`catalog_provider = "openai-codex" | "opencode-go"`; default — `openai-codex`.
Оба используют descriptor из закреплённого Pi catalog для выбранного provider,
включая context window, output limit и compaction. Для другого provider настройте
оба exports явно вместе с `model/pi_codex.implementation`; имя модели и имя
model export не переключают catalog. Неизвестная модель завершается ошибкой,
подмена модели отсутствует. `context/pi_context` этого параметра не принимает.
