#!/usr/bin/env python3
"""Install the pinned Node component into an existing Proteus prefix."""
import argparse
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", required=True, type=Path)
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    modules = source.parents[1]
    subprocess.run(["npm", "ci", "--ignore-scripts", "--omit=dev", "--no-audit", "--no-fund"], cwd=source, check=True)
    destination = args.prefix.resolve() / "share/pi-runtime"
    tools = destination / "tool/pi-tools"
    tools.mkdir(parents=True, exist_ok=True)
    for name in ["adapter.mjs", "worker.mjs", "package.json", "package-lock.json", "README.md", "NOTICE", "LICENSE.upstream"]:
        shutil.copy2(source / name, tools / name)
    shutil.copytree(source / "node_modules", tools / "node_modules", dirs_exist_ok=True)
    for relative in ["workflow/pi-runtime", "components/module-runtime-js", "components/pi-runtime-js", "model/pi-provider"]:
        source_dir = modules / relative
        target = destination / relative
        target.mkdir(parents=True, exist_ok=True)
        for path in source_dir.glob("*.mjs"):
            shutil.copy2(path, target / path.name)
    launcher = args.prefix.resolve() / "bin/proteus-pi-module"
    launcher.parent.mkdir(parents=True, exist_ok=True)
    launcher.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
worker = Path(__file__).resolve().parent.parent / "share/pi-runtime/tool/pi-tools/worker.mjs"
os.execvp("node", ["node", str(worker)])
''')
    launcher.chmod(0o755)
    print(launcher)


if __name__ == "__main__":
    main()
