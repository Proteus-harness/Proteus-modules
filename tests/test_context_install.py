"""Run both installers with cached dependencies, then use the installed adapters."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
NODE = ROOT / "workflow/opencode-runtime/node_modules/node-linux-x64/bin/node"


def installer(relative, name):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class InstalledContextAdapters(unittest.TestCase):
    def test_real_installers_copy_the_shared_context_import_closure(self):
        folder = ROOT / "target/context-install-tests"
        folder.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="installed-", dir=folder) as temporary:
            prefix = Path(temporary)

            def cached_dependencies(source, destination, **kwargs):
                # Reuse the exact existing dependency tree without npm/network or
                # another large copy; every production source copy runs normally.
                source, destination = Path(source), Path(destination)
                self.assertEqual(source.name, "node_modules")
                self.assertTrue(source.is_dir())
                os.symlink(source.resolve(), destination, target_is_directory=True)

            for relative, name in [("tool/pi-tools/install.py", "context_pi_installer"),
                                   ("workflow/opencode-runtime/install.py", "context_opencode_installer")]:
                module = installer(relative, name)
                with patch.object(sys, "argv", [str(ROOT / relative), "--prefix", str(prefix)]), \
                     patch.object(module.subprocess, "run") as npm, \
                     patch.object(module.shutil, "copytree", side_effect=cached_dependencies), \
                     contextlib.redirect_stdout(io.StringIO()):
                    module.main()
                    npm.assert_called_once()
                    self.assertEqual(npm.call_args.args[0][:2], ["npm", "ci"])

            script = prefix / "check.mjs"
            script.write_text('''
import assert from 'node:assert/strict';
import { piMessages } from './share/pi-runtime/components/pi-runtime-js/messages.mjs';
import { toNativeRequest } from './share/opencode-runtime/model/opencode-provider/messages.mjs';
import { LanguageModel } from './share/opencode-runtime/workflow/opencode-runtime/node_modules/@opencode/ai/dist/index.js';
const chunk = { source: 'fixture', path: 'src/δ.rs', content: 'exact\\r\\ntext\\n',
    render_mode: 'source_annotated', score: null, metadata: null };
const messages = [{ id: 'user', role: 'User', phase: null, name: null, tool_call_id: null,
    metadata: null, parts: [{ payload: { Context: { chunk } } }] }];
const expected = 'Context from fixture (src/δ.rs):\\nexact\\r\\ntext\\n';
assert.equal(piMessages(messages, {})[0].content[0].text, expected);
const request = { messages, instructions: [], tools: [], tool_choice: 'Auto', response_format: 'Text',
    sampling: {}, limits: {}, cache: {}, client_metadata: {} };
const model = LanguageModel.make({ id: 'fixture', provider: 'fixture', route: {} });
assert.equal((await toNativeRequest(request, model)).messages[0].content[0].text, expected);
''')
            subprocess.run([str(NODE), str(script)], check=True, capture_output=True, text=True, timeout=30)
            self.assertTrue(os.access(prefix / "bin/proteus-pi-module", os.X_OK))
            self.assertTrue(os.access(prefix / "bin/proteus-opencode-module", os.X_OK))
