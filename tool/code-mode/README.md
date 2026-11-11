# CodeMode

Самостоятельный процессный пакет `code-mode` экспортирует `tool/code_mode`.
Один вызов модели выполняет JavaScript-программу с циклами, проверками,
`await` и `Promise.all`. Модель получает только явно напечатанный вывод;
результаты дочерних инструментов остаются в canonical journal Core.
Само количество обращений к инструментам при этом не уменьшается.

## Подключение

```bash
python3 package.py --package code-mode --install /absolute/path/to/Proteus
```

Выберите поставляемый opt-in профиль
[`code-mode.config.toml`](https://github.com/Proteus-harness/Proteus-core/blob/main/configs/code-mode.config.toml)
и перезапустите backend. Профиль сохраняет обычные описания инструментов,
чтобы модель знала их имена и аргументы. CodeMode не заменяет workflow,
policy, approval или MCP adapter. Core должен поддерживать invocation-local
scheduling вложенных callbacks; оно сохраняет shared/exclusive разрешения
каждого дочернего инструмента, а не только внешнего `code_mode`.

## JavaScript API

```javascript
const results = await Promise.all(
    ['README.md', 'Cargo.toml'].map(path => tools.call('read_file', {path}))
);
for (const result of results) {
    if (!result.ok) throw Error(result.error);
}
text({files: results.length, bytes: results.reduce((n, r) => n + r.output.length, 0)});
```

`tools.call(name, args)` возвращает Promise полного `ToolResult`: `ok`,
`output`, `content`, `error`, `metadata`, `call_id`. `ok: false`, включая отказ
policy/approval, остаётся значением: программа должна проверить `.ok` и решить,
продолжать ли работу. Все такие отказы учитываются в `failed_tool_calls`, даже
если программа их игнорирует. Ошибка транспорта/host callback завершает весь
скрипт ошибкой, включая случай, когда JavaScript её перехватил.

Аргументы передаются в форме зарегистрированного инструмента. Например,
`apply_patch` принимает объект с полем `patch`:

```javascript
const result = await tools.call('apply_patch', {
    patch: '*** Begin Patch\n...\n*** End Patch'
});
if (!result.ok) throw Error(result.error);
text('patch applied');
```

API первой версии принимает ровно два аргумента и создаёт обычный canonical
function call. Третий аргумент отклоняется, включая `undefined`.
Точные имена и схемы берите из доступных модели описаний инструментов.
Workflow-owned операции `proteus_tool_search`/`proteus_tool_describe` не являются
registry tools и из скрипта недоступны; при dynamic selection обнаружьте
инструмент обычным вызовом до `code_mode`.

`text(value)` и `console.log(...values)` добавляют строку в общий вывод:
строки без JSON-кавычек, другие значения как JSON. Возвращаемое значение
программы автоматически не печатается. Global state между вызовами отсутствует.
Нужно `await` каждой делегированной операции; фоновых заданий и detached
Promises нет. Неразрешимый Promise без инструментов и JS jobs отклоняется сразу.
Вызов `code_mode` из него самого запрещён.

## Бюджеты

Все поля config необязательны; неизвестное поле или значение вне диапазона
отклоняется. `timeout_ms` также можно передать в аргументах вызова, уменьшив
настроенный бюджет.

| Поле config | По умолчанию | Допустимый диапазон |
|---|---:|---:|
| `timeout_ms` | 30 000 мс | 1–60 000 мс |
| `memory_limit_bytes` | 32 MiB | 1–128 MiB |
| `max_calls` | 256 | 1–1024 |
| `max_concurrency` | 4 | 1–16 |
| `max_output_bytes` | 32 KiB | 1 байт–1 MiB |

Фиксированные бюджеты: исходник до 64 KiB UTF-8, стек QuickJS до 512 KiB,
аргументы одного child call до 64 KiB, аргументы всех child calls суммарно
до 8 MiB, ответы child calls суммарно до 8 MiB serialized JSON.
Переполнение вывода завершает скрипт ошибкой, сохраняя допустимый UTF-8 префикс.
Ошибка JavaScript, отмена и timeout сохраняют уже напечатанный вывод.
Metadata содержит requested/attempted/completed/failed counts, host errors,
признак усечения и длительность, включая завершение запущенных callbacks.

Движок QuickJS встроен в бинарник: Node не нужен. Он не регистрирует `fs`,
`fetch`, `process`, `require`, `Deno`, `Bun`, timers или module loader.
Доступ к ОС возможен только через явно вызванные инструменты с обычными
проверками Core. Это граница JavaScript API, а не OS sandbox доверенного
worker-процесса. Бюджет heap не включает Rust bridge, другие процессы и журнал.

Deadline и отмена прерывают даже бесконечный JS-цикл. Они прекращают новые
host calls, но не отзывают уже начавшийся side effect. Очередь не начатых
callbacks отбрасывается; начатые callbacks завершаются до возврата результата
и сохраняют родительские cancellation/deadline Core. Поэтому длительность
вызова может превышать выбранный script timeout, если дочерний инструмент ещё
завершает работу. Внешний ToolSpec ограничен 65 секундами.

## Проверка

```bash
python3 scripts/test.py -p code-mode --lib
```

Тесты проверяют фильтрацию и короткий вывод, реальное ограниченное параллельное
исполнение, failures и identity ответов, budgets, CPU/memory/cancellation,
отсутствие OS globals, fresh state и завершение pending callbacks.
Численные сокращения токенов и latency требуют отдельного сопоставимого
прогона с моделью; библиотечные тесты такого выигрыша не доказывают.
