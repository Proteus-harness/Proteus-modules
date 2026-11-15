# Reference Modules

Здесь лежат implementations, которыми Proteus проверяет contracts и собирает
текущие dogfood profiles. Это не стандартная библиотека модулей, не набор
обязательных defaults и не привилегированный слой runtime.

Реализации экспортируются самостоятельными `proteus-<id>-module` из
`components/<id>-module`. Каждый пакет линкует только свою implementation и
общую обвязку `components/module-runtime`, не зависящую от implementations.
`exports.json` каждого пакета задаёт разрешённые exports; model допускает
настраиваемые имена bindings. Наличие пакета не означает активацию:
config явно описывает process command и выбирает конкретный `module_id`.

`components/process-module` / `proteus-reference-module` — только aggregate
fixture. Он нужен для проверки равенства manifests, tool catalogs и результатов
новых процессов, но не входит в стандартную сборку и поставку.

Reference-модуль использует тот же публичный process protocol, что и внешний
модуль, проходит тот же conformance gate и не получает исключений по имени или
расположению исходников. Каждый самостоятельный пакет использует тот же protocol/SDK и не получает
исключений по имени или расположению исходников.

Model exports подтверждают только предложенные `model.catalog.v1` и
`model.quota.v1`. Неизвестные optional host features пропускаются; отсутствие
feature не мешает обязательным describe/stream. Корневой handshake принимает
описательные `_meta` с авторскими ключами `namespace/name`; данные не меняют
slot authority.

## Native OpenCode Model

`model/opencode-provider` экспортирует `model/opencode` (`v13`) через
`proteus-opencode-module`. Он вызывает pinned `ModelsDev`, `ModelResolver` и
`LLMClient` OpenCode напрямую. Это отдельный model slot: выбор его в профиле
Codex сохраняет выбранные workflow, context, tools, policy и compactor Codex;
native agent prompt и OpenCode session loop не добавляются.

Обязательный `module_config.model.opencode.provider` выбирает native provider
(например, `opencode-go`). Каталог `model.catalog.v1` публикует его chat-модели,
названия, hidden status и точные native effort variants. Каталог удерживается
на пять минут с объединением одновременных запросов; после TTL обновляется при
следующем обращении, без фонового refresh. Metadata snapshot имеет ограничение
на удержание; слишком большой ответ обслуживается без помещения в кеш.
Ошибки каталога не заменяются статическим списком aliases. Source metadata и
capabilities берутся из ModelsDev; image/audio generators и записи без
положительных context/output limits не входят в chat surface.

API key читается из credential table OpenCode v2 в read-only режиме. По умолчанию
это `$XDG_DATA_HOME/opencode/opencode.db`, либо
`~/.local/share/opencode/opencode.db`, если XDG-путь не абсолютный или не задан.
Сохранённый active/latest credential имеет приоритет перед переменными среды,
объявленными native provider. Адаптер не создаёт и не мигрирует DB, не использует
Pi auth и старый `auth.json`. SQLite WAL/SHM coordination sidecars допустимы:
чтение видит committed WAL без изменения данных и checkpoint. OAuth требует native refresh и в этом ограниченном
model export явно отклоняется. Дополнительные параметры: `database_path`,
`models_file`, `models_url`; пример и способы проверки описаны в
[README runtime](../workflow/opencode-runtime/README.md#model-export).

`model.quota.v1` объявляется только для `opencode-go`: Go usage windows и USD
показатели используют тот же ключ и общий stateless helper
`components/model-runtime-js/opencode-go-quota.mjs`. Это не выбор дополнительных
agent slots. Canonical usage сохраняет inclusive input/output totals native SDK;
cached input и reasoning tokens передаются отдельно и повторно не прибавляются.

Transport использует native SDK semantics, включая function tools с
`strict=false` и native argument recovery. Freeform tools, provider-executed
tools, structured response format и чужие opaque history parts не поддержаны
и не превращаются молча в текст. Не заданный `max_output_tokens` остаётся
не заданным на wire; скрытый cap не добавляется. Text, visible reasoning,
tool-call history и native metadata проходят через canonical bridge; прерванный
ответ не считается завершённым сообщением. Эти границы не означают полную
эквивалентность всей vanilla OpenCode упряжке.

Tool definitions внутри каждого процесса фиксируются один раз при загрузке
каждого tool export: порядок каталога, точные имена, schema, visibility и
parallel capability относятся к этой реализации и её config. Повторная загрузка
создаёт новый каталог; между exports он не разделяется. Invoke использует индекс
имён без повторного разбора ToolSpec. Дубликаты остаются в list для прежней
проверки Core, а внутренний поиск сохраняет первый совпавший корректный spec.
Ошибочный ToolSpec по-прежнему приводит к ошибке list; проверка schema,
policy/approval и authority остаётся на прежних границах.

## Codex Base Instructions И Patch Surface

Пакет `codex-context-preparation` дополнительно экспортирует
`hook/codex_base_instructions`. На `BeforeModel` прямого запроса он выбирает
публичный model-specific `instructions_template` из OpenAI Codex
`rust-v0.167.0`, commit `086ccc833bc9cf01024ad2dd6af7dbd421b6bf9a`.
Включены `gpt-6.1-sol`, `gpt-6.1-terra`, `gpt-5.6-luna`; для остальных
используется upstream `default.md`. Пользовательские инструкции сохраняются.
Источники, лицензия Apache-2.0 и SHA256 находятся в
`hook/codex-context-preparation/src/upstream/0.167.0/`.
Auxiliary summary requests сохраняют собственную подготовку.

В `codex_patch` параметр `freeform=true` публикует custom tool с точной
upstream Lark grammar этой версии. Canonical input имеет `{input: string}`;
проверка schema, policy и workspace выполняется до применения patch.
Default `freeform=false` сохраняет function binding `{patch: string}`.
Профиль Core `codex` выбирает freeform; `code-mode` явно сохраняет function
binding для своего `tools.call` API.

Публичный pinned каталог отличается от обновляемого `/models` каталога
конкретной native CLI-сессии. Применение pinned template и grammar не закрывает
остальные fidelity gates. Native CLI `0.167.0` по умолчанию предоставляет
programmatic `exec` через отдельный code-mode-host; local summary compactor
и protocol/tool/session orchestration этой сборки проверяются отдельно.

## Окончания Строк В Direct Patch

`direct_patch` сохраняет CRLF при update, если все имеющиеся newline исходного
файла используют CRLF. LF/CRLF самого patch не выбирает EOL результата.
Отсутствие final newline сохраняется; `*** End of File` убирает final newline.
Файл без newline и `Add File` используют прежний LF формат. Bare CR остаётся
данными. Mixed LF/CRLF source сохраняет прежний exact LF-based путь без общей
нормализации: CRLF строки mixed-файла по-прежнему могут не совпасть с hunk.
Это не меняет newline representation в `file-tools` ranged reads/edits.

## SQLite Memory: Запрос Recall

`sqlite_memory` использует одну нормализацию запроса для `recall_memory` и
`context_provider/sqlite_memory`. Части запроса, разделённые пробелами,
соединяются через `AND`; каждая часть передаётся как буквальная FTS5-фраза.
Пунктуацию и диакритику внутри фразы обрабатывает tokenizer SQLite, как при
индексировании: `src/config.rs` не превращается в `srcconfigrs`.
Порядок и соседство слов внутри части сохраняются; prefix применяется только
к последнему токену каждой фразы, а не ко всем её словам. Это не сравнение
строк или точное совпадение пунктуации.

Вложенные кавычки передаются буквально. NUL внутри части заменяется пробелом,
так как parser MATCH использует NUL как конец выражения. Части без букв/цифр,
`_` или `-` по-прежнему пропускаются. Если частей не осталось, возвращаются
последние записи; части из одних `_`/`-` сохраняют прежнюю FTS-семантику пустой
фразы. `limit=0` по-прежнему возвращает пустой список. Схема базы, формат
записей, ранжирование и лимиты не меняются.

## CodeMode

Самостоятельный пакет `code-mode` экспортирует `tool/code_mode` и инструмент
`code_mode({code, timeout_ms?})`. Он исполняет JavaScript во встроенном QuickJS;
каждый вызов получает новое состояние. `tools.call(name, args)` возвращает
Promise с полным `ToolResult`, включая `ok`, `output`, `error` и `metadata`.
Аргументы соответствуют зарегистрированной function-схеме: например,
`tools.call("apply_patch", {patch: patchText})`.
Неуспешный результат инструмента нужно проверять явно:

```javascript
const results = await Promise.all(
  ["a.txt", "b.txt"].map(path => tools.call("read_file", {path}))
);
const failed = results.filter(result => !result.ok);
text({read: results.length - failed.length, failed: failed.length});
```

`text(value)` и `console.log(...values)` печатают ограниченный вывод. В следующий
запрос модели попадают этот вывод и служебный итог `code_mode`; сырые дочерние
результаты остаются переменными скрипта и фактами canonical journal. Возвращаемое
значение JavaScript само по себе не печатается. Счётчики неуспешных дочерних
вызовов сохраняются и тогда, когда скрипт обработал ошибку. `Promise.all`
допускает параллельную отправку, но реальное исполнение дополнительно подчиняется
общему scheduling и `supports_parallel_tool_calls` каждого инструмента.

Вызовы проходят существующий `host.tools.execute` и общий путь schema validation,
hooks, policy/approval, cancellation и journal; новый контракт Core не требуется.
Из JavaScript не доступны файловая система, сеть, команды ОС, загрузчик модулей
или model callbacks. Для эффектов используются зарегистрированные инструменты.
Лимиты ограничивают код, память и stack QuickJS, печать, число и параллелизм
вызовов, а также время скрипта. Это граница возможностей JavaScript, а не OS
sandbox всего worker-процесса. Уже допущенные host calls завершаются в границах
родительского invocation Core; deadline скрипта не обещает мгновенную остановку
или откат их эффектов. Фоновое исполнение и состояние между скриптами отсутствуют.

Установка пакета: `python3 package.py --package code-mode --install /path/to/Proteus`.
После установки укажите command `proteus-code-mode-module`, export
`components.<id>.exports.tool.code_mode` и включите `code_mode` в `tools.enabled`.
Самостоятельный профиль находится в
[профиле Core `code-mode`](https://github.com/Proteus-harness/Proteus-core/blob/main/configs/code-mode.config.toml).
При использовании `tool_selection.dynamic` добавьте `code_mode` в `always_include`.
`tools.call` принимает настоящие registry tool names: workflow-операции
`proteus_tool_search`, `proteus_tool_describe` и `proteus_tool_call` внутри скрипта
недоступны. Модель выполняет discovery до вызова `code_mode`.

V1 сохраняет прямые описания доступных tools и добавляет добровольно выбираемый
скриптовый инструмент. Уменьшение числа запросов и объёма контекста зависит от
задачи; сравнение расхода токенов на живых моделях пока не проводилось.

## История И Подготовка Workflow

`coding.single_loop`, `coding.plan_execute_review` и `coding.codex_loop`
закрывают старые вызовы без результата request-only ответом `aborted` до
подготовки контекста. Настоящий результат сохраняется. Это нормализация
запроса модели, а не повтор исполнения или запись вымышленного результата
в canonical history. Compactor получает такие пары во входе для summary,
но не возвращает отделённый request-only результат после удаления вызова.

Обычный и plan/execute/review workflows передают runtime instruction blocks
между базовыми и фазовыми инструкциями, сохраняя их порядок и приоритеты.
Общая sampling-подготовка подтверждает замену истории через checkpoint ACK
перед запросом модели; forced-final и review по-прежнему отключают tools.
Непустые внутренние plan/draft projections имеют собственные message/part
identities и остаются request-only; tool-bearing ответы и окончательный ответ
сохраняют canonical identities и прежние правила сохранения. Сообщения без
parts сохраняют прежнее ограничение классификации при ACK/compaction.

## Настройки Model-pack

Форма подключения описывает параметры адаптера, а не автоматически
определённые возможности выбранной модели. Параметры разделены на подключение,
авторизацию, кэширование, ответ модели, совместимость API и сеть/диагностику.
Основной файл авторизации ChatGPT или переменная с API-ключом доступны сразу;
альтернативные источники и технические overrides остаются дополнительными.
Секции — metadata представления: плоские config keys и реальные вложенные
объекты сохраняются. Модель и reasoning выбираются отдельно в чате.

Группа `capabilities` («Разрешения адаптера») вручную разрешает отправку
изображений, параллельных/freeform tools, JSON Schema, reasoning config и
verbosity. Включать следует только формат, который принимает конкретный
сервер и модель. Форма показывает одну вложенную группу без дублей плоских
флагов. Runtime сохраняет прежний приоритет: вложенное значение, иначе
одноимённый плоский параметр, иначе false; default формы учитывает тот же
приоритет. Это изменение представления, а не миграция существующего config.
Невалидное вложенное значение не маскируется плоским fallback.

`capabilities.hosted_tools` — список вручную объявленных поддерживаемых
семейств, а верхний `hosted_tools` — отдельный объект параметров их вызова.
Список сам по себе не включает инструмент и не проверяет его доступность
на сервере. `max_input_tokens` задаёт объявленный адаптером локальный бюджет
входного контекста Proteus, не увеличивает серверное окно модели. Без явного
значения адаптер не объявляет собственный предел.

`prompt_cache` включает подсказки провайдеру; `prompt_cache_key` задаёт ключ
маршрутизации кэша. Попадание в кэш определяет сервер: включённый флаг и
совпавший ключ не гарантируют cache hit. Это кэширование входного контекста
на стороне провайдера, а не локальный кэш готовых ответов.
`client_version` задаёт версию в запросе каталога Codex и не меняет версию
установленного клиента или выбранную модель.

Anthropic Messages сохраняет `redacted_thinking` как непрозрачный canonical
`ContentPart::Opaque` с форматом `anthropic.messages.redacted_thinking`.
Весь исходный JSON-блок, включая зашифрованное `data`, остаётся в исходной
позиции среди `thinking`, текста и `tool_use`: non-stream и SSE проходят
одинаковый путь хранения и продолжения после `tool_result`. Блок не становится
отображаемым текстом и не интерпретируется. При повторном запросе Anthropic
получает его без изменения; неизвестный opaque-формат или повреждённый блок
отклоняются. OpenAI Responses отклоняет такие opaque-части явно, поэтому смена
провайдера не может незаметно удалить их из истории. Это сохраняет
[контракт Anthropic для redacted thinking](https://platform.claude.com/docs/en/build-with-claude/thinking#redacted-thinking-blocks).

Anthropic SSE сохраняет стартовый текст блока и стартовый `tool_use.input`,
если JSON-дельты не заменили его. Закрытие HTTP 200 SSE без terminal event
возвращает `StreamDisconnected`; документированный overload возвращает
`Retryable`, а HTTP 400 `prompt is too long` — `ContextWindowExceeded`.
Тот же context failure возвращается для HTTP 200 ответа с
`stop_reason=model_context_window_exceeded`; `max_tokens` сохраняет
`FinishReason::Length`. Частичный текст и tool arguments не становятся
завершённым ответом при failure. Расход failed attempts этим не восстанавливается.

OpenAI Responses и Anthropic Messages передают inline images из tool results
после прежнего текстового/error представления результата. В OpenAI это требует
разрешённого `image_input`. Это не добавляет поддержку произвольных binary/PDF
вложений или новых лимитов изображений и не гарантирует поддержку формата
каждым совместимым сервером.

## Границы File-tools И ReadOnly Git

`read_file` ограничивает возвращаемый текст и одну строку 2 MiB независимо от
`start_line`, `limit` и `line_numbers`. Чтение диапазона дополнительно ограничено
16 MiB входа; после нужного числа строк остаток файла не просматривается ради
подсчёта. Ответ содержит `truncated`, `truncation_reason` и `scanned_bytes`;
`total_lines` равен `null`, если EOF не был достигнут. Полное чтение файла больше
2 MiB требует явно выбрать диапазон, который также подчиняется этим бюджетам.

Plain ranges сохраняют LF/CRLF между выбранными строками, исключая последний
разделитель; numbered output использует LF для отображения. Неполный UTF-8
символ допускается отбросить только при установленном byte-budget cut.
Невалидный UTF-8 на наблюдённом EOF возвращает ошибку во всех режимах чтения;
старый размер файла из metadata не доказывает остановку по бюджету.
Ошибка перечисления после открытия каталога в `list_dir` тоже возвращает
ошибку, а не успешный неполный список.

`write_file` и запись результата `edit_file` используют общий
`proteus_contracts::tool_support::workspace_file_for_write`. Он возвращает
открытый `WorkspaceFile`: запись выполняется через его `Write`, а `path()`
служит только для сообщений. Unix API открывает компоненты относительно
дескрипторов каталогов без следования symlink; Linux дополнительно использует
`openat2` с `BENEATH | NO_SYMLINKS`. На неподдерживаемой платформе операция
явно отклоняется. Проверенный строковый путь не разрешает последующий
`std::fs::write` и не заменяет этот API.

ReadOnly `git_status` / `git_diff` и workspace Git API Core используют общий
`proteus_contracts::git_support::protect_readonly_git`. Совпадение эффективного
атрибута `filter` у файла с настроенным исполняемым `clean` или `process`
приводит к явному отказу до чтения содержимого Git; filtered diff не подменяется
нефильтрованным. Неиспользуемый настроенный драйвер и неизвестный драйвер без
исполняемой конфигурации не мешают обычному чтению. Поддержка исполнения
драйвера требует отдельной операции с соответствующими полномочиями; один
`--no-textconv` эту границу не обеспечивает.

## Живой Просмотр Терминала

Опциональная capability `tool_terminal/v1` у `exec_command` публикует handle
сразу после запуска, до ожидания первого результата. Core связывает его с
исходным session/thread/workspace и поколением загруженного модуля; публичный
идентификатор клиента не совпадает с числовым session ID инструмента.
Просмотр читает отдельный raw ring по byte cursor и не забирает вывод модели.
Потеря вытесненного префикса отмечается `gap`; один ответ ограничен 64 KiB,
ожидание — одной секундой. `finished` выставляется только после выхода процесса
и остановки всех readers, поэтому последний ответ нужно дочитать до end cursor.

Модуль удерживает не более 32 записей по 1 MiB; завершённые вытесняются первыми
и истекают через 30 минут после завершения. Активные записи не вытесняются;
при исчерпании бюджета новая команда отклоняется до spawn. Запись не удерживает
живой процесс: ссылка на execution слабая. Лимит 16 model exec sessions и их LRU
остаются отдельными. Чтение и resize не продлевают model idle timeout.

Для интерактивного ввода, включая пароль, агент должен выбрать `tty: true`.
Ввод идёт напрямую в PTY без очереди ожидания model poll и не сохраняется как
аргумент инструмента, metadata или отдельный source capture. Клиент не добавляет
локальное эхо ввода. Для пароля процесс должен отключить терминальное ECHO; это
не запрещает самому процессу напечатать полученные данные. Обычный вывод
процесса, включая его эхо или печать ввода, остаётся доступен модели и capture.
У pipes stdin закрыт, ввод и resize отклоняются. Закрытие вкладки ничего не
останавливает; явный Stop завершает процесс. Одноразовый `shell` сохраняет свою
прежнюю семантику и не создаёт интерактивный терминал.

## Завершение Readers В Shell Exec

В `shell_tools` выход команды, остановка output readers и полнота потока —
разные факты. У pipes два readers, у PTY один. Каждый сохраняет terminal reason:
EOF, ошибка чтения или исчерпание exit-drain grace. Остановка всех readers
завершает ожидание независимо от причины; ожидание также ограничено yield
deadline и exit-drain grace. Возврат результата не доказывает полноту потока.
Exit code, cancellation и bounded head/tail
preview сохраняют прежнюю semantics. Этот внутренний учёт сам по себе не
включает capture на диск и не выдаёт artifact receipt.

На Unix PTY и pipes сохраняют identity вышедшего лидера до окончания drain;
оставшаяся process group останавливается до reap. После reap старый session
handle уже не имеет права сигналить по сохранённому PID. Число `111`, как и
другие ненулевые exit codes, не имеет специальной lifecycle-семантики.

One-shot `shell` на Unix читает stdout/stderr неблокирующе. После timeout,
cancel или ошибки канала отмены даётся короткий drain, затем оба reader threads
закрывают свои pipes и присоединяются. Тихие readers ждут readiness; отмена
будит их через общий wake fd, без периодического опроса. Потомок, ушедший через `setsid` и
сохранивший эти fd, не может бесконечно продлить ожидание EOF; это не обещание
остановить процесс, который покинул принадлежащую вызову process group.

Текстовый preview `exec_command` / `write_stdin` сохраняет до трёх trailing bytes
незавершённого UTF-8 символа между незавершающими polls. Они выдаются вместе с
продолжением символа, а `output_bytes` считает только bytes текущей выдачи.
Заведомо невалидный/binary вывод по-прежнему отображается с replacement characters.
При terminal выдаче или закрытии всех readers оставшийся incomplete suffix
выдаётся через тот же lossy decoder; terminal status не доказывает полный EOF.
Head/tail truncation и исходный raw capture не меняются.

### Явный Source Capture Для Exec

Опциональный объект `source_capture` в существующем module config export
`tool/shell_tools` включает ephemeral raw-prefix storage для `exec_command` и
`write_stdin` (pipes и PTY). One-shot `shell` / Ptyxis не меняются. Пример:

```json
{"source_capture":{"mode":"ephemeral_file","directory":"/absolute/private/capture-dir","max_capture_bytes":8388608,"max_captures":8,"retention_seconds":300}}
```

Directory заранее выбирает/создаёт оператор; вывод может содержать secrets.
Нет content logs, uploader или отдельного network channel. Requested ranges
возвращаются обычным tool RPC и доступны выбранному host/model pipeline.
Отсутствующий объект не добавляет tool, files или writer threads. Неполный /
невалидный объект отклоняется до регистрации tools. Schema одинакова в
standalone shell package и aggregate `shell_tools` / `reference.tools` fixture.

Capture admission (quota, anonymous tempfile, writer) выполняется **до** session
pruning и command spawn; ошибка означает, что команда не запущена. После запуска
queue overflow / storage failure прекращают только capture: обычный drain,
bounded preview, original exit code и cancellation остаются прежними. Raw prefix
имеет отдельный cap; exact cap ещё не доказывает truncation, следующий observed
byte — доказывает. Все reader EOF нужны для complete_stream: exit/drain timeout,
read error, failed storage или saturated counter не превращаются в completeness.

Результат exec содержит opaque handle независимо от session_id. Только тот же
binding generation и frozen session/thread/workspace owner могут вызвать новый
ReadOnly tool `read_exec_capture({handle,offset,max_bytes,format})`. Read range:
1..65536 raw bytes (default 4096), offset default 0. `format=utf8` (default)
строго проверяет выбранный byte range: разрезанный multibyte codepoint или binary
data дают encoding error; `format=hex` возвращает точные bytes без потерь.
Tool output — JSON envelope с `text` либо `bytes_hex`, `range` и полным receipt;
metadata повторяет bounded status для typed consumers. Pending/failed тоже
видны модели, а не скрыты только в metadata. `range_eof` относится к сохранённому
prefix, **не** к исходному потоку. Failed capture не требует rerun команды.

Хранилище только в этом worker generation: anonymous files закрываются после
последнего владельца; restart/rebind не восстанавливает handles. TTL 1..1800 s
считается от terminal writer state, lookup отклоняет expired handle. Cleanup lazy,
не обещание физического удаления ровно в N секунд. In-flight read, начавшийся до
expiry, может завершиться после неё; lease сохраняет reservation до file close.
Weak session taps не удерживают sealed file после expiry/drop binding.

Лимиты **на binding**, не на весь процесс/RSS: cap от 1 байта до 64 MiB, captures 1..16,
произведение до 256 MiB. Queue 8×8192 bytes + writer-held и enqueue-candidate
chunks (до 10 payloads); stack buffers/read buffers/hex/JSON escaping/envelope
copies, small metadata, thread/allocator overhead и OS page cache — отдельно.
Не более четырёх active range reads, один file cursor защищён на seek+read.
Blocking OS I/O не имеет гарантии мгновенной отмены. Успешный seal означает
readable immutable prefix, не fsync durability или гарантированный capture под
любой нагрузкой. Количественный throughput/RAM benefit без измерений не заявлен.

## Стоимость Реализаций

Эти правила относятся и к самостоятельному пакету, и к aggregate fixture:

- Передавайте большие owned history/results дальше без deep clone, если
  прежний владелец их больше не использует; для чтения берите borrow.
  Snapshot допустим, когда frozen view, rollback или отдельный потребитель
  должен пережить изменение источника. Объясняйте этот инвариант рядом с копией.
- Immutable schema/validator подготавливайте один раз для bound export identity
  и generation с её config/schema, не на каждый invoke. Не делите состояние
  между exports только из-за совпадения tool name; reload/rebind/restart
  инвалидирует прежнюю подготовку. Каждый invoke проверяет собственные args
  и проходит актуальные host policy/approval, authority и cancellation.
  Ошибка schema остаётся ошибкой публичной операции: кеширование не разрешает
  accept-all, старую schema или незаметный перенос ошибки list/invoke в startup.
- Сохраняйте typed DTO/`Value` между in-process helpers. Концептуально:
  `Value → String → Value → adapter` заменяется на `Value → adapter`,
  а JSON кодируйте один раз у process/HTTP/storage writer. API, требующий строку,
  и подпись точных bytes — конкретные исключения, не повод для roundtrip
  между соседними helpers. Strict validation и wire format сохраняются.
  В OpenAI HTTP retry одна фаза повторяет уже подготовленный request: JSON
  body сериализуется один раз, попытки разделяют immutable bytes reqwest.
  Encoded body живёт до конца фазы, включая backoff; drop future освобождает
  его. После OAuth refresh начинается отдельная фаза со свежими headers.
  Non-cloneable streaming body по-прежнему строится для каждой попытки;
  число повторов, ошибки, backoff и принятый response/SSE не меняются.
- Token estimates, context projections и другие derived данные обновляйте
  по изменившемуся срезу. Переиспользование prefix требует доказанного
  append-only состояния; edit/truncate/reset или смена config/revision
  инвалидирует зависимые данные. Если соответствие неизвестно, явно
  пересчитайте из canonical input. Cache имеет ключ, предел entries/bytes,
  lifetime, eviction/invalidation и явный error path.
  Derived cache не хранит effective authority или решения policy. Явные
  grants/remembered approvals принадлежат policy contract; его владелец
  проверяет их scope/lifetime заново для каждого invocation.
  Для membership/lookups внутри одного ACK используйте временный индекс,
  а не вложенные полные обходы. Он строится из текущего canonical input:
  request-only context остаётся перед следующим сохранённым canonical
  message, без такого anchor — в конце, с прежним порядком внутри группы.
  Этот индекс не переиспользуется между ACK и не заменяет validation истории.
- Проверяйте admission/резервируйте очередь до крупных allocation/encoding;
  освобождайте reservation по окончании её области владения, а не по одному
  запросу cancel активной записи/effect. Coalescing до дорогой работы
  допустимо только для заменяемых contract events, не для journal facts,
  terminal events или обязательных deltas. Сохраняйте ordering и backpressure.

Не вводите запрет на любой clone, JSON или cache: сохраняйте точную semantics,
durability и upstream parity. Сначала используйте существующий module/boundary
сценарий для затронутых args, reload, error и cancellation, не добавляйте
тест, который только ищет слова в исходниках. «Убран повторный parse/clone»
можно подтвердить потоком данных; «стало быстрее/меньше RSS» — только замером
с revisions, одинаковыми входами, размерами, concurrency и cold/warm режимом.
Без такого замера не обещайте runtime-выигрыш; startup и real-wire costs
учитывайте отдельно.
