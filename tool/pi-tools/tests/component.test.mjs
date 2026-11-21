import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, join } from "node:path";

test("real tool/v7 and context/v3 process preserves native prompts, four tools and bash cancellation", async () => {
    const base = fileURLToPath(new URL("../../../target/pi-tools-tests",import.meta.url));
    await mkdir(base, { recursive: true });
    const cwd = await mkdtemp(join(base, "wire-"));
    const agent = join(cwd, "agent");
    await mkdir(agent);
    await writeFile(join(agent, "SYSTEM.md"), "Wire native system prompt");
    await writeFile(join(agent, "APPEND_SYSTEM.md"), "Wire native append prompt");
    const child = spawn(process.execPath, [fileURLToPath(new URL("../worker.mjs",import.meta.url))], {
        env: { ...process.env, PI_CODING_AGENT_DIR: agent }, stdio: ["pipe", "pipe", "pipe"] });
    const lines = createInterface({ input: child.stdout });
    let stderr = "";
    child.stderr.on("data", data => { stderr += data; });
    const waiters = new Map();
    lines.on("line", line => {
        const response = JSON.parse(line);
        const waiter = waiters.get(response.id);
        if (waiter) { waiters.delete(response.id); waiter(response); }
    });
    let nextId = 0;
    const send = value => child.stdin.write(`${JSON.stringify(value)}\n`);
    function rpc(method, params) {
        const id = `h:7:${nextId++}`;
        const response = new Promise(resolve => waiters.set(id, resolve));
        send({ jsonrpc: "2.0", id, method, params });
        return { id, response };
    }
    function invoke(method, params, selected = { slot: "tool", module_id: "pi_tools" }) {
        const id = `h:7:${nextId}`;
        return rpc(method, { export: selected, lineage: { root_invocation_id: id, parent_invocation_id: null, depth: 0 }, params });
    }
    function tool(name, args) {
        return invoke("invoke", { call: { id: "test-call", name, args }, cwd, attribution: { execution_id: "test", agent: null }, skills: { disabled: [], packages: [] } });
    }
    try {
        const initialized = await rpc("initialize", { protocol_version: "v3", component_id: "pi", exports: [
            { slot: "tool", module_id: "pi_tools", contract_version: "v7", composition: "ordered_many", module_config: {}, host_features: [] },
            { slot: "context", module_id: "pi_context", contract_version: "v3", composition: "select_one", module_config: {}, host_features: [] },
        ] }).response;
        assert.ok(initialized.result, stderr);
        const built = await invoke("build", { task: { cwd } }, { slot: "context", module_id: "pi_context" }).response;
        assert.equal(built.error, undefined, stderr);
        assert.equal(built.result.result.chunks.find(c => c.metadata?.["pi/prompt"] === "system").content, "Wire native system prompt");
        assert.equal(built.result.result.chunks.find(c => c.metadata?.["pi/prompt"] === "append").content, "Wire native append prompt");
        const listed = await invoke("list", null).response;
        assert.deepEqual(listed.result.result.map(entry => entry.spec.name), ["read", "bash", "edit", "write"]);
        assert.equal((await tool("write", { path: "sample.txt", content: "before\n" }).response).result.result.ok, true);
        assert.equal((await tool("edit", { path: "sample.txt", edits: [{ oldText: "before", newText: "after" }] }).response).result.result.ok, true);
        assert.equal((await tool("read", { path: "sample.txt" }).response).result.result.output, "after\n");
        assert.equal((await tool("bash", { command: "printf child-output" }).response).result.result.output, "child-output");
        const pending = tool("bash", { command: "sleep .2; touch late" });
        setTimeout(() => send({ jsonrpc: "2.0", method: "$/cancelRequest", params: { invocation_id: pending.id, cause: "user" } }), 30);
        assert.equal((await pending.response).result.result.ok, false);
        await new Promise(resolve => setTimeout(resolve, 250));
        await assert.rejects(readFile(join(cwd, "late")), { code: "ENOENT" });
    } finally {
        child.stdin.end();
        await new Promise(resolve => child.once("exit", resolve));
        await rm(cwd, { recursive: true, force: true });
    }
});
