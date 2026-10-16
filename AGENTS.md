# Работа С Proteus-modules

Этот репозиторий содержит реализации, а не Core и не UI. Читайте README.md,
Cargo.toml и документацию соответствующего модуля. Публичные contracts и
process adapters принадлежат [Proteus-harness](https://github.com/Qweasd123tg/Proteus-harness).
Документация — на русском языке.

## Границы

- Инвариант: Core → Contract → Module Implementation. Реализации не связывайте
  напрямую между собой; общие DTO берите из proteus-contracts.
- Реализации расположены по основному slot. Модуль с несколькими exports
  хранится один раз; каждый export сохраняет authority своего slot.
- components/process-module собирает библиотеки в reference executable. Это
  обычный процесс, без особого ABI или исключений по имени.
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
Runner задаёт PROTEUS_TEST_HARNESS_ROOT из закреплённого Cargo dependency и
готовит DCP; не запускайте сборки или установки из Rust tests. Выбирайте
реальные проверки поведения и контрактов, без тестов на расположение исходников.
После успешной проверки создайте отдельный git commit. Не повторяйте полный
прогон после правки только tests/fixtures: достаточно упавшего target.

При изменении публичной границы обновите harness и его module_swap gate.
После завершённой правки приложения/backend пересоберите portable в соседнем
Proteus-desktop (`../../GPUI/Client` при общей раскладке):
`python3 scripts/portable.py --fast`. Сохраняйте target и инкрементальные caches.
