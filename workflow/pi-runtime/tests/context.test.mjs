import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { context } from "../context.mjs";
import { workflow } from "../loop.mjs";
import { canonical } from "../../../components/module-runtime-js/canonical.mjs";

async function fixture(t) {
    const base = resolve("target/performance/pi-prompt-fixtures");
    await mkdir(base, { recursive: true });
    const root = await mkdtemp(`${base}/resources-`);
    const cwd = `${root}/workspace`, agent = `${root}/agent`;
    await mkdir(`${cwd}/.pi`, { recursive: true });
    await mkdir(`${cwd}/.git`);
    await mkdir(agent);
    const previous = process.env.PI_CODING_AGENT_DIR;
    process.env.PI_CODING_AGENT_DIR = agent;
    t.after(async () => {
        if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
        else process.env.PI_CODING_AGENT_DIR = previous;
        await rm(root, { recursive: true, force: true });
    });
    return { root, cwd, agent };
}

async function snapshot(root) {
    const output = {};
    for (const item of await readdir(root, { recursive: true, withFileTypes: true })) {
        const path = `${item.parentPath}/${item.name}`;
        output[path.slice(root.length)] = item.isDirectory() ? "directory" : await readFile(path, "base64");
    }
    return output;
}

test("native resources preserve independent global/project SYSTEM precedence without package effects", async t => {
    const { root, cwd, agent } = await fixture(t);
    await writeFile(`${agent}/SYSTEM.md`, "\ufeffGLOBAL_SYSTEM");
    await writeFile(`${agent}/APPEND_SYSTEM.md`, "\ufeffGLOBAL_APPEND");
    await writeFile(`${cwd}/AGENTS.md`, "PROJECT_CONTEXT");
    await writeFile(`${agent}/settings.json`, JSON.stringify({ packages: ["npm:must-not-install"] }));
    await mkdir(`${agent}/extensions`);
    await writeFile(`${agent}/extensions/disabled.js`, "throw Error('extension must stay disabled')");
    const load = async () => {
        const before = await snapshot(root);
        const result = (await context({ task: { cwd } })).result.chunks;
        assert.deepEqual(await snapshot(root), before, "read-only context cannot install or rewrite resources");
        assert.equal(result.filter(c => c.source === "pi:project_context").find(c => c.path === `${cwd}/AGENTS.md`).content, "PROJECT_CONTEXT");
        return result;
    };
    let chunks = await load();
    assert.equal(chunks.find(c => c.metadata?.["pi/prompt"] === "system").content, "GLOBAL_SYSTEM");
    assert.equal(chunks.find(c => c.metadata?.["pi/prompt"] === "append").content, "GLOBAL_APPEND");
    await writeFile(`${cwd}/.pi/SYSTEM.md`, "PROJECT_SYSTEM");
    chunks = await load();
    assert.equal(chunks.find(c => c.metadata?.["pi/prompt"] === "system").content, "PROJECT_SYSTEM");
    assert.equal(chunks.find(c => c.metadata?.["pi/prompt"] === "append").content, "GLOBAL_APPEND");
    await writeFile(`${cwd}/.pi/APPEND_SYSTEM.md`, "PROJECT_APPEND");
    chunks = await load();
    assert.equal(chunks.find(c => c.metadata?.["pi/prompt"] === "append").content, "PROJECT_APPEND");
    assert.ok(!chunks.some(c => c.content.includes("GLOBAL_")));
});

test("Pi sampling uses native custom/append prompt sections once and keeps AGENTS separate", async t => {
    const { cwd, agent } = await fixture(t);
    await writeFile(`${agent}/SYSTEM.md`, "CUSTOM_SYSTEM_NATIVE");
    await writeFile(`${agent}/APPEND_SYSTEM.md`, "CUSTOM_APPEND_NATIVE");
    await writeFile(`${cwd}/AGENTS.md`, "PROJECT_CONTEXT_NATIVE");
    const text = (role, value) => canonical(role, { Text: { text: value } }, role === "User" ? "user" : "model");
    let sampled;
    const result = await workflow({ task: { cwd, text: "inspect" }, history: [text("User", "inspect")],
        runtime: { model_ref: { provider: "pi_codex", model: "gpt-6.1-sol" }, reasoning: {}, instructions: [] } }, {
        signal: new AbortController().signal,
        async hostCall(method, params) {
            if (method === "host.events.emit") return {};
            if (method === "host.context.build") return (await context(params)).result;
            if (method === "host.tools.select") return { tools: [] };
            if (method === "host.context.prepare") return { output: { messages: params.input.messages, changed: false }, target: null };
            if (method === "host.model.stream.start") { sampled = params.request; return { stream_id: "prompt" }; }
            if (method === "host.model.stream.next") return { type: "response", response: {
                messages: [text("Assistant", "done")], tool_calls: [], finish_reason: "Stop", usage: { input_tokens: 10, output_tokens: 1 },
            } };
            if (method === "host.history.checkpoint") return { revision: null };
            throw new Error(method);
        },
    });
    assert.equal(result.status, "success", JSON.stringify(result.failure));
    const prompt = sampled.instructions.at(-1).text;
    for (const marker of ["CUSTOM_SYSTEM_NATIVE", "CUSTOM_APPEND_NATIVE", "PROJECT_CONTEXT_NATIVE"])
        assert.equal(prompt.split(marker).length - 1, 1, marker);
    assert.ok(!prompt.includes("You are a coding assistant running inside pi"));
    assert.match(prompt, /<project_context>[\s\S]*PROJECT_CONTEXT_NATIVE/);
    assert.ok(!prompt.includes("<project_instructions path=\"" + agent + "/SYSTEM.md"));
});
