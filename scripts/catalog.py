#!/usr/bin/env python3
"""Generate the Git source catalog from the independent package manifests."""
import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import package


CATEGORIES = {
    "model": "Модели",
    "workflow": "Workflow",
    "context": "Контекст",
    "policy": "Разрешения",
    "hook": "Hooks",
    "tool": "Инструменты",
    "context_provider": "Поиск и память",
}

# Human-facing copy only. Versions, paths, selection and exports come from
# the same manifests used by package.py; new packages work without this map.
DESCRIPTIONS = {
    "code-mode": "JavaScript-сценарии с вызовами инструментов и коротким итогом для модели.",
    "codex-context-preparation": "Подготовка и сжатие истории по правилам Codex.",
    "codex-patch": "Применение изменений файлов в формате Codex apply_patch.",
    "coding-workflow": "Циклы выполнения задач, планирования и проверки кода.",
    "context-pack": "Построение контекста задачи и рабочей области.",
    "direct-patch": "Применение прямых изменений файлов.",
    "file-tools": "Чтение, запись и просмотр файлов рабочей области.",
    "git-tools": "Инструменты работы с Git-репозиторием.",
    "hook-pack": "Инструкции и ограничение объёма результатов инструментов.",
    "memory-pack": "Память в JSONL с инструментами и поставщиком контекста.",
    "model-pack": "Провайдеры моделей и авторизация.",
    "plan-tool": "План задачи и состояние его шагов.",
    "policy-pack": "Политики разрешений и подтверждений инструментов.",
    "rg-search": "Поиск по рабочей области через ripgrep.",
    "rust-lsp": "Инструменты работы с Rust language server.",
    "shell-tool": "Запуск команд и интерактивный терминал.",
    "skill-pack": "Обнаружение и чтение навыков агента.",
    "sqlite-memory": "Память в SQLite с инструментами и поставщиком контекста.",
    "tool-selection-hook": "Динамический отбор доступных инструментов.",
}


def exported_names(exports):
    if not isinstance(exports, dict) or not exports:
        raise ValueError("У пакета нет exports")
    names = []
    for slot, identities in sorted(exports.items()):
        if (not isinstance(slot, str) or not slot or not isinstance(identities, list)
                or not identities or not all(isinstance(identity, str) and identity for identity in identities)):
            raise ValueError("Неверное описание exports")
        names.extend(f"{slot}/{identity}" for identity in sorted(identities))
    return names


def catalog(root=ROOT):
    root = Path(root).resolve()
    entries = []
    categories = set()
    for identity, selected in sorted(package.packages(root).items()):
        slot = selected.implementation.relative_to(root).parts[0]
        category = slot.replace("_", "-")
        categories.add((category, CATEGORIES.get(slot, slot)))
        exports = exported_names(selected.exports)
        description = DESCRIPTIONS.get(identity, "Реализация процессного модуля Proteus.")
        entries.append({
            "id": identity,
            "kind": "process_module",
            "title": identity,
            "description": description + " Exports: " + ", ".join(exports) + ".",
            "category": category,
            "version": selected.version,
            "path": selected.manifest.parent.relative_to(root).as_posix(),
            "source": {
                "path": ".",
                "build": ["python3", "package.py", "--package", identity],
                "artifact": f"dist/{identity}.zip",
                "requirements": ["Rust", "Git", "Python 3.11+", "LLD (Linux x86_64)",
                                 *selected.requirements],
            },
        })
    return {
        "format": 1,
        "title": "Proteus Modules",
        "categories": [{"id": identity, "title": title} for identity, title in sorted(categories)],
        "entries": entries,
    }


def main(argv=None, *, root=ROOT):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true", help="update proteus-catalog.json without building")
    mode.add_argument("--check", action="store_true", help="check that the committed catalog is current")
    args = parser.parse_args(argv)
    value = catalog(root)
    destination = Path(root) / "proteus-catalog.json"
    if args.write:
        package.atomic_json(value, destination)
        print(f"{destination}: {len(value['entries'])} модулей")
        return 0
    expected = json.dumps(value, ensure_ascii=False, indent=2) + "\n"
    if not destination.is_file() or destination.read_text() != expected:
        print("Каталог устарел; выполните python3 scripts/catalog.py --write", file=sys.stderr)
        return 1
    print(f"Каталог актуален: {len(value['entries'])} модулей")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
