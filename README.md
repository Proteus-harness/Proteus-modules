# Proteus Modules

Реализации публичных контрактов [Proteus-core](https://github.com/Proteus-harness/Proteus-core).
Текущая версия первого согласованного набора — `0.1.0-alpha.3` (experimental).
Версии DCP и процессных контрактов учитываются отдельно. Совместимость между
alpha-выпусками не обещана; потребитель закрепляет конкретную ревизию.
Исходники сгруппированы по основному слоту:

| Каталог | Реализации |
|---|---|
| `model/` | Провайдеры моделей и авторизация |
| `workflow/` | Алгоритмы выполнения задач |
| `context/` | Построение контекста |
| `policy/` | Разрешения и подтверждения |
| `hook/` | Обработка истории, отбор инструментов, DCP |
| `tool/` | Файлы, shell, git, patch, plan, skills, Rust LSP |
| `context_provider/` | Поиск и память; некоторые также экспортируют tool |
| `components/` | Исполняемые сборки с несколькими exports |
| `research/` | Эксперименты вне основного Cargo workspace |

Размещение не выбирает реализацию в профиле и не даёт ей дополнительных прав.
Модуль с несколькими exports хранится один раз по основной ответственности;
каждый export имеет права своего контракта. Текущее устройство reference
executable описано в [reference.md](docs/reference.md).

## Сборка И Проверка

Нужны Rust, Git, Python 3; Node gates DCP и Claude command hooks дополнительно
требуют Node.js 22.18+ и npm.
Контракты и test harness подключены по фиксированному Git commit в Cargo.toml;
соседний checkout harness для обычной сборки не требуется.

```bash
cargo build --locked -p proteus-reference-module
python3 scripts/test.py full
python3 scripts/test.py -p rg-search --lib
```

Runner получает путь к harness из Cargo metadata: интеграционные тесты читают
его fixtures и профили из той же ревизии, против которой собраны зависимости.
Полный gate и scope `-p proteus-reference-module` включают Node-тесты DCP и
Claude command hooks. Для последних runner задаёт `PROTEUS_HOOK_SDK_ROOT`
из той же закреплённой зависимости Core, поэтому SDK integration tests
не пропускаются из-за отсутствия переменной окружения. Никакие модели
с платным API не вызываются.
`research/tool-output-artifacts` собирается отдельным `--manifest-path`.

GitHub Actions запускает этот gate только вручную. Обычные push и PR не
запускают проверки. Общая релизная сборка запускается тегом `v*` в
Proteus-desktop и использует закреплённую ревизию Modules.

Основной исполняемый файл — `target/debug/proteus-reference-module`.

На Linux/glibc reference executable до запуска workers ограничивает malloc
двумя аренами, чтобы уменьшить удержание свободной памяти при множестве
компонентов. Явные `MALLOC_ARENA_MAX` и `GLIBC_TUNABLES` с
`glibc.malloc.arena_max` имеют приоритет; process-host environment policy
при этом не меняется.
Он использует тот же process protocol, что и любой внешний модуль. Модули
не зависят от приложения и его расширений. Конфигурация slots и примеры:
[документация harness](https://github.com/Proteus-harness/Proteus-core/blob/main/docs/architecture/modules.md).

## DCP И Происхождение

[hook/dcp](hook/dcp/README.md) — отдельный Node executable с hook/tool exports;
он не линкуется в Rust reference-module. Прочие реализации собираются как
ordinary Rust libraries внутри этого executable, без отдельного native ABI.
Код DCP и его upstream имеют лицензию AGPL-3.0-or-later; сведения и notices
сохранены в его каталоге. Лицензии остальных заимствованных частей находятся
рядом с исходниками.

[hook/claude-command-hooks](hook/claude-command-hooks/README.md) — отдельный
Node entry для явно выбранного `strict_subset` Claude command hooks
(PreToolUse/Stop). Он использует закреплённый публичный hook-process SDK Core,
сохраняет scripts и command strings, предоставляет инспекцию без исполнения
и не входит в Rust reference executable. Это ограниченный путь переноса,
не полная совместимость с чужим harness. Node-тесты входят в общий gate;
отдельный запуск описан в README адаптера.

История implementations перенесена из `Proteus-agent` (ныне Proteus-core),
авторы и коммиты сохранены. После выделения исходники размещены по слотам.
