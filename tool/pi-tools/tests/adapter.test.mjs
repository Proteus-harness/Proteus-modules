import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { createPiDefinitions, executePiTool, specFor, toToolResult } from "../adapter.mjs";

const base = resolve("../../target/pi-tools-tests");
await mkdir(base, { recursive: true });

async function fixture(run) {
    const cwd = await mkdtemp(join(base, "case-"));
    try { await run(cwd); } finally { await rm(cwd, { recursive: true, force: true }); }
}

async function direct(cwd, name, args, signal = new AbortController().signal) {
    const tool = createPiDefinitions(cwd).find(tool => tool.name === name);
    try {
        return toToolResult("case", await tool.execute("case", args, signal));
    } catch (error) { return toToolResult("case", null, error); }
}

test("all four descriptions and schemas come directly from pinned Pi", async () => {
    const definitions = createPiDefinitions(base);
    assert.deepEqual(definitions.map(tool => tool.name), ["read", "bash", "edit", "write"]);
    for (const tool of definitions) {
        const spec = specFor(tool);
        assert.equal(spec.description, tool.description);
        assert.deepEqual(spec.input_schema, tool.parameters);
    }
});

test("native and component adapter agree on text, ranges and errors", async () => fixture(async cwd => {
    await writeFile(join(cwd, "text.txt"), "one\ntwo\nthree\n");
    for (const args of [{ path: "text.txt" }, { path: "text.txt", offset: 2, limit: 1 }, { path: "missing.txt" }]) {
        const expected = await direct(cwd, "read", args);
        const actual = await executePiTool({ cwd, call: { id: "case", name: "read", args } }, new AbortController().signal);
        assert.deepEqual(actual, expected);
    }
}));

test("native and adapter preserve multi-edit, BOM and CRLF", async () => fixture(async cwd => {
    const original = "\ufeffalpha\r\nbeta\r\ngamma\r\n";
    const args = { path: "text.txt", edits: [{ oldText: "alpha", newText: "first" }, { oldText: "gamma", newText: "third" }] };
    await writeFile(join(cwd, args.path), original);
    const expected = await direct(cwd, "edit", args);
    const expectedFile = await readFile(join(cwd, args.path));
    await writeFile(join(cwd, args.path), original);
    const actual = await executePiTool({ cwd, call: { id: "case", name: "edit", args } }, new AbortController().signal);
    assert.deepEqual(actual, expected);
    assert.deepEqual(await readFile(join(cwd, args.path)), expectedFile);
}));

test("ambiguous edit fails without changing the file", async () => fixture(async cwd => {
    const args = { path: "text.txt", edits: [{ oldText: "same", newText: "other" }] };
    await writeFile(join(cwd, args.path), "same\nsame\n");
    const expected = await direct(cwd, "edit", args);
    const actual = await executePiTool({ cwd, call: { id: "case", name: "edit", args } }, new AbortController().signal);
    assert.equal(actual.ok, false);
    assert.deepEqual(actual, expected);
    assert.equal(await readFile(join(cwd, args.path), "utf8"), "same\nsame\n");
}));

test("native and adapter create directories and overwrite content identically", async () => fixture(async cwd => {
    const args = { path: "nested/text.txt", content: "written\n" };
    const expected = await direct(cwd, "write", args);
    await rm(join(cwd, "nested"), { recursive: true });
    const actual = await executePiTool({ cwd, call: { id: "case", name: "write", args } }, new AbortController().signal);
    assert.deepEqual(actual, expected);
    assert.equal(await readFile(join(cwd, args.path), "utf8"), args.content);
}));

test("native and adapter keep bash stdout and nonzero failure semantics", async () => fixture(async cwd => {
    for (const args of [{ command: "printf hello" }, { command: "printf failure; exit 7" }]) {
        const expected = await direct(cwd, "bash", args);
        assert.equal(expected.ok, args.command === "printf hello");
        const actual = await executePiTool({ cwd, call: { id: "case", name: "bash", args } }, new AbortController().signal);
        assert.deepEqual(actual, expected);
    }
}));

test("AbortSignal stops upstream bash before its later effect", async () => fixture(async cwd => {
    const controller = new AbortController();
    const pending = executePiTool({ cwd, call: { id: "case", name: "bash", args: { command: "sleep .15; touch late" } } }, controller.signal);
    setTimeout(() => controller.abort(), 30);
    const result = await pending;
    assert.equal(result.ok, false);
    await new Promise(resolve => setTimeout(resolve, 200));
    await assert.rejects(readFile(join(cwd, "late")), { code: "ENOENT" });
}));
