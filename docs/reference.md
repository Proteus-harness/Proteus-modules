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

Tool definitions внутри каждого процесса фиксируются один раз при загрузке
каждого tool export: порядок каталога, точные имена, schema, visibility и
parallel capability относятся к этой реализации и её config. Повторная загрузка
создаёт новый каталог; между exports он не разделяется. Invoke использует индекс
имён без повторного разбора ToolSpec. Дубликаты остаются в list для прежней
проверки Core, а внутренний поиск сохраняет первый совпавший корректный spec.
Ошибочный ToolSpec по-прежнему приводит к ошибке list; проверка schema,
policy/approval и authority остаётся на прежних границах.

## Завершение Readers В Shell Exec

В `shell_tools` выход команды, остановка output readers и полнота потока —
разные факты. У pipes два readers, у PTY один. Каждый сохраняет terminal reason:
EOF, ошибка чтения или исчерпание exit-drain grace. Остановка всех readers
завершает ожидание независимо от причины; ожидание также ограничено yield
deadline и exit-drain grace. Возврат результата не доказывает полноту потока.
Exit code, cancellation и bounded head/tail
preview сохраняют прежнюю semantics. Этот внутренний учёт сам по себе не
включает capture на диск и не выдаёт artifact receipt.

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
