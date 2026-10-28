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
