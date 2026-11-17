# Session Storage Component

Самостоятельный executable `proteus-session-storage-module` предоставляет
`session_storage/jsonl` и `session_storage/sqlite`, contract `v1`, composition
`select_one`. Каждый явно выбранный export имеет независимое состояние и не
получает host callbacks. Конфигурация export — пустой object.

Описание форматов, гарантий и проверок находится в
[storage/session-storage](../../storage/session-storage/README.md).

В конфигурации агента выбирается export и его процесс:

```toml
[modules]
session_storage = "jsonl"

[components.session-storage]
command = "proteus-session-storage-module"

[components.session-storage.transport]
max_frame_bytes = 134217728

[components.session-storage.exports.session_storage.jsonl]
```

Для SQLite замените `jsonl` на `sqlite` в обеих строках. Это выбор формата
для новых сессий; автоматического переноса существующего журнала нет.
Повышенный frame budget действует только на этот компонент и позволяет
читать целиком сохранённые большие записи.
