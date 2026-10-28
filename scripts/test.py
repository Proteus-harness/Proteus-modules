#!/usr/bin/env python3
"""Test implementations against their pinned harness dependency."""
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent


def main(args):
    if not args or args == ["--help"]:
        print("Usage: scripts/test.py full [Cargo options]\n       scripts/test.py -p PACKAGE [Cargo options]")
        return 0 if args else 2
    full = args[0] == "full"
    if full:
        args = ["--workspace", "--no-fail-fast", *args[1:]]
    env = os.environ.copy()
    env.setdefault("CARGO_BUILD_JOBS", "2")
    env.setdefault("RUST_TEST_THREADS", "4")
    env.setdefault("PYTHONDONTWRITEBYTECODE", "1")
    for key in ("NO_PROXY", "no_proxy"):
        values = [value for value in env.get(key, "").split(",") if value]
        env[key] = ",".join(dict.fromkeys([*values, "localhost", "127.0.0.1", "::1"]))
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version=1"],
        cwd=ROOT, env=env, text=True,
    ))
    package = next(package for package in metadata["packages"] if package["name"] == "proteus-core")
    harness = Path(package["manifest_path"]).parents[2]
    env["PROTEUS_TEST_HARNESS_ROOT"] = str(harness)
    if full or "proteus-reference-module" in args:
        env["PROTEUS_HOOK_SDK_ROOT"] = str(harness / "examples/modules/hook-process")
        hook_tests = sorted((ROOT / "hook/claude-command-hooks/tests").glob("*.test.mjs"))
        subprocess.run(["node", "--test", *map(str, hook_tests)], cwd=ROOT, env=env, check=True)
    return subprocess.call(["cargo", "test", "--locked", *args], cwd=ROOT, env=env)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
