import { test } from "node:test";
import assert from "node:assert/strict";
import { canonicalAnswer } from "../../../model/pi-provider/adapter.mjs";
import { piMessages } from "../../../components/pi-runtime-js/messages.mjs";

test("canonical content owns the text and arguments while Pi provider signatures round-trip", () => {
    const original = {
        role: "assistant",
        provider: "opencode-go",
        model: "deepseek-v4.1-flash",
        api: "openai-completions",
        stopReason: "toolUse",
        timestamp: 1,
        responseId: "resp",
        endTurn: false,
        usage: { input: 8, cacheRead: 2, cacheWrite: 0, output: 12, reasoning: 4, totalTokens: 22 },
        content: [
            { type: "thinking", thinking: "a thought", thinkingSignature: "signature", redacted: true },
            { type: "text", text: "a very long reply", textSignature: "phase" },
            {
                type: "toolCall",
                id: "one",
                name: "edit",
                arguments: { path: "test.py", code: "new implementation" },
                thoughtSignature: "call-sig",
            },
        ],
    };
    const response = canonicalAnswer(original, "message");
    const [message] = response.messages;
    const metadata = JSON.stringify(message.metadata);
    assert.ok(!metadata.includes("new implementation"));
    assert.ok(!metadata.includes("a thought"));
    assert.equal(response.end_turn, false);
    assert.deepEqual(piMessages([message], {})[0], original);
    // Accept records from the first release without treating their visible
    // payload as more authoritative than the Core message.
    message.metadata.pi_native = structuredClone(original);
    message.parts[1].payload.Text.text = "updated";
    assert.equal(piMessages([message], {})[0].content[1].text, "updated");
});
