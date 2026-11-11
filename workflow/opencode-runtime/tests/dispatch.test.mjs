import { test } from "node:test";
import assert from "node:assert/strict";
import { toolBarrier } from "../dispatch.mjs";
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));

test("parallel calls overlap and serial calls fence both earlier and later calls",async()=>{
    const dispatch=toolBarrier([{name:"parallel",supports_parallel_tool_calls:true},{name:"serial",supports_parallel_tool_calls:false}],new AbortController().signal);
    let active=0,peak=0;
    const trace=[];
    const effect=(id,exclusive=false)=>async()=>{
        if(exclusive) assert.equal(active,0);
        trace.push(`start:${id}`);active++;peak=Math.max(peak,active);
        await delay(10);active--;trace.push(`end:${id}`);return id;
    };
    const results=await Promise.all([
        dispatch({name:"parallel"},effect("a")),dispatch({name:"parallel"},effect("b")),
        dispatch({name:"serial"},effect("fence",true)),dispatch({name:"parallel"},effect("later")),
    ]);
    assert.equal(peak,2);
    assert.ok(trace.indexOf("start:fence")>trace.indexOf("end:a"));
    assert.ok(trace.indexOf("start:fence")>trace.indexOf("end:b"));
    assert.ok(trace.indexOf("start:later")>trace.indexOf("end:fence"));
    assert.deepEqual(results,["a","b","fence","later"]);
});

test("cancellation while waiting never starts another effect",async()=>{
    const controller=new AbortController(),dispatch=toolBarrier([],controller.signal);
    let release;const running=dispatch({name:"first"},()=>new Promise(resolve=>{release=resolve;}));
    let late=false;const waiting=dispatch({name:"second"},()=>{late=true;});
    await delay(1);controller.abort(new Error("turn canceled"));release();await running;
    await assert.rejects(waiting,/turn canceled/);assert.equal(late,false);
});
