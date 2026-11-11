import { test } from "node:test";
import assert from "node:assert/strict";
import { make, render } from "@opencode/core/session/system-prompt";
import { nativeSystemPrompt, createPromptExport } from "../prompt.mjs";

test("native generic template dynamically reflects actual tool names", async () => {
    const model = { provider: "test", model: "unmatched-model" };
    for (const names of [[], ["shell"], ["read", "edit", "write", "shell"]]) {
        assert.equal(await nativeSystemPrompt(model, names), make(names));
    }
    assert.ok(!make([]).includes("oldString"));
    assert.ok(make(["edit"]).includes("oldString"));
});

test("actual upstream provider plugins select GPT, Astra and Anthropic instructions", async () => {
    const tools = ["shell", "read", "edit", "write"];
    const base = make(tools);
    const gpt = await nativeSystemPrompt({ provider: "openai", model: "gpt-6.1-sol" }, tools);
    const astra = await nativeSystemPrompt({ provider: "openai", model: "gpt-6-astra" }, tools);
    const claude = await nativeSystemPrompt({ provider: "anthropic", model: "claude-sonnet-5-5" }, tools);
    assert.notEqual(gpt, base);
    assert.notEqual(astra, gpt);
    assert.ok(claude.startsWith(base));
    assert.ok(claude.includes("# Code comments"));
});

test("upstream honors an explicit agent system override", async () => {
    const tools = ["shell", "edit"];
    const custom = "Agent system ${OPENCODE_TOOL_GUIDANCE}";
    const result = await nativeSystemPrompt({ provider: "openai", model: "gpt-6.1-sol" }, tools, custom);
    // Native optimize skips configured agent.system entirely.
    assert.equal(result, custom);
    assert.notEqual(result, render(custom, tools));
});

test("hook applies actual prompt to the current request model and tool set", async () => {
    const hook = createPromptExport({ slot: "hook", module_id: "opencode_prompt", contract_version: "v6", composition: "ordered_many", module_config: {} });
    const request = { model: { provider: "openai", model: "gpt-6.1-sol" }, messages: [], instructions: [], tools: [{ name: "shell" }] };
    const output = await hook.invoke("hook.invoke", { event: { event: "before_model", request } }, { signal: new AbortController().signal });
    assert.equal(output.result.instructions[0].text, await nativeSystemPrompt(request.model, ["shell"]));
    assert.deepEqual(output.result.messages, request.messages);
    assert.deepEqual(await hook.invoke("hook.invoke", { event: { event: "turn_started" } }, {}), { result: { action: "continue" } });
});
