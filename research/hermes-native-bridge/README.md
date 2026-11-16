# Native Hermes: проверка границ подключения

Это исполняемый integration probe, **не установленный Hermes-профиль** и не
реализация `workflow/v20`. Он проверяет, почему недостаточно обернуть `AIAgent`
обычными callbacks или заменить только его основной model client и `_invoke_tool`.

Закреплён upstream Hermes `v0.23.1`, commit
`3b421578588243f920886e6fe79a6faa87ea026e`. SHA256 входных исходников находятся
в [SOURCES.json](SOURCES.json). Чужой commit или изменённый файл отклоняются
**до исполнения** выбранных функций.

## Что действительно исполняется

`probe.py` компилирует выбранные AST-узлы прямо из проверенного checkout. Их тела,
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

## Настоящий native constructor и toolset smoke

Отдельный `native_smoke.py` действительно импортирует неизменённый `AIAgent`
и вызывает его полный публичный конструктор. Native dependencies готовятся
через закреплённый `uv.lock`, без optional extras и установки проекта как wheel.
В проверенном окружении установлены 65 packages. Это отдельная venv, не пакеты
основного пользователя. Никаких замен native functions, subclass или plugins
для smoke не используется.

```sh
UV_PROJECT_ENVIRONMENT=/absolute/hermes-dependencies \
  uv sync --frozen --no-dev --no-install-project --project /tmp/hermes-source
python3 -B research/hermes-native-bridge/native_smoke.py \
  --upstream /tmp/hermes-source \
  --python /absolute/hermes-dependencies/bin/python \
  --state target/research/hermes-native-bridge/fresh-state \
  --output target/research/hermes-native-bridge/native-smoke.json
```

Требуется Linux/bubblewrap. `--state` должен быть новым каталогом. Child process
получает read-only root, только этот writable state, очищенный environment и
`--unshare-net`. SDK credential намеренно нерабочий. Личные Hermes home/settings
не используются. PM lazy installs выключены только для изолированной проверки
уже подготовленного окружения. Исходники должны быть полностью чистыми на pin.

Подтверждено для source environment с toolset `hermes-cli`: настоящий
`ContextCompressor`, 19 model-visible native tools, включая `memory`,
`skill_manage`, `delegate_task`, `terminal`, file tools и tool-search bridge.
SDK создан с fixture model `gpt-4o-mini` и localhost endpoint: это не выбор
пользовательской модели и не inference с этой моделью.
Native memory store создаётся, memory flags включены, `skip_background_review`
остаётся `False`. Реальный `load_background_review_settings()` возвращает `True`;
реальный memory-nudge counter даёт `False` девять раз и `True` на десятом шаге.

Это проверка конструктора/выбора tools/defaults: `run_conversation`, model
inference, native tool invocations и сам background fork **не запускались**.
Наличие tool definition не доказывает выполнение всех optional integrations.

## Vanilla background review — отдельная lifecycle граница

Background review не является выключенной по умолчанию integration:

- [`config_defaults.py:795`](https://github.com/NousResearch/hermes-agent/blob/3b421578588243f920886e6fe79a6faa87ea026e/hermes_cli/config_defaults.py#L795)
  задаёт `auxiliary.background_review.enabled = True`; memory/user-profile flags
  включены, memory nudge interval равен 10 (строки 1296–1306).
- [`turn_context.py:819`](https://github.com/NousResearch/hermes-agent/blob/3b421578588243f920886e6fe79a6faa87ea026e/agent/turn_context.py#L819)
  с доступным native memory tool/store поднимает trigger каждые десять user
  turns. Skill-review trigger проверяет interval в 10 tool iterations и наличие
  `skill_manage` (`turn_finalizer.py:863`, `agent_init.py:1236`).
- [`background_review.py:228`](https://github.com/NousResearch/hermes-agent/blob/3b421578588243f920886e6fe79a6faa87ea026e/agent/background_review.py#L228)
  читает настройку с default `True`; safe-worker и finite gateway turn выключают
  automatic review. Это явные режимы, а не vanilla interactive default.
- [`turn_finalizer.py:591`](https://github.com/NousResearch/hermes-agent/blob/3b421578588243f920886e6fe79a6faa87ea026e/agent/turn_finalizer.py#L591)
  планирует review после доставки ответа; `run_agent.py:827` запускает daemon
  thread с отдельным native `AIAgent`. Его whitelist допускает mutations
  `memory`/`skill_manage` (`background_review.py:1116`), когда включены их triggers.

Foreground `workflow/v20`, `tool/v6`, model и checkpoint capabilities Proteus
позволяют выразить foreground Hermes операции. Неполный adapter — не
доказательство невыразимости этих contracts.

Но continuation model/tools **после terminal родительского workflow** нужен
Core-owned background task lifecycle: собственные task/execution identity,
admission и authority, durable state/result delivery, cancellation и shutdown
ownership. Удержание старого `InvocationContext` в native daemon не создаёт
этих прав. Read-only async collaboration не заменяет writer lifecycle для
memory/skills maintenance. Это конкретная граница строгого vanilla профиля,
отдельная от ещё не написанного foreground adapter.

Ожидание daemon до выдачи terminal меняет native порядок/latency. Отключение
background review публичной опцией допустимо только как явно выбранный другой
setup; выдавать его за полную vanilla reference-сборку нельзя.

## Что нужно для настоящего профиля

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
5. Native post-turn background review получает явного Core-owned владельца
   вместо продления invocation authority или скрытого daemon bypass.

Перед установкой `hermes.config.toml` нужны реальные component/Core проверки
tool denial, отказа/потери ACK, auxiliary summary, abort, partial model failure,
overflow и cold continuation без повторения принятых effects. В этом probe
эти интеграционные гарантии не реализованы и не заявляются. До решения
background lifecycle строгий `hermes.config.toml` не устанавливается.
