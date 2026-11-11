#!/usr/bin/env python3
"""Install the pinned OpenCode adapters into an existing Proteus prefix."""
import argparse
from pathlib import Path
import shutil
import subprocess


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
    shared = runtime / "components/module-runtime-js"
    shared.mkdir(parents=True, exist_ok=True)
    for path in (source.parent.parent / "components/module-runtime-js").glob("*.mjs"):
        shutil.copy2(path, shared / path.name)
    binary = args.prefix.resolve() / "bin/proteus-opencode-module"
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
root = Path(__file__).resolve().parent.parent / "share/opencode-runtime/workflow/opencode-runtime"
os.execv(str(root / "node_modules/node-linux-x64/bin/node"), ["node", str(root / "worker.mjs")])
''')
    binary.chmod(0o755)
    print(binary)


if __name__ == "__main__":
    main()
