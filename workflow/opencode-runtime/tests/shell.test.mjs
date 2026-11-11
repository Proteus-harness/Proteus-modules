import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { executeNativeFile, nativeFileDefinitions } from "../files.mjs";
const base = resolve("../../target/opencode-shell-fixtures");
process.env.PROTEUS_OPENCODE_RUNTIME_DIR = `${base}/runtime`;
process.env.OPENCODE_TEST_HOME = base;
await mkdir(base, { recursive: true });
async function fixture(t) { const cwd = await mkdtemp(`${base}/shell-`); await mkdir(`${cwd}/.git`);
    t.after(() => rm(cwd, { recursive:true, force:true })); return cwd; }
function run(cwd, command, args = {}, signal) {
    return executeNativeFile({cwd, call:{id:"shell-test",name:"shell",args:{command,...args}},attribution:{execution_id:"shell-test",agent:null}}, signal);
}

test("native shell schema/description/env/combined output and exit status", async t => {
    const cwd = await fixture(t);
    const shell = (await nativeFileDefinitions(cwd, true)).find(tool => tool.name === "shell");
    assert.equal(shell.safety, "RunsCommands");
    assert.match(shell.description, /Linux using bash/);
    assert.equal(shell.input_schema.properties.timeout.type, "integer");
    const result = await run(cwd, "printf '%s:%s:%s:%s' \"$AGENT\" \"$OPENCODE\" \"$AI_AGENT\" \"$OPENCODE_SESSION_ID\"; printf stderr >&2; exit 7");
    assert.equal(result.ok, true, result.error);
    assert.match(result.output, /^1:1:opencode:shell-teststderr/);
    assert.match(result.output, /7/);
    assert.equal(result.metadata["opencode/output"].exit, 7);
    assert.deepEqual(result.metadata["opencode/synthetic"], []);
});

test("upstream foreground timeout returns actual native timeout notice", async t => {
    const cwd = await fixture(t);
    const result = await run(cwd, "sleep 2", {timeout:20});
    assert.equal(result.ok, true, result.error);
    assert.equal(result.metadata["opencode/output"].timeout, true);
    assert.match(result.output, /exceeded timeout of 20 ms/);
});

test("background gate and cancellation prevent late side effects", async t => {
    const cwd = await fixture(t);
    const background = await run(cwd, "touch background-bad", {background:true});
    assert.equal(background.ok, false);
    assert.match(background.error, /background shell jobs/);
    await assert.rejects(readFile(`${cwd}/background-bad`));
    const controller = new AbortController();
    const pending = run(cwd, "sleep .2; touch canceled-bad", {}, controller.signal);
    setTimeout(() => controller.abort(new Error("test cancel")), 30);
    const result = await pending;
    assert.equal(result.ok, false);
    await new Promise(resolve => setTimeout(resolve, 300));
    await assert.rejects(readFile(`${cwd}/canceled-bad`));
});

test("native grep/glob run real ripgrep and preserve path/count/truncation output", async t => {
    const cwd = await fixture(t);
    await run(cwd, "printf 'needle\\nother\\nneedle\\n' > sample.txt");
    for (const [name,args,pattern] of [["glob",{pattern:"*.txt"},/sample\.txt/],["grep",{pattern:"needle",limit:1},/Found 1 matches/]]) {
        const result = await executeNativeFile({cwd,call:{id:"search",name,args},attribution:{execution_id:"search",agent:null}});
        assert.equal(result.ok,true,result.error);
        assert.match(result.output,pattern);
        if (name === "grep") assert.equal(result.metadata["opencode/metadata"].truncated,true);
    }
});
