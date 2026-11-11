import test from 'node:test';
import assert from 'node:assert/strict';
import { executionCall } from '../../../workflow/pi-runtime/tool-arguments.mjs';
import { createCodingToolDefinitions, validateToolArguments } from '../../../workflow/pi-runtime/upstream.mjs';

const tools = createCodingToolDefinitions('/tmp');
function native(tool, args) {
    const clone = structuredClone(args);
    return validateToolArguments(tool, {name:tool.name, arguments:tool.prepareArguments ? tool.prepareArguments(clone) : clone});
}

test('checkpoint execution binding equals actual native coercion and keeps raw sample', () => {
    const read = tools.find(t => t.name === 'read');
    for (const args of [{path:'a',offset:null,limit:null}, {path:'a',offset:'3',limit:'10'}]) {
        const call = {id:'same',name:'read',args:structuredClone(args),surface:'function',raw_arguments:null};
        const prepared = executionCall(call,read);
        assert.deepEqual(prepared.args,native(read,args));
        assert.deepEqual(call.args,args);
        assert.equal(prepared.id,call.id);
    }
});

test('binding uses native edit argument preparation before native validation', () => {
    const edit = tools.find(t => t.name === 'edit');
    for (const args of [{path:'a',oldText:'old',newText:'new'}, {path:'a',edits:JSON.stringify([{oldText:'old',newText:'new'}])}]) {
        const call = {id:'same',name:'edit',args:structuredClone(args)};
        assert.deepEqual(executionCall(call,edit).args,native(edit,args));
        assert.deepEqual(call.args,args);
    }
});

test('invalid args are retained for upstream validation failure, never invented', () => {
    const read = tools.find(t => t.name === 'read');
    const call = {id:'same',name:'read',args:{offset:2}};
    assert.throws(() => native(read,call.args),/Validation failed/);
    assert.equal(executionCall(call,read),call);
});
