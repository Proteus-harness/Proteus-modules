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
