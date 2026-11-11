import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { nativeInitialContext } from "../context.mjs";
import { nativeLocation } from "../location.mjs";

const base = resolve("../../target/opencode-context-fixtures");
process.env.PROTEUS_OPENCODE_RUNTIME_DIR = `${base}/runtime`;
process.env.OPENCODE_TEST_HOME = base;
await mkdir(base, { recursive:true });

test("actual native instruction plugin loads project AGENTS only and native date/env", async t => {
    const cwd = await mkdtemp(`${base}/context-`);
    t.after(() => rm(cwd, { recursive:true, force:true }));
    await mkdir(`${cwd}/.git`);
    await writeFile(`${cwd}/AGENTS.md`, "Use project rules.\n");
    await writeFile(`${cwd}/CLAUDE.md`, "must not be loaded by vanilla discovery\n");
    const output = await nativeInitialContext(cwd);
    assert.match(output.text, /^Today's date: /);
    assert.ok(output.text.includes(`  Working directory: ${cwd}`));
    assert.ok(output.text.includes("Is directory a git repo: yes"));
    assert.ok(output.text.endsWith(`Instructions from: ${cwd}/AGENTS.md\nUse project rules.\n`));
    assert.ok(!output.text.includes("must not be loaded"));
    assert.deepEqual(output.values["core/instructions"], [{ path:`${cwd}/AGENTS.md`, content:"Use project rules.\n" }]);
});

test("native project root discovery and instruction order use root to current directory", async t => {
    const root = await mkdtemp(`${base}/nested-`);
    t.after(() => rm(root, { recursive:true, force:true }));
    const cwd = `${root}/subdir`;
    await mkdir(`${root}/.git`);
    await mkdir(cwd);
    await writeFile(`${root}/AGENTS.md`, "root-rule");
    await writeFile(`${cwd}/AGENTS.md`, "nested-rule");
    assert.equal((await nativeLocation(cwd)).project.directory, root);
    const output = await nativeInitialContext(cwd);
    const paths = output.values["core/instructions"].map(file => file.path);
    // The native discovery plugin emits cwd first, then walks ancestors.
    assert.deepEqual(paths, [`${cwd}/AGENTS.md`,`${root}/AGENTS.md`]);
    assert.ok(output.text.includes(`Workspace root folder: ${root}`));
});
