#!/usr/bin/env python3
"""Build and deliver independent backend processes, without Core or Desktop."""
import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent


@dataclass(frozen=True)
class Package:
    identity: str
    binary: str
    version: str
    manifest: Path
    implementation: Path
    exports: dict


def packages(root=ROOT):
    found = {}
    for exports in sorted((root / "components").glob("*/exports.json")):
        manifest = exports.with_name("Cargo.toml")
        spec = tomllib.loads(manifest.read_text())["package"]
        metadata = spec["metadata"]["proteus"]
        identity = metadata["id"]
        if identity in found:
            raise SystemExit(f"Duplicate module package: {identity}")
        found[identity] = Package(identity, spec["name"], spec["version"], manifest.resolve(),
                                  root / metadata["implementation"], json.loads(exports.read_text()))
    if not found:
        raise SystemExit(f"No module packages in {root}")
    return found


def atomic_copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", dir=destination.parent)
    os.close(descriptor)
    try:
        shutil.copy2(source, temporary)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def atomic_json(value, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", dir=destination.parent)
    try:
        with os.fdopen(descriptor, "w") as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def index_path(profile, root=ROOT):
    return root / "dist/.artifacts" / f"{profile}.json"


def artifact_record(package, executable):
    return {"manifest": str(package.manifest), "executable": str(executable),
            "binary": package.binary, "exports": package.exports, "version": package.version}


def build(selected, profile="release", target_dir=None, env=None, root=ROOT):
    command = ["cargo", "build", "--locked", "--profile", profile,
               "--message-format=json-render-diagnostics"]
    for package in selected:
        command += ["-p", package.binary]
    command += ["--bin", selected[0].binary] if len(selected) == 1 else ["--bins"]
    if target_dir is not None:
        command += ["--target-dir", str(Path(target_dir).resolve())]
    print("+ " + " ".join(command), file=sys.stderr, flush=True)
    result = subprocess.run(command, cwd=root, env=env, stdout=subprocess.PIPE, text=True)
    manifests = {package.manifest: package for package in selected}
    outputs = {package.identity: [] for package in selected}
    finished = []
    targets = set()
    invalid = False
    for line in result.stdout.splitlines():
        try:
            record = json.loads(line)
        except ValueError:
            print(line, file=sys.stderr)
            invalid = True
            continue
        if not isinstance(record, dict):
            invalid = True
            continue
        reason = record.get("reason")
        if reason == "compiler-message":
            print(record["message"].get("rendered", ""), end="", file=sys.stderr)
        elif reason == "build-finished":
            finished.append(record.get("success"))
        elif reason == "compiler-artifact":
            target = record.get("target", {})
            targets.add(target.get("name", ""))
            manifest = record.get("manifest_path")
            if not isinstance(manifest, str):
                continue
            manifest = Path(manifest)
            package = manifests.get((root / manifest).resolve())
            if (package is not None and target.get("name") == package.binary
                    and target.get("kind") == ["bin"] and record.get("profile", {}).get("test") is False):
                executable = record.get("executable")
                if isinstance(executable, str) and executable:
                    outputs[package.identity].append((root / executable).resolve())
                else:
                    invalid = True
    if result.returncode:
        raise subprocess.CalledProcessError(result.returncode, command, output=result.stdout)
    if invalid or len(finished) != 1 or finished[0] is not True:
        raise SystemExit("Cargo did not report a successful module build")
    resolved = {}
    for identity, paths in outputs.items():
        if len(paths) != 1 or not paths[0].is_file():
            raise SystemExit(f"{identity}: expected one existing Cargo executable, received {paths}")
        resolved[identity] = paths[0]
    destination = index_path(profile, root)
    index = json.loads(destination.read_text()) if destination.exists() else {"packages": {}}
    index["last_build"] = {"selected": [package.identity for package in selected],
                           "artifact_targets": sorted(targets)}
    index["packages"].update({package.identity: artifact_record(package, resolved[package.identity])
                              for package in selected})
    atomic_json(index, destination)
    return resolved


def reuse(selected, profile="release", root=ROOT):
    """Reuse recorded actual paths; never guess a Cargo target directory."""
    path = index_path(profile, root)
    if not path.is_file():
        raise SystemExit(f"No recorded {profile} module artifacts; run package.py without --no-build")
    index = json.loads(path.read_text())["packages"]
    outputs = {}
    for package in selected:
        record = index.get(package.identity, {})
        executable = Path(record.get("executable", ""))
        if (record.get("manifest") != str(package.manifest) or record.get("binary") != package.binary
                or not executable.is_absolute() or not executable.is_file()):
            raise SystemExit(f"No reusable artifact for {package.identity}; build that package first")
        outputs[package.identity] = executable
    return outputs


def install(package, binary, destination, root=ROOT):
    """Publish one module in a runtime root; no other module or host is touched."""
    with binary.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    module = destination / "share/modules" / package.identity
    module.mkdir(parents=True, exist_ok=True)
    notices = module / "licenses"
    notices.mkdir(exist_ok=True)
    shutil.copy2(root / "LICENSE", notices / "LICENSE")
    # Preserve upstream provenance beside the exact implementation being shipped.
    for source in sorted(package.implementation.rglob("*")):
        if source.is_file() and source.name in ("LICENSE", "NOTICE", "UPSTREAM.md", "PROVENANCE.md"):
            target = notices / source.relative_to(package.implementation)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    atomic_copy(binary, destination / "bin" / package.binary)
    atomic_json({"id": package.identity, "version": package.version,
                 "command": [f"bin/{package.binary}"], "exports": package.exports,
                 "sha256": digest}, module / "module.json")


def archive(package, binary, output, root=ROOT):
    with tempfile.TemporaryDirectory(prefix="proteus-module-package-") as directory:
        stage = Path(directory) / package.identity
        install(package, binary, stage, root)
        output.mkdir(parents=True, exist_ok=True)
        destination = output / f"{package.identity}.zip"
        descriptor, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", dir=output)
        os.close(descriptor)
        try:
            with zipfile.ZipFile(temporary, "w", zipfile.ZIP_DEFLATED) as zipped:
                for path in sorted(stage.rglob("*")):
                    if path.is_file():
                        zipped.write(path, path.relative_to(stage))
            os.replace(temporary, destination)
        finally:
            Path(temporary).unlink(missing_ok=True)
        return destination


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", action="append", metavar="ID", help="select only this package (repeatable)")
    parser.add_argument("--profile", default="release", help="Cargo profile (default: release)")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--no-build", action="store_true", help="reuse recorded artifacts without Cargo; no freshness check")
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--bundle", type=Path, help="install selected packages into a runtime root")
    modes.add_argument("--install", type=Path, help="update exactly one package in an existing runtime root")
    modes.add_argument("--print-artifacts", action="store_true", help="print selected binary paths as JSON; do not package")
    modes.add_argument("--output", type=Path, help="ZIP directory (default: dist)")
    args = parser.parse_args(argv)
    available = packages()
    requested = args.package or list(available)
    if len(set(requested)) != len(requested) or any(name not in available for name in requested):
        parser.error("--package must name distinct known packages: " + ", ".join(available))
    if args.install and (not args.package or len(requested) != 1):
        parser.error("--install requires exactly one --package")
    selected = [available[name] for name in requested]
    outputs = reuse(selected, args.profile) if args.no_build else build(selected, args.profile, args.target_dir)
    if args.print_artifacts:
        print(json.dumps({identity: str(path) for identity, path in outputs.items()}))
    elif args.bundle or args.install:
        for package in selected:
            install(package, outputs[package.identity], args.bundle or args.install)
    else:
        for package in selected:
            print(archive(package, outputs[package.identity], args.output or ROOT / "dist"))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode)
