# Native Hermes: проверка границ подключения

Это исполняемый integration probe, **не установленный Hermes-профиль** и не
реализация `workflow/v2`. Он проверяет, почему недостаточно обернуть `AIAgent`
обычными callbacks или заменить только его основной model client и `_invoke_tool`.

Закреплён upstream Hermes `v0.23.1`, commit
`3b421578588243f920886e6fe79a6faa87ea026e`. SHA256 входных исходников находятся
в [SOURCES.json](SOURCES.json). Чужой commit или изменённый файл отклоняются
**до исполнения** выбранных функций.

## Что действительно исполняется

Probe компилирует выбранные AST-узлы прямо из проверенного checkout. Их тела,
ветвления и номера строк не переписываются. Вместо импорта всего `AIAgent`
подключаются только необходимые функции; внешние зависимости заменены явными
детерминированными sentinels. Native `TodoStore` и `todo_tool` выполняются
настоящие, с состоянием только в памяти. Конструктор, весь conversation loop,
реальные провайдеры и инструменты с внешними эффектами не запускаются.

| Проверка | Наблюдаемое поведение закреплённого upstream |
|---|---|
| Только override `_invoke_tool` | Sequential `todo_list` меняет native TodoStore, не вызывая override: dispatch имеет отдельный inline path. |
| Только execution middleware | Обычное исключение в callback до `next_call` регистрируется как ошибка plugin; native цепочка продолжает terminal executor. Положительный контроль: явный terminal error result останавливает эффект. |
| Только основной client | `_call_summary_llm` обращается к отдельному `call_llm(task="compression")`; заменённого primary client недостаточно. |
| Native persistence success | При отсутствии SessionDB native flush возвращает `None`, а progress wrapper принимает это как успех. Это не Core checkpoint и не durable ACK. |

Это не баги standalone Hermes: его plugin middleware намеренно fail-open,
а запуск без SessionDB поддерживается. Эти свойства нельзя использовать как
доказательство Core admission или сохранения canonical history.

## Запуск без inference

Нужен локальный source checkout; проверен Python 3.14.8. Дополнительные Python
packages не требуются. Получение исходников требует сети; сам probe сеть не
использует.

```sh
git clone --filter=blob:none --no-checkout https://github.com/NousResearch/hermes-agent.git /tmp/hermes-source
git -C /tmp/hermes-source checkout --detach 3b421578588243f920886e6fe79a6faa87ea026e
python3 -B research/hermes-native-bridge/probe.py \
  --upstream /tmp/hermes-source \
  --output target/research/hermes-native-bridge/receipt.json
HERMES_UPSTREAM_SOURCE=/tmp/hermes-source \
  python3 -B -m unittest discover -s research/hermes-native-bridge/tests -v
```

Результат `counterexamples_reproduced` означает четыре воспроизведённых
границы неполного адаптера. Он не является PASS всего Hermes runtime/parity.
Платных model requests — ноль.

## Что нужно для настоящего профиля

Существующие общие `workflow/v2`, `tool/v6`, model и checkpoint capabilities
Proteus позволяют выразить нужные операции. Probe не обосновывает новый
общий contract или исключение для имени `hermes` в Core.

Нужна отдельная реализация native bridge:

1. Все основные, auxiliary, fallback и delegated model calls направляются в
   выбранные Core model routes с canonical usage/errors/cancellation.
2. Все native tool paths, включая sequential inline, concurrent, memory/context
   engine и delegate, проходят `host.tools.execute`. Ошибка bridge не должна
   продолжать native executor мимо Core admission.
3. История, привязки tool calls/results и compaction проходят настоящий Core
   checkpoint. Перед продолжением требуется его non-null durable revision;
   native SessionDB boolean/`None` не заменяет ACK.
4. Pinned native prompt/toolset selection сохраняются, внешние плагины и
   optional integrations выбираются явно. Нельзя назвать generic loop с
   Hermes prompt строгим Hermes-профилем.

Перед установкой `hermes.config.toml` нужны реальные component/Core проверки
tool denial, отказа/потери ACK, auxiliary summary, abort, partial model failure,
overflow и cold continuation без повторения принятых effects. В этом probe
эти интеграционные гарантии не реализованы и не заявляются.
