# Источник Алгоритма

Reference export `tool/codex_patch` адаптирован из OpenAI Codex commit
`67cc3c318dc8b5532db6ade4182b1dc6f3870889`:

- `codex-rs/apply-patch/src/{parser,streaming_parser,seek_sequence}.rs` — parser
  и поиск context; streaming parser и его fixtures перенесены с отделением tests;
- `file_update.rs` — только default-ветка `NormalizeToLf`; `update.rs`
  использует строки и список replacements без exec-server и diff renderer;
- `invocation.rs::try_verify_apply_patch_args` и `lib.rs::apply_hunks_to_files` —
  предварительная проверка, повторное чтение при применении, порядок writes,
  отсутствие rollback после ошибки записи и группировка success summary;
- `core/src/tools/handlers/apply_patch.rs` — prefix ошибок verification и
  отказ при Environment ID в локальном режиме.

[Исходный код](https://github.com/openai/codex/tree/67cc3c318dc8b5532db6ade4182b1dc6f3870889/codex-rs/apply-patch)
распространяется под Apache-2.0; [LICENSE](LICENSE) и [NOTICE](NOTICE) включены
рядом. Перенесённые файлы помечены как адаптированные.

Выбран локальный режим и default `ApplyPatchPreserveLineEndings = false`.
Путь исполнения — общий `tool/v6`: canonical `ToolCall` с opaque текстом
`args.patch` (function) либо `args.input` (freeform), cwd вызова внутри workspace,
`ToolResult` или module error. Policy/approval/safety принадлежат общему tool path.
Remote Environment ID отклоняется явно.
Workspace path validation сохраняет границу Proteus: относительные пути и
абсолютные пути внутри выбранного working directory, без parent traversal
и symlink-компонентов. Абсолютный путь сначала переводится в относительный
через компонентный `strip_prefix`, поэтому aliases одинаково участвуют в
проверках duplicate target и self-move до записей. `workdir` выбирается внутри
workspace; при выборе подкаталога разрешённая граница остаётся этим
подкаталогом, а не расширяется обратно до корня workspace. Это ограничение
локальной сборки, а не полная filesystem semantics Codex.
Полный upstream applied delta, diff-progress events, длительность/exit-code
обёртки tool output и optional PreserveLineEndings не представлены этим срезом.
Proxy-профили сохраняют function tool surface; freeform transport настраивается
отдельно. Синтаксис module задаётся profile instructions, Core его не выбирает.

## Защита От Переноса На Тот Же Путь

`module_config.tool.codex_patch.reject_self_move = true` — явно выбранное отличие:
verification отклоняет `Move to`, если нормализованные source и destination
совпадают. Весь patch проверяется до первой записи, поэтому такой отказ
сохраняет исходные файлы и не применяет предыдущие hunks.

В поставляемых рабочих Codex fragments защита включена. Значение по умолчанию
для самого export — `false`: этот режим повторяет закреплённый upstream,
который сначала пишет destination, затем удаляет source и при self-move
удаляет обновлённый файл. Для сравнения точной семантики явно отключайте флаг.
Остальные parser, replacement и failure paths от настройки не меняются.
