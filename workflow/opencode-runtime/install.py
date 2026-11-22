#!/usr/bin/env python3
"""Install the pinned OpenCode adapters into an existing Proteus prefix."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys


def write_module_manifest(prefix, source=None):
    """Describe creation options without npm, network or runtime startup."""
    source = source or Path(__file__).resolve().parent
    modules = source.parent.parent
    sys.path.insert(0, str(modules))
    from package import atomic_json, model_connections
    prefix = prefix.resolve()
    binary = prefix / "bin/proteus-opencode-module"
    exports = json.loads((source / "module-exports.json").read_text())
    version = json.loads((source / "package.json").read_text())["version"]
    with binary.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    manifest = {"id": "opencode-runtime", "version": version,
                "command": ["bin/proteus-opencode-module"], "exports": exports,
                "sha256": digest,
                "model_connections": model_connections(source / "model-connections.json", exports)}
    destination = prefix / "share/modules/opencode-runtime/module.json"
    atomic_json(manifest, destination)
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, required=True)
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    subprocess.run(["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund"], cwd=source, check=True)
    runtime = args.prefix.resolve() / "share/opencode-runtime"
    destination = runtime / "workflow/opencode-runtime"
    destination.mkdir(parents=True, exist_ok=True)
    for filename in ["package.json", "package-lock.json", "NOTICE", "LICENSE.upstream", "README.md", *[path.name for path in source.glob("*.mjs")]]:
        shutil.copy2(source / filename, destination / filename)
    shutil.copytree(source / "node_modules", destination / "node_modules", dirs_exist_ok=True)
    modules = source.parent.parent
    for relative in ["components/module-runtime-js", "components/model-runtime-js", "model/opencode-provider"]:
        source_dir = modules / relative
        for path in source_dir.rglob("*.mjs"):
            name = path.relative_to(source_dir)
            if "tests" in name.parts:
                continue
            target = runtime / relative / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
    seed_relative = Path("model/opencode-provider/catalog-seeds")
    seeds = runtime / seed_relative
    seeds.mkdir(parents=True, exist_ok=True)
    for provider in ("opencode-go", "opencode"):
        filename = f"{provider}.a177c5468699a3ce48b27bc417847a0804af1564.json"
        shutil.copy2(modules / seed_relative / filename, seeds / filename)
    binary = args.prefix.resolve() / "bin/proteus-opencode-module"
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
root = Path(__file__).resolve().parent.parent / "share/opencode-runtime/workflow/opencode-runtime"
os.execv(str(root / "node_modules/node-linux-x64/bin/node"), ["node", str(root / "worker.mjs")])
''')
    binary.chmod(0o755)
    write_module_manifest(args.prefix, source)
    print(binary)


if __name__ == "__main__":
    main()
