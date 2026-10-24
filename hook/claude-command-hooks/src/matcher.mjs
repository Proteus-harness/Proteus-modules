import { Script } from "node:vm";

const expression = new Script("new RegExp(pattern).test(name)");

/** Claude's exact-list/regexp split, with explicit local resource limits. */
export function matcher(pattern) {
  if (pattern === undefined || pattern === "" || pattern === "*") return () => true;
  if (typeof pattern !== "string" || pattern.length > 1024) {
    throw new Error("matcher must be a string of at most 1024 characters");
  }
  if (/^[a-zA-Z0-9_\- ,|]+$/.test(pattern)) {
    const names = new Set(pattern.split(/[|,]/).map(name => name.trim()));
    return name => names.has(name);
  }
  new RegExp(pattern); // Reject syntax errors during inspection, before execution.
  return name => {
    if (typeof name !== "string" || name.length > 256) throw new Error("tool name exceeds matcher limit");
    // A trusted but accidentally pathological regexp must not hang the worker.
    return expression.runInNewContext({ pattern, name }, { timeout: 25 });
  };
}
