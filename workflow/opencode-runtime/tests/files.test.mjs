import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { Effect } from "effect";
import { executeNativeFile, nativeFileDefinitions, withNativeFiles } from "../files.mjs";
import { instructionEvents, projectInstructions } from "../instructions.mjs";

const target = resolve("../../target/opencode-runtime-fixtures");
process.env.PROTEUS_OPENCODE_RUNTIME_DIR = `${target}/runtime`;
process.env.OPENCODE_TEST_HOME = target;
await mkdir(target, { recursive: true });
async function fixture(t) {
    const cwd = await mkdtemp(`${target}/files-`);
    await mkdir(`${cwd}/.git`);
    t.after(() => rm(cwd, { recursive: true, force: true }));
    return cwd;
}
function input(cwd, name, args, id = "call-1") {
    return { cwd, call: { id, name, args, raw_arguments: null }, attribution: { execution_id: "test-execution", agent: null } };
}
function message(result) {
    return { id: "e3f4f671-ac73-4d3b-9f57-65082685a4c5", role: "Tool", metadata: null, parts: [{ payload: { ToolResult: { result } } }] };
}

test("native definitions are exact workspace read/write/edit schemas", async t => {
    const cwd = await fixture(t);
    const definitions = await nativeFileDefinitions(cwd);
    assert.deepEqual(definitions.map(tool => tool.name), ["read", "write", "edit"]);
    assert.equal(definitions.find(tool => tool.name === "read").input_schema.properties.offset.type, "integer");
    assert.equal(definitions.find(tool => tool.name === "edit").input_schema.properties.oldString.type, "string");
    assert.ok(definitions.every(tool => !tool.supports_parallel_tool_calls));
});

test("adapter output equals real upstream execute on read range and missing file", async t => {
    const cwd = await fixture(t);
    await writeFile(`${cwd}/a.txt`, "alpha\nbeta\ngamma\n");
    const args = { path: `${cwd}/a.txt`, offset: 2, limit: 1 };
    const { result: actual } = await withNativeFiles(cwd, tools => tools.get("read").execute(args, { id: "call-1", agent: "build", sessionID: "test-execution", messageID: "call-1", progress: () => Effect.void }));
    const result = await executeNativeFile(input(cwd, "read", args));
    assert.equal(result.ok, true);
    assert.equal(result.output, actual.content);
    assert.deepEqual(result.metadata["opencode/output"], actual.output ? JSON.parse(JSON.stringify(actual.output)) : null);
    assert.match(result.output, /beta/);
    assert.doesNotMatch(result.output, /alpha/);
    const missing = await executeNativeFile(input(cwd, "read", { path: `${cwd}/missing` }));
    assert.equal(missing.ok, false);
    assert.match(missing.error, /not found|ENOENT/i);
});

test("native write/edit preserve BOM and execute real replaceAll/error semantics", async t => {
    const cwd = await fixture(t);
    const filePath = `${cwd}/nested/a.txt`;
    assert.equal((await executeNativeFile(input(cwd, "write", { path: filePath, content: "one\none\n" }))).ok, true);
    await writeFile(filePath, "\ufeffone\none\n");
    const ambiguous = await executeNativeFile(input(cwd, "edit", { path: filePath, oldString: "one", newString: "two" }));
    assert.equal(ambiguous.ok, false);
    assert.match(ambiguous.error, /multiple|unique|more than once/i);
    assert.equal(await readFile(filePath, "utf8"), "\ufeffone\none\n");
    const edited = await executeNativeFile(input(cwd, "edit", { path: filePath, oldString: "one", newString: "two", replaceAll: true }));
    assert.equal(edited.ok, true);
    assert.equal(await readFile(filePath, "utf8"), "\ufefftwo\ntwo\n");
    assert.equal((await executeNativeFile(input(cwd, "write", { path: filePath, content: "new\n" }))).ok, true);
    assert.equal(await readFile(filePath, "utf8"), "\ufeffnew\n");
});

test("native AGENTS injection survives canonical metadata, projects once and deduplicates history", async t => {
    const cwd = await fixture(t);
    await mkdir(`${cwd}/nested`);
    const filePath = `${cwd}/nested/a.txt`;
    const instructions = `${cwd}/nested/AGENTS.md`;
    await writeFile(filePath, "hello\n");
    await writeFile(instructions, "Use narrow changes.\n");
    const result = await executeNativeFile(input(cwd, "read", { path: filePath }));
    assert.equal(result.ok, true, result.error);
    const events = result.metadata["opencode/synthetic"];
    assert.equal(events.length, 1);
    assert.equal(events[0].text, `Instructions from: ${instructions}\nUse narrow changes.\n`);
    assert.deepEqual(events[0].metadata, { instruction: { paths: [instructions] } });
    const history = [message(result)];
    assert.equal(instructionEvents(history).length, 1);
    const projected = projectInstructions(history, { provider: "test", model: "fake" });
    assert.equal(projected.length, 2);
    assert.equal(projected[1].role, "User");
    assert.equal(projected[1].parts[0].payload.Text.text, events[0].text);
    assert.deepEqual(projectInstructions(projected, { provider: "test", model: "fake" }), projected);
    const again = await executeNativeFile(input(cwd, "read", { path: filePath }, "call-2"), undefined, { message_id: "owner", conversation: { messages: history } });
    assert.equal(again.ok, true);
    assert.deepEqual(again.metadata["opencode/synthetic"], []);
});

test("external permission and pre-aborted execution fail before file effects", async t => {
    const cwd = await fixture(t);
    const controller = new AbortController();
    controller.abort();
    const filePath = `${cwd}/never-written`;
    const canceled = await executeNativeFile(input(cwd, "write", { path: filePath, content: "bad" }), controller.signal);
    assert.equal(canceled.ok, false);
    await assert.rejects(readFile(filePath));
    const outside = await executeNativeFile(input(cwd, "read", { path: resolve(cwd, "../outside") }));
    assert.equal(outside.ok, false);
    assert.match(outside.error, /Unable to read/);
});

test("actual native input repair removes nulls, coerces scalar strings and rejects required missing arguments", async t => {
    const cwd = await fixture(t);
    await writeFile(`${cwd}/a.txt`, "alpha\nbeta\n");
    const result = await executeNativeFile(input(cwd, "read", { path:"a.txt", offset:"2", limit:null, extra:"discarded" }));
    assert.equal(result.ok, true, result.error);
    assert.match(result.output, /beta/);
    assert.doesNotMatch(result.output, /alpha/);
    const invalid = await executeNativeFile(input(cwd, "read", { offset:2 }));
    assert.equal(invalid.ok, false);
    assert.match(invalid.error, /Invalid arguments for tool "read"/);
});

test("actual native patch supports add/update/move/delete and verifies before effects", async t => {
    const cwd = await fixture(t);
    await writeFile(`${cwd}/before.txt`, "before\n");
    await writeFile(`${cwd}/delete.txt`, "delete\n");
    const patchText = "*** Begin Patch\n*** Add File: new.txt\n+new\n*** Update File: before.txt\n*** Move to: moved.txt\n@@\n-before\n+after\n*** Delete File: delete.txt\n*** End Patch";
    const result = await executeNativeFile(input(cwd,"patch",{patchText}));
    assert.equal(result.ok,true,result.error);
    assert.match(result.output,/A new.txt/);
    assert.equal(await readFile(`${cwd}/new.txt`,"utf8"),"new\n");
    assert.equal(await readFile(`${cwd}/moved.txt`,"utf8"),"after\n");
    await assert.rejects(readFile(`${cwd}/before.txt`));
    await assert.rejects(readFile(`${cwd}/delete.txt`));
    const invalid = await executeNativeFile(input(cwd,"patch",{patchText:"*** Begin Patch\n*** Add File: must-not-exist\n+no\n*** Update File: absent\n@@\n-x\n+y\n*** End Patch"}));
    assert.equal(invalid.ok,false);
    await assert.rejects(readFile(`${cwd}/must-not-exist`));
});
