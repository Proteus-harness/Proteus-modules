import { test } from "node:test";
import assert from "node:assert/strict";
import { piMessages } from "../../../components/pi-runtime-js/messages.mjs";

test("Pi preserves canonical context mode, source, optional path and exact content", () => {
    const chunks = [
        { source: "search", path: null, content: "found", render_mode: "source_annotated", metadata: { render_mode: "verbatim" } },
        { source: "memory", path: "src/δ.rs", content: "line 1\r\nline 2\n", render_mode: "source_annotated", metadata: null },
        { source: "empty-path", path: "", content: "value", render_mode: "source_annotated", metadata: null },
        { source: "empty-content", path: null, content: "", render_mode: "source_annotated", metadata: null },
        { source: "private-label", path: "private-path", content: "exact\r\n  text\n", render_mode: "verbatim", metadata: { render_mode: "source_annotated" } },
        { source: "empty-verbatim", path: null, content: "", render_mode: "verbatim", metadata: null },
    ].map(chunk => ({ ...chunk, score: null }));
    const expected = ["Context from search:\nfound", "Context from memory (src/δ.rs):\nline 1\r\nline 2\n",
        "Context from empty-path ():\nvalue", "Context from empty-content:\n", "exact\r\n  text\n", ""];
    const input = [{ id: "user", role: "User", phase: null, name: null, tool_call_id: null, metadata: null,
        parts: [{ payload: { Text: { text: "before" } } },
            ...chunks.map(chunk => ({ payload: { Context: { chunk } } })),
            { payload: { Text: { text: "after" } } }] }];
    const original = structuredClone(input);
    for (const chunk of chunks) Object.freeze(chunk);
    assert.deepEqual(piMessages(input, {})[0].content.map(item => item.text), ["before", ...expected, "after"]);
    assert.deepEqual(input, original);
});
