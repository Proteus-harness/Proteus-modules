import test from "node:test";
import assert from "node:assert/strict";
import { canonical } from "../../../components/module-runtime-js/canonical.mjs";
import { Schema } from "effect";
import { Input as NativeReadInput } from "@opencode/core/tool/plugin/read";
import { definition } from "@opencode/core/tool/runtime";
import { workflow } from "../workflow.mjs";
const request = {
    model: { provider: "openai_codex", model: "gpt-6.1-sol" },
    reasoning: { effort: "high", summary: true },
};
const message = (role, text) => canonical(role, { Text: { text } }, role === "User" ? "user" : "model");
const reply = (text) => ({
    messages: [message("Assistant", text)],
    tool_calls: [],
    finish_reason: "Stop",
    usage: { input_tokens: 100, output_tokens: 8 },
});
test("upstream loop dispatches the model call once through the bound registry and checkpoints before it", async () => {
    const history = [message("User", "read the file")],
        trace = [];
    const call = { id: "r1", name: "read", args: { path: "note", offset: "2", limit: null }, surface: "function", raw_arguments: null };
    const tool = {
        name: "read",
        description: "Read a file",
        input_schema: definition({ name: "read", input: NativeReadInput }).inputSchema,
    };
    assert.throws(() => Schema.decodeUnknownSync(NativeReadInput)(call.args));
    let steps = 0,
        tracked = [],
        sampled;
    const result = await workflow(
        {
            task: { cwd: "/work", text: "read the file" },
            history,
            runtime: {
                instructions: [],
                max_input_tokens: 128000,
                model_ref: request.model,
                reasoning: request.reasoning,
            },
        },
        {
            signal: new AbortController().signal,
            async hostCall(method, p) {
                trace.push(method);
                if (method === "host.context.build")
                    return {
                        chunks: [
                            {
                                source: "opencode:instructions",
                                content: "instructions",
                                render_mode: "verbatim",
                            },
                        ],
                    };
                if (method === "host.tools.select") return { tools: [tool] };
                if (method === "host.context.prepare")
                    return { output: { messages: p.input.messages, changed: false }, target: null };
                if (method === "host.model.stream.start") {
                    assert.ok(
                        p.request.messages[0].parts[0].payload.Context.chunk.content.includes("instructions"),
                    );
                    sampled =
                        ++steps === 1
                            ? {
                                  ...reply(""),
                                  messages: [canonical("Assistant", { ToolCall: { call } }, "model")],
                                  tool_calls: [call],
                                  finish_reason: "ToolCalls",
                              }
                            : reply("done");
                    if (steps === 2)
                        assert.equal(
                            p.request.messages.at(-1).parts[0].payload.ToolResult.result.output,
                            "test result",
                        );
                    return { stream_id: "stream" };
                }
                if (method === "host.model.stream.next") {
                    return { type: "response", response: sampled };
                }
                if (method === "host.history.checkpoint") {
                    tracked = p.tool_results;
                    if (tracked.length) {
                        assert.deepEqual(tracked[0].execution_call.args, { path: "note", offset: 2 });
                        assert.deepEqual(JSON.parse(tracked[0].execution_call.raw_arguments), { path: "note", offset: 2 });
                        Schema.decodeUnknownSync(NativeReadInput)(tracked[0].execution_call.args);
                    }
                    return { revision: null };
                }
                if (method === "host.tools.execute") {
                    assert.equal(tracked[0].execution_call.id, p.call.id);
                    assert.deepEqual(p.call.args, { path: "note", offset: 2 });
                    return {
                        call_id: p.call.id,
                        ok: true,
                        output: "test result",
                        error: null,
                        content: [],
                        metadata: null,
                    };
                }
                if (method === "host.events.emit") return {};
                throw Error(method);
            },
        },
    );
    assert.equal(result.status, "success", JSON.stringify(result.failure));
    assert.equal(result.result.output.text, "done");
    assert.equal(steps, 2);
    assert.equal(trace.filter((m) => m === "host.tools.execute").length, 1);
});

test("native step fan-out respects serial specs and pre-effect checkpoint identities", async () => {
    const calls = [1,2,3].map(i => ({id:`serial-${i}`,name:"read",args:{path:`note-${i}`},surface:"function",raw_arguments:null}));
    const tool = {name:"read",description:"native read",input_schema:definition({name:"read",input:NativeReadInput}).inputSchema,supports_parallel_tool_calls:false};
    let sampled, steps=0, active=0, peak=0;
    const bindings=new Map(), trace=[], completed=[];
    const output=await workflow({task:{cwd:"/work",text:"read three files"},history:[message("User","read")],runtime:{instructions:[],max_input_tokens:128000,model_ref:request.model,reasoning:request.reasoning}}, {
        signal:new AbortController().signal,
        async hostCall(method,p) {
            if(method==="host.events.emit") return {};
            if(method==="host.context.build") return {chunks:[],token_estimate:null};
            if(method==="host.tools.select") return {tools:[tool]};
            if(method==="host.context.prepare") return {output:{messages:p.input.messages,changed:false},target:null};
            if(method==="host.model.stream.start") {
                sampled=++steps===1 ? {...reply(""),messages:[{...message("Assistant",""),parts:calls.map(call=>({part_id:call.id,provenance:"model",scope:"conversation",payload:{ToolCall:{call}}}))}],tool_calls:calls,finish_reason:"ToolCalls"} : reply("done");
                return {stream_id:"serial-stream"};
            }
            if(method==="host.model.stream.next") return {type:"response",response:sampled};
            if(method==="host.history.checkpoint") {
                trace.push("checkpoint");
                for(const binding of p.tool_results) bindings.set(binding.call_id,binding);
                return {revision:null};
            }
            if(method==="host.tools.execute") {
                assert.equal(bindings.size,3,"all identities checkpointed before any effect");
                assert.ok(bindings.get(p.call.id).message_id);
                assert.ok(bindings.get(p.call.id).part_id);
                assert.deepEqual(bindings.get(p.call.id).execution_call,p.call);
                trace.push(`start:${p.call.id}`); active++; peak=Math.max(peak,active);
                await new Promise(resolve=>setTimeout(resolve,15));
                active--; completed.push(p.call.id); trace.push(`end:${p.call.id}`);
                return {call_id:p.call.id,ok:true,output:p.call.id,error:null,content:[],metadata:null};
            }
            throw new Error(method);
        },
    });
    assert.equal(output.status,"success",JSON.stringify(output.failure));
    assert.equal(peak,1);
    assert.deepEqual(completed,calls.map(call=>call.id));
    assert.ok(trace.indexOf("checkpoint")<trace.indexOf("start:serial-1"));
    const results=output.result.new_messages.flatMap(m=>m.parts.filter(p=>p.payload.ToolResult).map(p=>({message:m.id,part:p.part_id,result:p.payload.ToolResult.result})));
    for(const entry of results) {
        assert.equal(entry.message,bindings.get(entry.result.call_id).message_id);
        assert.equal(entry.part,bindings.get(entry.result.call_id).part_id);
    }
    assert.equal(results.length,3);
});
