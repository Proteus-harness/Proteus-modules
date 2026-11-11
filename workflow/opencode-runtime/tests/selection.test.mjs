import { test } from "node:test";
import assert from "node:assert/strict";
import { selectNativeTools } from "../selection.mjs";
const specs = ["read","write","edit","patch","shell","grep","glob"].map(name => ({name}));
test("actual native patch hook condition selects GPT patch vs other model edit/write", async () => {
    for (const model of ["gpt-6.1-sol","gpt-6.1-terra","gpt-6-astra"]) {
        assert.deepEqual((await selectNativeTools({model},specs)).map(spec => spec.name), ["read","patch","shell","grep","glob"]);
    }
    for (const model of ["deepseek-v4.1-flash","gpt-oss-120b","gpt-4.1","claude-sonnet-5.5"]) {
        assert.deepEqual((await selectNativeTools({model},specs)).map(spec => spec.name), ["read","write","edit","shell","grep","glob"]);
    }
    assert.equal(specs.length,7);
});
