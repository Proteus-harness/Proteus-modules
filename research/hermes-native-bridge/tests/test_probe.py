"""Run the native seam controls against a verified local pinned checkout."""
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
from native import NativeSource, SOURCES
import probe


class NativeBridgeProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.checkout = Path(os.environ.get(
            "HERMES_UPSTREAM_SOURCE", str(HERE.parents[1] / "target/research/hermes-agent-v0.23.1")
        ))
        cls.native = NativeSource(cls.checkout)

    def test_native_sequential_todo_bypasses_only_invoke_override(self):
        result = probe.sequential_tool_bypass(self.native)
        self.assertEqual(result["native_todo_revision"], 1)
        self.assertEqual(result["invoke_override_calls"], 0)

    def test_native_middleware_error_falls_through_but_explicit_denial_stops(self):
        result = probe.middleware_failure_is_open(self.native)
        self.assertEqual(result["terminal_effects"], 1)
        self.assertTrue(result["explicit_denial_control_prevented_effect"])

    def test_native_summary_has_separate_auxiliary_boundary(self):
        result = probe.auxiliary_summary_bypass(self.native)
        self.assertEqual(result["auxiliary_calls"], 1)
        self.assertEqual(result["primary_client_calls"], 0)

    def test_native_progress_success_is_not_core_durable_ack(self):
        result = probe.persistence_is_not_core_ack(self.native)
        self.assertTrue(result["native_progress_acknowledged"])
        self.assertIsNone(result["core_checkpoint_revision"])

    def test_unexpected_checkout_revision_is_rejected(self):
        with patch("native.subprocess.check_output", return_value="0" * 40):
            with self.assertRaisesRegex(ValueError, "pinned release commit"):
                NativeSource(self.checkout)

    def test_modified_pinned_source_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="hermes-source-control-") as temporary:
            root = Path(temporary)
            for relative in SOURCES["files"]:
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(self.checkout / relative, target)
            changed = root / "agent/tool_executor.py"
            changed.write_bytes(changed.read_bytes() + b"\n# modified source\n")
            with patch("native.subprocess.check_output", return_value=SOURCES["revision"]):
                with self.assertRaisesRegex(ValueError, "Pinned Hermes source changed"):
                    NativeSource(root)


if __name__ == "__main__":
    unittest.main()
