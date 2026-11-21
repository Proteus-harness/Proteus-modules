"""Provider creation metadata can be installed without npm or model requests."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("opencode_runtime_installer", ROOT / "workflow/opencode-runtime/install.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class OpenCodeCreationMetadata(unittest.TestCase):
    def test_manifest_describes_concrete_creation_defaults_and_wildcard_capacity(self):
        folder = ROOT / "target/provider-creation-tests"
        folder.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="metadata-", dir=folder) as temporary:
            prefix = Path(temporary)
            binary = prefix / "bin/proteus-opencode-module"
            binary.parent.mkdir()
            binary.write_bytes(b"synthetic launcher")
            path = installer.write_module_manifest(prefix)
            manifest = json.loads(path.read_text())
            self.assertEqual(path, prefix / "share/modules/opencode-runtime/module.json")
            self.assertEqual(manifest["exports"]["model"], ["*"])
            self.assertEqual(manifest["command"], ["bin/proteus-opencode-module"])
            self.assertEqual(manifest["sha256"], hashlib.sha256(binary.read_bytes()).hexdigest())
            presets = manifest["model_connections"]
            self.assertTrue({"opencode-go", "opencode-zen"} <= {entry["id"] for entry in presets})
            self.assertTrue(all(entry["module_id"] != "*" and "HOME" in entry["env_allowlist"] for entry in presets))
            self.assertFalse(any("api_key" in entry["module_config"] for entry in presets))
