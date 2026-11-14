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

При распознанном обрыве транспорта модель возвращает типизированную ошибку.
`pi_loop` открывает новый stream с тем же принятым контекстом, включая результаты
уже выполненных инструментов. Каждая попытка видна как отдельный model exchange.
Профиль использует штатный бюджет Pi 1.5.2: до трёх повторов с базовыми задержками
2, 4 и 8 секунд. Отмена прерывает ожидание. Инструменты получают вызов после окончательного ответа.
Прежние результаты остаются в журнале с их исходными call IDs. Отказы авторизации,
квоты и неизвестные ошибки завершают запрос. Отдельная операция, исход которой
не был подтверждён до аварийного завершения процесса, требует проверки состояния.

У Flash адаптер также распознаёт EOF, при котором в SSE отсутствует `finish_reason`.
Повторы при установлении HTTP-запроса остаются в штатном `retryProviderRequest`
Pi. Отдельный бюджет workflow применяется к распознанным транспортным сбоям.

Streaming модели передаёт обычный текст и опубликованный Pi `thinking_delta`
в отдельные события Core; reasoning-событие связано с ID итогового сообщения.
Reasoning остаётся отдельной частью итогового ответа. Redacted-блоки хранятся
как `Opaque` с форматом `pi/thinking` и восстанавливаются только для Pi replay;
они и непрозрачные подписи не показываются как поток текста.

### Лимиты OpenCode Go

Model export с явным `implementation = "opencode-go"` предоставляет
`model.quota.v1`. Запросы метаданных используют тот же credential resolver Pi,
что и генерация. Лимиты Go возвращаются единым общим bucket с окнами 5 часов,
недели и месяца. Месячная продолжительность не подменяется фиксированным числом
дней, время сброса берётся из ответа. Проценты не преобразуются в доллары
по опубликованным тарифным таблицам.

Денежные бюджеты Console загружаются отдельно и помечаются `informational`,
поскольку метод quota не получает текущую модель. HTTP 403 этого endpoint
оставляет доступными лимиты Go и добавляет подсказку проверить `Budgets Read`
и права аккаунта. Для service account и старого ключа доступ также может
отличаться. Остальные ошибки бюджетов сообщаются отдельно. Общий refresh
кэшируется 30 секунд по credential, конкурентные читатели объединяются.
Отмена всех читателей прекращает HTTP, общий таймаут составляет 15 секунд.
Необязательный запрос денежных бюджетов ограничен 5 секундами; его таймаут
публикуется отдельным уведомлением и сохраняет уже полученные лимиты Go.

Mapping опирается на
[upstream usage-limits.ts](https://github.com/anomalyco/opencode/blob/1fa50dd28cc860768fd89180cf40b2fb912b2813/packages/core/src/usage-limits.ts).
Проверка `node --test tool/pi-tools/tests/quota.test.mjs` использует
синтетические ответы и не вызывает модель.
