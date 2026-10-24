import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { inspectConfig } from "../src/config.mjs";

const fixture = fileURLToPath(new URL("fixtures/command.mjs", import.meta.url));
const command = (mode = "allow", value) => ({
  type: "command", command: process.execPath,
  args: [fixture, mode, ...(value === undefined ? [] : [value])],
});
const source = (hook = command()) => ({ hooks: { PreToolUse: [{ matcher: "Write", hooks: [hook] }] } });

async function inspect(document, overrides = {}) {
  const directory = await mkdtemp(join(tmpdir(), "proteus-hook-config-"));
  const path = join(directory, "hooks.json");
  await writeFile(path, typeof document === "string" ? document : JSON.stringify(document));
  const settings = {
    config_path: path,
    sdk_root: process.env.PROTEUS_HOOK_SDK_ROOT ?? join(directory, "sdk"),
    plugin_root: directory,
    events: ["PreToolUse", "Stop"], mode: "strict_subset", allow_execution: false,
    tool_mappings: { write_file: { name: "Write", arguments: { path: "file_path" } } },
    ...overrides,
  };
  return { ...await inspectConfig(settings), settings, directory };
}

test("inspection parses unchanged command configuration without executing it", async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-no-execution-"));
  const marker = join(directory, "must-not-exist");
  const literal = `echo unchanged > '${marker}'`;
  const result = await inspect(source({ type: "command", command: literal }));
  assert.equal(result.report.compatible, true);
  assert.ok(result.report.hooks.length > 0);
  await assert.rejects(readFile(marker), { code: "ENOENT" });
  const execResult = await inspect(source(command("marker", marker)));
  assert.equal(execResult.report.compatible, true);
  await assert.rejects(readFile(marker), { code: "ENOENT" });
});

test("known unsupported selected hook behavior is reported, not silently skipped", async () => {
  for (const hook of [
    { type: "prompt", prompt: "review" },
    { ...command(), async: true },
    { ...command(), asyncRewake: true },
    { ...command(), if: "Write(*.ts)" },
    { ...command(), onFailure: "continue" },
    { ...command(), shell: "powershell" },
    { ...command(), unexpected: true },
  ]) {
    const { report } = await inspect(source(hook));
    assert.equal(report.compatible, false, JSON.stringify(hook));
    assert.ok(report.diagnostics.some((item) => item.level === "error"));
  }
});

test("unselected event diagnostics do not pretend that its behavior was ported", async () => {
  const document = source();
  document.hooks.SessionStart = [{ hooks: [{ type: "command", command: "not executed" }] }];
  const { report } = await inspect(document);
  assert.equal(report.compatible, true);
  assert.ok(report.diagnostics.some((item) => /SessionStart/.test(JSON.stringify(item))));
});

test("missing files, invalid JSON and relative explicit paths never auto-discover hooks", async () => {
  const malformed = await inspect("{");
  assert.equal(malformed.report.compatible, false);
  for (const changes of [
    { config_path: "hooks.json" },
    { config_path: "/no-such-proteus-config-849943/hooks.json" },
    { sdk_root: "relative-sdk" },
    { plugin_root: "relative-plugin" },
    { mode: "compatible" },
    { events: ["SessionStart"] },
    { events: [] },
    { events: ["PreToolUse", "PreToolUse"] },
  ]) {
    const { report } = await inspect(source(), changes);
    assert.equal(report.compatible, false, JSON.stringify(changes));
  }
});

test("unbounded commands, timeouts and ambiguous argument maps are rejected", async () => {
  for (const hook of [
    { ...command(), command: "" },
    { ...command(), command: "x".repeat(65_537) },
    { ...command(), timeout: 0 },
    { ...command(), timeout: -1 },
    { ...command(), timeout: 601 },
    { ...command(), args: "not-an-array" },
    { ...command(), args: [3] },
    { ...command(), command: "-not-an-executable" },
    { ...command(), command: "OVERRIDE=value" },
    { ...command(), args: ["$CLAUDE_PLUGIN_DATA/file"] },
    { ...command(), args: ["${CLAUDE_PLUGIN_DATA}/file"] },
  ]) {
    assert.equal((await inspect(source(hook))).report.compatible, false, JSON.stringify(hook).slice(0, 200));
  }
  const { report } = await inspect(source(), {
    tool_mappings: { write_file: { name: "Write", arguments: { path: "file_path", other: "file_path" } } },
  });
  assert.equal(report.compatible, false);
});

test("Stop matcher does not falsely filter a lifecycle event", async () => {
  const { report } = await inspect({ hooks: { Stop: [{ matcher: "never", hooks: [command("stop")] }] } });
  assert.equal(report.compatible, true);
  assert.ok(report.diagnostics.some((item) => /matcher/i.test(JSON.stringify(item))));
});

test("bounded inspection rejects oversized files and respects disableAllHooks", async () => {
  const oversized = await inspect(" ".repeat(1_048_577));
  assert.equal(oversized.report.compatible, false);
  assert.ok(oversized.report.diagnostics.some(item => /1 MiB/.test(item.message)));
  const disabled = await inspect({ disableAllHooks: true, ...source() });
  assert.equal(disabled.report.compatible, true);
  assert.equal(disabled.plan.handlers.length, 0);
});

test("explicit null configuration does not receive an implicit default", async () => {
  assert.equal((await inspect(source({ ...command(), timeout: null }))).report.compatible, false);
  for (const changes of [
    { tool_mappings: null },
    { tool_mappings: { write_file: { name: "Write", arguments: null } } },
  ]) {
    assert.equal((await inspect(source(), changes)).report.compatible, false);
  }
  assert.equal((await inspect(source(), {
    tool_mappings: { write_file: { name: "Write" } },
  })).report.compatible, true, "omitted optional fields still have their documented defaults");
});

test("inspection rejects FIFO paths without waiting for a writer", { skip: process.platform === "win32" }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-hook-fifo-"));
  const fifo = join(directory, "config.pipe");
  execFileSync("mkfifo", [fifo]);
  // A child deadline keeps this regression bounded even if nonblocking open breaks.
  const moduleUrl = new URL("../src/config.mjs", import.meta.url).href;
  const probe = `import {readJson} from ${JSON.stringify(moduleUrl)};
    try { await readJson(process.argv[1]); process.exitCode = 2; }
    catch (error) { console.log(error.message); }`;
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", probe, fifo], { encoding: "utf8", timeout: 2000 });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /regular file/);
});

test("inspection rejects certain Stop overlap and reports possible tool overlap", async () => {
  const stop = await inspect({ hooks: { Stop: [{ hooks: [command("stop"), command("stop")] }] } });
  assert.equal(stop.report.compatible, false);
  assert.ok(stop.report.diagnostics.some(item => item.level === "error" && /overlap|multiple/i.test(item.message)));
  const tool = await inspect({ hooks: { PreToolUse: [
    { matcher: "Write", hooks: [command()] },
    { matcher: "Read", hooks: [command()] },
  ] } });
  assert.equal(tool.report.compatible, true);
  assert.ok(tool.report.diagnostics.some(item => item.level === "warning" && /overlap/i.test(item.message)));
});

test("committed validate-bash recipe is inspectable without fetching or running upstream code", async () => {
  const recipePath = fileURLToPath(new URL("../examples/validate-bash.hooks.json", import.meta.url));
  const original = await readFile(recipePath, "utf8");
  const recipe = JSON.parse(original);
  const result = await inspect(recipe, {
    config_path: recipePath, events: ["PreToolUse"],
    tool_mappings: { shell: { name: "Bash", arguments: { command: "command" } } },
  });
  assert.equal(result.report.compatible, true);
  assert.equal(result.plan.handlers.length, 1);
  assert.equal(result.plan.handlers[0].hook.command, recipe.hooks.PreToolUse[0].hooks[0].command);
  assert.equal(result.plan.handlers[0].matches("Bash"), true);
  assert.equal(result.plan.handlers[0].matches("MyBash"), false);
  assert.deepEqual(result.plan.mappings.get("shell"), { name: "Bash", arguments: [["command", "command"]] });
  assert.equal(await readFile(recipePath, "utf8"), original);
});
