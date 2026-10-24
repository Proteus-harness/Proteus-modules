import assert from "node:assert/strict";
import { test } from "node:test";
import { matcher } from "../src/matcher.mjs";

test("omitted, empty and wildcard matchers match every tool", () => {
  for (const pattern of [undefined, "", "*"]) {
    for (const name of ["Bash", "write_file", "mcp__example__read"]) {
      assert.equal(matcher(pattern)(name), true);
    }
  }
});

test("ordinary names and lists use exact case-sensitive matching", () => {
  assert.equal(matcher("Bash")("Bash"), true);
  assert.equal(matcher("Bash")("MyBash"), false);
  assert.equal(matcher("Bash")("BashExtra"), false);
  assert.equal(matcher("Bash")("bash"), false);
  for (const pattern of ["Edit|Write", "Edit, Write", " Edit | Write "]) {
    assert.equal(matcher(pattern)("Edit"), true);
    assert.equal(matcher(pattern)("Write"), true);
    assert.equal(matcher(pattern)("NotebookEdit"), false);
  }
  assert.equal(matcher("mcp__brave-search")("mcp__brave-search"), true);
  assert.equal(matcher("mcp__brave-search")("mcp__brave-search__query"), false);
});

test("explicit regex matcher follows unanchored JavaScript test semantics", () => {
  assert.equal(matcher("Edit.*")("NotebookEdit"), true);
  assert.equal(matcher("^Edit$")("NotebookEdit"), false);
  assert.equal(matcher("^Edit$")("Edit"), true);
  const match = matcher("mcp__memory__.*");
  assert.equal(match("mcp__memory__search"), true);
  assert.equal(match("mcp__memory__search"), true, "matcher must not retain state");
  assert.equal(match("mcp__other__search"), false);
});

test("invalid or unbounded matcher configuration fails explicitly", () => {
  for (const pattern of [null, 1, {}, [], "[", "x".repeat(1025)]) {
    assert.throws(() => matcher(pattern));
  }
});

test("pathological regular expressions cannot monopolize the worker", () => {
  const matches = matcher("^(a+)+$");
  assert.throws(() => matches(`${"a".repeat(100)}!`), /timed out|timeout/i);
});
