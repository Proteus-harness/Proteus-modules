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
- Правка одного пакета: `python3 package.py --package ID`; установка без сборки
  хостов/остальных пакетов: `--package ID --install /path/to/Proteus`.
  Не собирайте reference fixture, Core или Desktop для обычной правки пакета.
- Не возвращайте dylib/native registrations, compatibility readers, старые
  aliases или новые behavior defaults в Core. Черновые producers и consumers
  обновляются вместе.
- Доверенные собственные модули — текущий сценарий. Не добавляйте sandbox или
  hardening в задачу без поручения владельца.
- Совместимые с upstream реализации повторяют его семантику. Улучшения — только
  отдельным явно названным режимом. Сохраняйте LICENSE, NOTICE и происхождение.
- Держите файлы связными и небольшими; при 500–700 строках рассмотрите разделение.

## Проверка

`python3 scripts/test.py -p PACKAGE ...` — затронутый scope,
`python3 scripts/test.py full` — взаимодействие crates, contracts или wiring.
Runner задаёт PROTEUS_TEST_HARNESS_ROOT из закреплённого Cargo dependency;
DCP отложен в research/dcp и исключён из штатной сборки и проверок.
Не запускайте сборки или установки из Rust tests. Выбирайте
реальные проверки поведения и контрактов, без тестов на расположение исходников.
После успешной проверки создайте отдельный git commit. Не повторяйте полный
прогон после правки только tests/fixtures: достаточно упавшего target.

При изменении публичной границы обновите harness и его module_swap gate.
После завершённой правки приложения/backend пересоберите portable в соседнем
Proteus-desktop (`../Proteus-desktop` при общей раскладке). Выбирайте только
затронутые компоненты по его BUILDING.md: для одного модуля достаточно
`python3 package.py --package ID --install /absolute/ready/Proteus` из Modules;
для всего набора — `python3 scripts/portable.py --components modules --no-archive`
из Desktop. `--fast` удалён; обычный профиль — release.
Сохраняйте target и инкрементальные caches.
