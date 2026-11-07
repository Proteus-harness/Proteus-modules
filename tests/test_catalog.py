"""The public source index follows real package metadata without a build."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("module_source_catalog", ROOT / "scripts/catalog.py")
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)


class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="module catalog tests ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def add_package(self, identity, *, slot="tool", version="1.2.3", exports=None):
        component = self.root / "components" / f"{identity}-module"
        component.mkdir(parents=True)
        (component / "Cargo.toml").write_text(
            f'[package]\nname = "proteus-{identity}-module"\nversion = "{version}"\n'
            f'[package.metadata.proteus]\nid = "{identity}"\nimplementation = "{slot}/{identity}"\n')
        (component / "exports.json").write_text(json.dumps(exports or {slot: [identity]}))
        (self.root / slot / identity).mkdir(parents=True)
        return component

    def test_catalog_contains_exactly_the_independent_delivery_packages(self):
        value = catalog.catalog()
        available = catalog.package.packages()
        self.assertTrue(available)
        self.assertEqual({entry["id"] for entry in value["entries"]}, set(available))
        self.assertNotIn("process-module", {entry["id"] for entry in value["entries"]})
        for entry in value["entries"]:
            selected = available[entry["id"]]
            with self.subTest(identity=entry["id"]):
                self.assertEqual(entry["version"], selected.version)
                self.assertTrue((ROOT / entry["path"] / "Cargo.toml").is_file())
                self.assertEqual(entry["source"]["path"], ".")
                self.assertEqual(entry["source"]["build"],
                                 ["python3", "package.py", "--package", selected.identity])
                self.assertEqual(entry["source"]["requirements"],
                                 ["Rust", "Git", "Python 3.11+", "LLD (Linux x86_64)"])
                self.assertNotIn("artifacts", entry)
                self.assertEqual(entry["source"]["artifact"], f'dist/{entry["id"]}.zip')

    def test_new_multi_export_package_uses_metadata_without_editorial_registration(self):
        component = self.add_package("new-package", slot="context_provider", version="2.0.0",
                                     exports={"tool": ["memory_write"], "context_provider": ["memory"]})
        # The shared aggregate fixture has no independent package export manifest.
        (self.root / "components/process-module").mkdir()
        (self.root / "components/process-module/Cargo.toml").write_text("[package]\nname='fixture'\n")
        with patch.object(catalog.package.subprocess, "run", side_effect=AssertionError("must not build")):
            value = catalog.catalog(self.root)
        self.assertEqual(len(value["entries"]), 1)
        entry = value["entries"][0]
        self.assertEqual(entry["version"], "2.0.0")
        self.assertEqual(entry["path"], component.relative_to(self.root).as_posix())
        self.assertEqual(entry["category"], "context-provider")
        self.assertIn("context_provider/memory", entry["description"])
        self.assertIn("tool/memory_write", entry["description"])
        self.assertEqual(entry["kind"], "process_module")

    def test_check_reports_stale_metadata_and_write_never_builds(self):
        component = self.add_package("example")
        with (contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()),
              patch.object(catalog.package.subprocess, "run", side_effect=AssertionError("must not build"))):
            self.assertEqual(catalog.main(["--check"], root=self.root), 1)
            self.assertEqual(catalog.main(["--write"], root=self.root), 0)
            self.assertEqual(catalog.main(["--check"], root=self.root), 0)
            before = (self.root / "proteus-catalog.json").read_bytes()
            path = component / "Cargo.toml"
            path.write_text(path.read_text().replace('version = "1.2.3"', 'version = "1.2.4"'))
            self.assertEqual(catalog.main(["--check"], root=self.root), 1)
            self.assertEqual((self.root / "proteus-catalog.json").read_bytes(), before)
            self.assertEqual(catalog.main(["--write"], root=self.root), 0)
        self.assertEqual(json.loads((self.root / "proteus-catalog.json").read_text())
                         ["entries"][0]["version"], "1.2.4")

    def test_invalid_exports_fail_instead_of_advertising_a_broken_module(self):
        self.add_package("broken", exports={"tool": "not-a-list"})
        with self.assertRaisesRegex(ValueError, "exports"):
            catalog.catalog(self.root)

    def test_committed_index_is_current(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(catalog.main(["--check"]), 0)


if __name__ == "__main__":
    unittest.main()
