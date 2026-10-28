# Proteus Modules

Реализации публичных контрактов [Proteus-core](https://github.com/Proteus-harness/Proteus-core).
Текущая версия первого согласованного набора — `0.1.0-alpha.3` (experimental).
Версии процессных контрактов учитываются отдельно. Совместимость между
alpha-выпусками не обещана; потребитель закрепляет конкретную ревизию.
Исходники сгруппированы по основному слоту:

| Каталог | Реализации |
|---|---|
| `model/` | Провайдеры моделей и авторизация |
| `workflow/` | Алгоритмы выполнения задач |
| `context/` | Построение контекста |
| `policy/` | Разрешения и подтверждения |
| `hook/` | Обработка истории, отбор инструментов |
| `tool/` | Файлы, shell, git, patch, plan, skills, Rust LSP |
| `context_provider/` | Поиск и память; некоторые также экспортируют tool |
| `components/` | Исполняемые сборки с несколькими exports |
| `research/` | Эксперименты вне основного Cargo workspace |

Размещение не выбирает реализацию в профиле и не даёт ей дополнительных прав.
Модуль с несколькими exports хранится один раз по основной ответственности;
каждый export имеет права своего контракта. Самостоятельные процессы и общий
тестовый fixture описаны в [reference.md](docs/reference.md).

## Сборка И Проверка

Нужны Rust, Git, Python 3; gate Claude command hooks дополнительно
требует Node.js 22.18+.
Контракты и test harness подключены по фиксированному Git commit в Cargo.toml;
соседний checkout harness для обычной сборки не требуется.

```bash
python3 package.py --package plan-tool
python3 package.py --package plan-tool --install /path/to/Proteus
cargo build --locked # только самостоятельные пакеты; без Core/Desktop
python3 scripts/test.py full
python3 scripts/test.py -p rg-search --lib
```

Runner получает путь к harness из Cargo metadata: интеграционные тесты читают
его fixtures и профили из той же ревизии, против которой собраны зависимости.
Полный gate и scope `-p proteus-reference-module` включают Node-тесты
Claude command hooks. Runner задаёт `PROTEUS_HOOK_SDK_ROOT`
из той же закреплённой зависимости Core, поэтому SDK integration tests
не пропускаются из-за отсутствия переменной окружения. Никакие модели
с платным API не вызываются.
`research/tool-output-artifacts` собирается отдельным `--manifest-path`.

GitHub Actions запускает этот gate только вручную. Обычные push и PR не
запускают проверки. Общая релизная сборка запускается тегом `v*` в
Proteus-desktop и использует закреплённую ревизию Modules.

Каждая из 18 групп реализаций имеет свой `components/<id>-module` и бинарник
`proteus-<id>-module`. `package.py --package <id>` собирает только этот пакет
и зависимости и создаёт `dist/<id>.zip`. `--install /path/to/Proteus` заменяет
только его бинарник и metadata в существующей поставке; Core, Desktop и другие
пакеты не собираются. Перезапустите backend после обновления: hot reload не
добавлялся. Изменение общего SDK/runtime требует пересборки его потребителей.

`package.py --bundle /path/to/runtime` готовит весь набор. `--no-build` использует
пути последней успешной сборки из `dist/.artifacts/<profile>.json` без Cargo и
без проверки свежести исходников. Пути берутся из Cargo artifacts, включая
внешние target/cache каталоги. Общий `proteus-reference-module` остался только
для conformance/differential tests и не входит в обычную поставку.

Авторизация ChatGPT выполняется через поставляемый пакет моделей:
`proteus-model-pack-module auth openai_codex login`.
Состояние локального входа: `proteus-model-pack-module auth openai_codex status`.

На Linux/glibc каждый worker до запуска потоков ограничивает malloc
двумя аренами, чтобы уменьшить удержание свободной памяти при множестве
компонентов. Явные `MALLOC_ARENA_MAX` и `GLIBC_TUNABLES` с
`glibc.malloc.arena_max` имеют приоритет; process-host environment policy
при этом не меняется.
Он использует тот же process protocol, что и любой внешний модуль. Модули
не зависят от приложения и его расширений. Конфигурация slots и примеры:
[документация harness](https://github.com/Proteus-harness/Proteus-core/blob/main/docs/architecture/modules.md).

## Отложенные Эксперименты И Происхождение

[research/dcp](research/dcp/README.md) — отложенный эксперимент с отдельным
Node executable и hook/tool exports. Он исключён из штатных профилей,
поставки и общего test gate; npm запускается только вручную в его каталоге.
Библиотечные реализации линкуются в отдельные процессные пакеты. Общая обвязка
`components/module-runtime` не зависит от implementations; отдельного native ABI нет.
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
