# Работа С Proteus-modules

Этот репозиторий содержит реализации, а не Core и не UI. Читайте README.md,
Cargo.toml и документацию соответствующего модуля. Публичные contracts и
process adapters принадлежат [Proteus-core](https://github.com/Proteus-harness/Proteus-core).
Документация — на русском языке.

## Границы

- Инвариант: Core → Contract → Module Implementation. Реализации не связывайте
  напрямую между собой; общие DTO берите из proteus-contracts.
- Реализации расположены по основному slot. Модуль с несколькими exports
  хранится один раз; каждый export сохраняет authority своего slot.
- components/<id>-module — независимые исполняемые пакеты. Общая обвязка
  components/module-runtime не зависит от implementations и использует Core SDK.
  components/process-module — aggregate fixture только для тестов, не поставка.
- Не возвращайте dylib/native registrations, compatibility readers, старые
  aliases или новые behavior defaults в Core. Черновые producers и consumers
  обновляются вместе.
- Доверенные собственные модули — текущий сценарий. Не добавляйте sandbox или
  hardening в задачу без поручения владельца.
- Совместимые с upstream реализации повторяют его семантику. Улучшения — только
  отдельным явно названным режимом. Сохраняйте LICENSE, NOTICE и происхождение.
- Держите файлы связными и небольшими; при 500–700 строках рассмотрите разделение.

## Стоимость Runtime

Для затратных путей действуют [правила стоимости реализаций](docs/reference.md#стоимость-реализаций)
и общий [стандарт расхода ОЗУ](https://github.com/Proteus-harness/Proteus-core/blob/main/docs/development/memory.md).
Выигрыш в latency, памяти или CPU подтверждайте замером на сопоставимых входах.

## Проверка

`python3 scripts/test.py -p PACKAGE ...` — затронутый scope,
`python3 scripts/test.py full` — взаимодействие crates, contracts или wiring.
Runner задаёт PROTEUS_TEST_HARNESS_ROOT из закреплённого Cargo dependency;
DCP отложен в research/dcp и исключён из штатной сборки и проверок.
Не запускайте сборки или установки из Rust tests. Выбирайте
реальные проверки поведения и контрактов, без тестов на расположение исходников.
После успешной проверки создайте отдельный git commit. Не повторяйте полный
прогон после правки только tests/fixtures: достаточно упавшего target.
При расхождении модуля со стендом определите, какой участник нарушил контракт.
[Разбор падений в Core](https://github.com/Proteus-harness/Proteus-core/blob/main/docs/development/testing.md#разбор-падений).

При изменении публичного контракта обновите его производителей, потребителей
и проверку заменяемости в Core. Для правки только документации достаточно
проверить содержание, ссылки и `git diff --check`.

Один изменённый модуль установите в готовую поставку из этого репозитория
командой `python3 package.py --package ID --install /absolute/ready/Proteus`.
Для передачи пакета в виде ZIP используйте `python3 package.py --package ID`.
Если изменился набор поставляемых модулей, выберите компонент `modules` по
[руководству по сборке Desktop](https://github.com/Proteus-harness/Proteus-desktop/blob/main/BUILDING.md#выбор-компонентов).
Сохраняйте инкрементальные кэши.
