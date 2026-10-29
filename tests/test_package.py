"""Build scope, recorded Cargo outputs and independent package publication."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("module_package", Path(__file__).parents[1] / "package.py")
package = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = package
spec.loader.exec_module(package)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="independent module tests ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "LICENSE").write_text("synthetic license")
        for identity in ("one", "two"):
            folder = self.root / "components" / identity
            folder.mkdir(parents=True)
            (folder / "exports.json").write_text(json.dumps({"tool": [identity]}))
            (folder / "Cargo.toml").write_text(
                f'[package]\nname = "proteus-{identity}-module"\nversion = "1.0.0"\n'
                f'[package.metadata.proteus]\nid = "{identity}"\nimplementation = "tool/{identity}"\n')
            (self.root / "tool" / identity).mkdir(parents=True)
        self.packages = package.packages(self.root)
        self.outputs = {}
        for identity, item in self.packages.items():
            binary = self.root / "external cache/a-target/release" / item.binary
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_bytes(f"new {identity}".encode())
            binary.chmod(0o755)
            self.outputs[identity] = binary

    def build(self, identities, status=0, records=None):
        selected = [self.packages[identity] for identity in identities]
        if records is None:
            records = [{"reason": "compiler-artifact", "manifest_path": str(item.manifest),
                        "target": {"name": item.binary, "kind": ["bin"]}, "profile": {"test": False},
                        "executable": str(self.outputs[item.identity])} for item in selected]
            records.append({"reason": "build-finished", "success": True})
        result = subprocess.CompletedProcess(["cargo"], status, "\n".join(map(json.dumps, records)))
        with patch.object(package.subprocess, "run", return_value=result) as run:
            outputs = package.build(selected, "release", root=self.root)
        return outputs, run.call_args

    def test_one_package_build_uses_exact_artifact_and_no_other_build_roots(self):
        outputs, call = self.build(["one"])
        self.assertEqual(outputs, {"one": self.outputs["one"]})
        self.assertEqual(call.args[0], ["cargo", "build", "--locked", "--profile", "release",
            "--message-format=json-render-diagnostics", "-p", "proteus-one-module",
            "--bin", "proteus-one-module"])
        self.assertEqual(call.kwargs["cwd"], self.root)

    def test_complete_build_is_one_invocation_and_preserves_unselected_index_entries(self):
        outputs, call = self.build(["one", "two"])
        self.assertEqual(outputs, self.outputs)
        self.assertIn("--bins", call.args[0])
        before = json.loads(package.index_path("release", self.root).read_text())["packages"]["two"]
        self.build(["one"])
        after = json.loads(package.index_path("release", self.root).read_text())
        self.assertEqual(after["packages"]["two"], before)
        self.assertEqual(after["last_build"]["selected"], ["one"])

    def test_failed_build_never_replaces_recorded_outputs(self):
        self.build(["one"])
        path = package.index_path("release", self.root)
        before = path.read_bytes()
        with self.assertRaises(subprocess.CalledProcessError):
            self.build(["one"], status=1)
        self.assertEqual(path.read_bytes(), before)

    def test_missing_duplicate_wrong_or_incomplete_artifacts_are_rejected(self):
        item = self.packages["one"]
        event = {"reason":"compiler-artifact", "manifest_path": str(item.manifest),
                 "target":{"name":item.binary,"kind":["bin"]}, "profile":{"test":False},
                 "executable":str(self.outputs["one"])}
        finished = {"reason":"build-finished","success":True}
        for records in ([finished], [event, event, finished], [event],
                        [dict(event, manifest_path=str(self.packages["two"].manifest)), finished],
                        [event, dict(finished, success=False)], [event, dict(finished, success=1)],
                        [dict(event, executable=str(self.root / "missing")), finished]):
            with self.subTest(records=records), self.assertRaises(SystemExit):
                self.build(["one"], records=records)
        self.assertFalse(package.index_path("release", self.root).exists())

    def test_reuse_uses_recorded_external_path_without_cargo(self):
        self.build(["one"])
        with patch.object(package.subprocess, "run") as run:
            self.assertEqual(package.reuse([self.packages["one"]], "release", self.root),
                             {"one": self.outputs["one"]})
            with self.assertRaises(SystemExit):
                package.reuse([self.packages["two"]], "release", self.root)
        run.assert_not_called()

    def test_install_updates_only_selected_binary_and_its_metadata(self):
        destination = self.root / "runtime"
        (destination / "bin").mkdir(parents=True)
        other = destination / "bin/proteus-two-module"
        other.write_bytes(b"other module")
        package.install(self.packages["one"], self.outputs["one"], destination, self.root)
        self.assertEqual(other.read_bytes(), b"other module")
        self.assertEqual((destination / "bin/proteus-one-module").read_bytes(), b"new one")
        manifest = json.loads((destination / "share/modules/one/module.json").read_text())
        self.assertEqual(manifest["exports"], {"tool": ["one"]})
        self.assertEqual(manifest["command"], ["bin/proteus-one-module"])

    def test_zip_contains_one_executable_with_licenses_and_metadata(self):
        archive = package.archive(self.packages["one"], self.outputs["one"], self.root / "dist", self.root)
        with zipfile.ZipFile(archive) as zipped:
            self.assertIsNone(zipped.testzip())
            self.assertEqual(zipped.read("bin/proteus-one-module"), b"new one")
            self.assertEqual(zipped.getinfo("bin/proteus-one-module").external_attr >> 16 & 0o111, 0o111)
            self.assertIn("share/modules/one/licenses/LICENSE", zipped.namelist())
            self.assertFalse(any("two" in name for name in zipped.namelist()))

    def test_reuse_rejects_changed_bytes_metadata_and_missing_digest(self):
        self.build(["one"])
        index_path = package.index_path("release", self.root)
        index = json.loads(index_path.read_text())
        for field, value in (("sha256", None), ("version", "2.0.0"), ("exports", {})):
            with self.subTest(field=field):
                changed = json.loads(json.dumps(index))
                changed["packages"]["one"][field] = value
                index_path.write_text(json.dumps(changed))
                with self.assertRaises(SystemExit):
                    package.reuse([self.packages["one"]], "release", self.root)
        index_path.write_text(json.dumps(index))
        self.outputs["one"].write_bytes(b"changed artifact")
        with self.assertRaises(SystemExit):
            package.reuse([self.packages["one"]], "release", self.root)

    def snapshot(self, destination):
        return {str(path.relative_to(destination)): (path.read_bytes(), path.stat().st_mode)
                for path in destination.rglob("*") if path.is_file()}

    def test_staging_and_each_publish_failure_preserve_entire_selected_package(self):
        destination = self.root / "runtime"
        package.install(self.packages["one"], self.outputs["one"], destination, self.root)
        package.install(self.packages["two"], self.outputs["two"], destination, self.root)
        previous = self.snapshot(destination)
        self.outputs["one"].write_bytes(b"updated one")
        (self.root / "LICENSE").write_text("updated license")
        real_copy = package.shutil.copy2
        real_replace = package.os.replace
        for phase, count in (("stage", 1), ("stage", 2), ("backup", 2),
                             ("publish", 1), ("publish", 2), ("publish", 3)):
            calls = 0
            def copy(source, target):
                nonlocal calls
                selected = ((phase == "stage" and ".stage-" in str(target)) or
                            (phase == "backup" and ".rollback-" in str(target)))
                if selected:
                    calls += 1
                    if calls == count:
                        Path(target).write_bytes(b"partial")
                        raise OSError("injected copy failure")
                return real_copy(source, target)
            def publish(source, target):
                nonlocal calls
                if phase == "publish" and ".stage-" in str(source):
                    calls += 1
                    if calls == count:
                        raise OSError("injected publication failure")
                return real_replace(source, target)
            with self.subTest(phase=phase, count=count), \
                 patch.object(package.shutil, "copy2", side_effect=copy), \
                 patch.object(package.os, "replace", side_effect=publish), self.assertRaises(OSError):
                package.install(self.packages["one"], self.outputs["one"], destination, self.root)
            self.assertEqual(self.snapshot(destination), previous)

    def test_failed_first_install_removes_new_outputs(self):
        destination = self.root / "runtime"
        real_replace = package.os.replace
        def fail_metadata(source, target):
            if Path(target).name == "module.json":
                raise OSError("injected metadata failure")
            return real_replace(source, target)
        with patch.object(package.os, "replace", side_effect=fail_metadata), self.assertRaises(OSError):
            package.install(self.packages["one"], self.outputs["one"], destination, self.root)
        self.assertEqual(self.snapshot(destination), {})

    def test_failed_restore_retains_previous_binary_with_diagnostic(self):
        destination = self.root / "runtime"
        package.install(self.packages["one"], self.outputs["one"], destination, self.root)
        self.outputs["one"].write_bytes(b"updated one")
        real_replace = package.os.replace
        def fail(source, target):
            source, target = Path(source), Path(target)
            if target.name == "module.json" and ".stage-" in source.name:
                raise OSError("injected publication failure")
            if target.name == "proteus-one-module" and ".rollback-" in source.name:
                raise OSError("injected restore failure")
            return real_replace(source, target)
        with patch.object(package.os, "replace", side_effect=fail), self.assertRaises(OSError) as caught:
            package.install(self.packages["one"], self.outputs["one"], destination, self.root)
        backups = list(destination.rglob("*.rollback-*"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), b"new one")
        self.assertIn(str(backups[0]), "\n".join(caught.exception.__notes__))
        self.assertFalse(list(destination.rglob("*.stage-*")))

    def test_digest_checks_staged_bytes_before_install_or_zip_publication(self):
        destination = self.root / "runtime"
        package.install(self.packages["one"], self.outputs["one"], destination, self.root)
        previous = self.snapshot(destination)
        digest = package.file_sha256(self.outputs["one"])
        real_copy = package.shutil.copy2
        def mutate_copy(source, target):
            result = real_copy(source, target)
            if Path(source) == self.outputs["one"]:
                Path(target).write_bytes(b"artifact changed during copy")
            return result
        with patch.object(package.shutil, "copy2", side_effect=mutate_copy):
            with self.assertRaises(SystemExit):
                package.install(self.packages["one"], self.outputs["one"], destination, self.root,
                                expected_sha256=digest)
            with self.assertRaises(SystemExit):
                package.archive(self.packages["one"], self.outputs["one"], self.root / "zip", self.root,
                                expected_sha256=digest)
        self.assertEqual(self.snapshot(destination), previous)
        self.assertFalse((self.root / "zip/one.zip").exists())

    def test_invalid_selection_fails_before_cargo(self):
        with patch.object(package, "packages", return_value=self.packages), \
             patch.object(package, "build") as build:
            for args in (["--package", "missing"], ["--package", "one", "--package", "one"],
                         ["--install", str(self.root)]):
                with self.subTest(args=args), self.assertRaises(SystemExit):
                    package.main(args)
            build.assert_not_called()


if __name__ == "__main__":
    unittest.main()
