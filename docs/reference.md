# Reference Modules

Здесь лежат implementations, которыми Proteus проверяет contracts и собирает
текущие dogfood profiles. Это не стандартная библиотека модулей, не набор
обязательных defaults и не привилегированный слой runtime.

Все реализации экспортируются через один исполняемый
`proteus-reference-module`. Наличие crate в этой папке само по себе не означает
активацию: config явно описывает process command и выбирает конкретный
`module_id`.

Reference-модуль использует тот же публичный process protocol, что и внешний
модуль, проходит тот же conformance gate и не получает исключений по имени или
расположению исходников. Crates в этом каталоге — только implementation detail
этого executable, а не особый способ загрузки модулей.

Model exports подтверждают только предложенные `model.catalog.v1` и
`model.quota.v1`. Неизвестные optional host features пропускаются; отсутствие
feature не мешает обязательным describe/stream. Корневой handshake принимает
описательные `_meta` с авторскими ключами `namespace/name`; данные не меняют
slot authority.

Tool definitions внутри reference executable фиксируются один раз при загрузке
каждого tool export: порядок каталога, точные имена, schema, visibility и
parallel capability относятся к этой реализации и её config. Повторная загрузка
создаёт новый каталог; между exports он не разделяется. Invoke использует индекс
имён без повторного разбора ToolSpec. Дубликаты остаются в list для прежней
проверки Core, а внутренний поиск сохраняет первый совпавший корректный spec.
Ошибочный ToolSpec по-прежнему приводит к ошибке list; проверка schema,
policy/approval и authority остаётся на прежних границах.
