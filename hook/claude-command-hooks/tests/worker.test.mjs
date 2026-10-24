import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const sdk = process.env.PROTEUS_HOOK_SDK_ROOT;
const entry = fileURLToPath(new URL("../src/entry.mjs", import.meta.url));
const fixture = fileURLToPath(new URL("fixtures/command.mjs", import.meta.url));

test("pinned process worker routes this adapter and cancels only the addressed command", {
  skip: !sdk && "Set PROTEUS_HOOK_SDK_ROOT to the pinned Core SDK", timeout: 10_000,
}, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-command-wire-"));
  const bindings = [];
  for (const [id, mode, value] of [["check", "deny", "wire veto"], ["slow", "sleep", "5000"]]) {
    const config = join(directory, `${id}.json`);
    await writeFile(config, JSON.stringify({ hooks: {
      PreToolUse: [{ matcher: "Write", hooks: [{ type: "command", command: process.execPath, args: [fixture, mode, value] }] }],
      Stop: [{ hooks: [{ type: "command", command: process.execPath, args: [fixture, "exit2", "verify before finishing"] }] }],
    } }));
    bindings.push({
      slot: "hook", module_id: id, contract_version: "v6", composition: "ordered_many", host_features: [],
      module_config: { entry, settings: {
        config_path: config, sdk_root: sdk, plugin_root: directory,
        mode: "strict_subset", allow_execution: true, events: ["PreToolUse", "Stop"],
        tool_mappings: { write_file: { name: "Write", arguments: { path: "file_path" } } },
      } },
    });
  }
  const child = spawn(process.execPath, [join(sdk, "worker.mjs")], { cwd: directory, stdio: ["pipe", "pipe", "pipe"] });
  const closed = once(child, "close");
  const lines = createInterface({ input: child.stdout });
  const pending = new Map();
  let stderr = "";
  let unexpected;
  child.stderr.setEncoding("utf8").on("data", chunk => { stderr += chunk; });
  lines.on("line", line => {
    try {
      const frame = JSON.parse(line);
      const resolve = pending.get(frame.id);
      if (!resolve) { unexpected = frame; return; }
      pending.delete(frame.id);
      resolve(frame);
    } catch (error) { unexpected = error; }
  });
  t.after(async () => {
    child.stdin.end();
    const kill = setTimeout(() => child.kill("SIGKILL"), 1000);
    await closed;
    clearTimeout(kill);
    lines.close();
  });
  const send = (id, method, params) => new Promise(resolve => {
    pending.set(id, resolve);
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
  });
  const event = { event: "before_tool", spec: null, blocked: null, call: {
    id: "00000000-0000-0000-0000-000000000005", name: "write_file", args: { path: "wire.txt" },
    surface: "function", raw_arguments: null,
  } };
  const invoke = (number, module, selected = event) => {
    const id = `h:7:${number}`;
    return send(id, "hook.invoke", {
      export: { slot: "hook", module_id: module },
      lineage: { root_invocation_id: id, parent_invocation_id: null, depth: 0 },
      params: { cwd: directory, attribution: { execution_id: "00000000-0000-0000-0000-000000000001", agent: null }, conversation: null, event: selected },
    });
  };
  const initialized = await send("h:7:0", "initialize", { protocol_version: "v3", component_id: "claude-port-test", exports: bindings });
  assert.equal(initialized.error, undefined, stderr);
  assert.equal(initialized.result.exports.length, 2);
  assert.equal(initialized.result.exports[0].contract_version, "v6");
  const slow = invoke(1, "slow");
  const quick = await invoke(2, "check");
  assert.deepEqual(quick.result.result, { action: "block_tool", reason: "wire veto" });
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "$/cancelRequest", params: { invocation_id: "h:7:1", cause: "user" } })}\n`);
  assert.equal((await slow).error.code, -32800);
  const stopped = await invoke(3, "check", { event: "before_stop", task: {}, history: [], output: { text: "candidate" }, attempt: 0, continuation: null });
  assert.deepEqual(stopped.result.result, { action: "continue_turn", reason: "verify before finishing" });
  assert.equal(unexpected, undefined, "adapter must not emit host callbacks or corrupt protocol stdout");
});
