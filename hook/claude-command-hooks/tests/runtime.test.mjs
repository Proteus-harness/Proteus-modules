import assert from "node:assert/strict";
import { cp, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { test } from "node:test";
import setup from "../src/entry.mjs";

const sdkRoot = process.env.PROTEUS_HOOK_SDK_ROOT;
const sdkTest = (name, callback) => test(name, { skip: !sdkRoot && "Set PROTEUS_HOOK_SDK_ROOT to the pinned Core SDK" }, callback);
const fixture = fileURLToPath(new URL("fixtures/command.mjs", import.meta.url));
const command = (mode = "allow", value) => ({
  type: "command", command: process.execPath,
  args: [fixture, mode, ...(value === undefined ? [] : [value])],
});
const document = (handler = command(), event = "PreToolUse", matcher = "Write") => ({
  hooks: { [event]: [{ matcher, hooks: [handler] }] },
});
const attribution = {
  execution_id: "00000000-0000-0000-0000-000000000001",
  agent: { session_id: "00000000-0000-0000-0000-000000000002", thread_id: "00000000-0000-0000-0000-000000000003", turn_id: "00000000-0000-0000-0000-000000000004" },
};
const before = (args = { path: "notes.txt", content: "hello" }, name = "write_file") => ({
  event: "before_tool", blocked: null, spec: null,
  call: { id: "00000000-0000-0000-0000-000000000005", name, args, surface: "function", raw_arguments: null },
});
const stop = (attempt = 0) => ({
  event: "before_stop", task: {}, history: [], output: { text: "Finished" }, attempt, continuation: null,
});

async function prepare(source, overrides = {}) {
  const directory = await mkdtemp(join(tmpdir(), "proteus-command-runtime-"));
  const configPath = join(directory, "hooks.json");
  await writeFile(configPath, JSON.stringify(source));
  const settings = {
    config_path: configPath, sdk_root: sdkRoot, plugin_root: directory,
    mode: "strict_subset", allow_execution: true, events: ["PreToolUse", "Stop"],
    tool_mappings: { write_file: { name: "Write", arguments: { path: "file_path" } } },
    ...overrides,
  };
  const { createHooks } = await import(pathToFileURL(join(sdkRoot, "hooks.mjs")));
  const hooks = createHooks(settings);
  await setup(hooks.api);
  hooks.seal();
  return {
    directory, settings,
    async invoke(event, options = {}) {
      return (await hooks.invoke({ event, attribution: options.attribution ?? attribution,
        cwd: directory, conversation: null }, options.signal ?? new AbortController().signal,
      () => { throw new Error("This adapter must not invoke host model/tool callbacks"); })).result;
    },
  };
}

sdkTest("configuration setup does not execute commands, even when execution is opted in", async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-setup-idle-"));
  const marker = join(directory, "marker");
  await prepare(document(command("marker", marker)));
  await assert.rejects(readFile(marker), { code: "ENOENT" });
});

sdkTest("runtime requires the owner's explicit execution opt-in", async () => {
  await assert.rejects(prepare(document(), { allow_execution: false }), /execution|allow_execution|opt.in/i);
});

sdkTest("PreToolUse maps names and inputs without inventing Claude session state", async () => {
  const port = await prepare(document(command("echo")));
  const original = before();
  const result = await port.invoke(original);
  assert.equal(result.action, "block_tool");
  const input = JSON.parse(result.reason);
  assert.equal(input.hook_event_name, "PreToolUse");
  assert.equal(input.tool_name, "Write");
  assert.deepEqual(input.tool_input, { file_path: "notes.txt", content: "hello" });
  assert.equal(input.tool_use_id, original.call.id);
  assert.equal(input.session_id, attribution.agent.session_id);
  assert.equal(input.cwd, port.directory);
  assert.equal(Object.hasOwn(input, "transcript_path"), false);
  assert.equal(Object.hasOwn(input, "permission_mode"), false);
  assert.equal(original.call.args.path, "notes.txt");
});

sdkTest("detached input does not forge a conversational identity", async () => {
  const port = await prepare(document(command("echo")));
  const result = await port.invoke(before(), { attribution: { ...attribution, agent: null } });
  assert.equal(JSON.parse(result.reason).session_id, null);
});

sdkTest("allow never grants permissions and unknown tool names do not match Write", async () => {
  const port = await prepare(document(command()));
  assert.deepEqual(await port.invoke(before()), { action: "continue" });
  const deny = await prepare(document(command("deny")));
  assert.deepEqual(await deny.invoke(before({}, "other")), { action: "continue" });
  assert.deepEqual(await deny.invoke(before({}, "constructor")), { action: "continue" });
  for (const name of ["__proto__", "constructor"]) {
    const mapped = await prepare(document(command("echo")), {
      tool_mappings: { [name]: { name: "Write", arguments: {} } },
    });
    assert.equal(JSON.parse((await mapped.invoke(before({}, name))).reason).tool_name, "Write");
  }
  const explicitNoop = await prepare(document(command("raw", '{"continue":true}')));
  assert.deepEqual(await explicitNoop.invoke(before()), { action: "continue" });
});

sdkTest("deny JSON and exit 2 both produce the canonical veto", async () => {
  for (const mode of ["deny", "exit2"]) {
    const port = await prepare(document(command(mode, "owner check failed")));
    assert.deepEqual(await port.invoke(before()), { action: "block_tool", reason: "owner check failed" });
  }
});

sdkTest("updatedInput reverses declared argument renames", async () => {
  const port = await prepare(document(command("rewrite", JSON.stringify({ file_path: "other.txt", content: "new" }))));
  assert.deepEqual(await port.invoke(before()), { action: "tool_arguments", args: { path: "other.txt", content: "new" } });
});

sdkTest("mapping refuses a collision instead of losing an existing argument", async () => {
  const port = await prepare(document(command("echo")));
  await assert.rejects(port.invoke(before({ path: "first", file_path: "second" })), /collision|ambiguous|conflict/i);
  await assert.rejects(port.invoke(before({ file_path: "native-field-must-not-be-renamed-on-return" })), /collision|ambiguous|conflict|occupied/i);
  const reverse = await prepare(document(command("rewrite", JSON.stringify({ path: "foreign-unmapped-field" }))));
  await assert.rejects(reverse.invoke(before()), /collision|ambiguous|conflict|occupied/i);
});

sdkTest("Stop maps each attempt and ignores its unsupported matcher as upstream does", async () => {
  const port = await prepare(document(command("stop"), "Stop", "never"));
  for (const attempt of [0, 1, 8]) {
    const result = await port.invoke(stop(attempt));
    assert.equal(result.action, "continue_turn");
    const input = JSON.parse(result.reason);
    assert.equal(input.hook_event_name, "Stop");
    assert.equal(input.stop_hook_active, attempt > 0);
    assert.equal(input.last_assistant_message, "Finished");
  }
});

sdkTest("multiple matching handlers fail before any command executes", async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-overlap-"));
  const marker = join(directory, "marker");
  const source = { hooks: { PreToolUse: [
    { matcher: "Write", hooks: [command("marker", marker)] },
    { matcher: "*", hooks: [command("marker", marker)] },
  ] } };
  const port = await prepare(source);
  await assert.rejects(port.invoke(before()), /multiple|more than one|ambiguous|matching/i);
  await assert.rejects(readFile(marker), { code: "ENOENT" });
});

sdkTest("strict unsupported output and command failures never silently allow", async () => {
  for (const value of [
    "plain text", "{broken", '{"continue":false}',
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}',
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","additionalContext":"new authority"}}',
    ...[null, false, 0, ""].map(value => JSON.stringify({ hookSpecificOutput: value })),
    ...[null, false, 0, {}, []].map(value => JSON.stringify({ hookSpecificOutput: {
      hookEventName: "PreToolUse", permissionDecision: "allow", permissionDecisionReason: value,
    } })),
    ...[null, false, 0, "", []].map(value => JSON.stringify({ hookSpecificOutput: {
      hookEventName: "PreToolUse", permissionDecision: "allow", updatedInput: value,
    } })),
  ]) {
    const port = await prepare(document(command("raw", value)));
    await assert.rejects(port.invoke(before()));
  }
  const failed = await prepare(document(command("error")));
  await assert.rejects(failed.invoke(before()), /exited 1|failure/);
});

sdkTest("parent cancellation cleans up commands before their delayed side effect", async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-cancel-"));
  const marker = join(directory, "marker");
  const port = await prepare(document(command("delayed-marker", marker)));
  const controller = new AbortController();
  const pending = port.invoke(before(), { signal: controller.signal });
  setTimeout(() => controller.abort(new Error("test owner cancellation")), 40);
  await assert.rejects(pending, /cancel/i);
  await new Promise((resolve) => setTimeout(resolve, 300));
  await assert.rejects(readFile(marker), { code: "ENOENT" });
});

sdkTest("per-command timeout aborts a live child and oversized output fails closed", async () => {
  const slow = await prepare(document({ ...command("sleep", "2000"), timeout: 1 }));
  const started = Date.now();
  await assert.rejects(slow.invoke(before()), /timeout|timed out/i);
  assert.ok(Date.now() - started < 1900);
  const loud = await prepare(document(command("overflow")));
  await assert.rejects(loud.invoke(before()), /1 MiB|output/i);
});

sdkTest("plugin path metacharacters stay data and do not mutate global environment", async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus ' $ ` path-"));
  const script = join(directory, "fixture.mjs");
  await cp(fixture, script);
  const priorRoot = process.env.CLAUDE_PLUGIN_ROOT;
  const priorProject = process.env.CLAUDE_PROJECT_DIR;
  const port = await prepare(document({
    type: "command", command: process.execPath,
    args: ["${CLAUDE_PLUGIN_ROOT}/fixture.mjs", "env"],
  }), { plugin_root: directory });
  const result = await port.invoke(before());
  assert.deepEqual(JSON.parse(result.reason), { root: directory, project: port.directory });
  assert.equal(process.env.CLAUDE_PLUGIN_ROOT, priorRoot);
  assert.equal(process.env.CLAUDE_PROJECT_DIR, priorProject);
  const shell = await prepare(document({
    type: "command", command: `"${process.execPath}" "\${CLAUDE_PLUGIN_ROOT}/fixture.mjs" env`,
  }), { plugin_root: directory });
  assert.deepEqual(JSON.parse((await shell.invoke(before())).reason), { root: directory, project: shell.directory });
  const otherRoot = await mkdtemp(join(tmpdir(), "proteus-other-root-"));
  const other = await prepare(document(command("env")), { plugin_root: otherRoot });
  const [firstResult, secondResult] = await Promise.all([port.invoke(before()), other.invoke(before())]);
  assert.equal(JSON.parse(firstResult.reason).root, directory);
  assert.equal(JSON.parse(secondResult.reason).root, otherRoot);
});

sdkTest("changed SDK bytes are not silently accepted as the pinned runtime", async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-sdk-pin-"));
  await cp(sdkRoot, directory, { recursive: true });
  await writeFile(join(directory, "command.mjs"), "throw new Error('must not import mismatched SDK');\n");
  await assert.rejects(prepare(document(), { sdk_root: directory }), /pin|hash|digest|mismatch/i);
});

sdkTest("exec placeholders cannot become env assignments after substitution", async () => {
  const directory = await mkdtemp(join(tmpdir(), "proteus-root=assignment-"));
  const marker = join(directory, "must-not-exist");
  const port = await prepare(document({
    type: "command", command: "${CLAUDE_PLUGIN_ROOT}/not-an-executable",
    args: [process.execPath, fixture, "marker", marker],
  }), { plugin_root: directory });
  await assert.rejects(port.invoke(before()), /expanded exec executable|env launcher/i);
  await assert.rejects(readFile(marker), { code: "ENOENT" });
});
