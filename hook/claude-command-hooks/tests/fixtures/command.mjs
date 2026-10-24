// Original fixture: no copied or downloaded upstream hook code is executed.
import { writeFile } from "node:fs/promises";

let text = "";
for await (const chunk of process.stdin) text += chunk;
const input = JSON.parse(text);
const [mode, value] = process.argv.slice(2);
const deny = (reason) => console.log(JSON.stringify({ hookSpecificOutput: {
  hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: reason,
} }));

switch (mode) {
  case "allow":
    console.log(JSON.stringify({ hookSpecificOutput: {
      hookEventName: "PreToolUse", permissionDecision: "allow",
    } }));
    break;
  case "echo": deny(JSON.stringify(input)); break;
  case "env": deny(JSON.stringify({
    root: process.env.CLAUDE_PLUGIN_ROOT, project: process.env.CLAUDE_PROJECT_DIR,
  })); break;
  case "deny": deny(value ?? "fixture denied"); break;
  case "exit2": process.stderr.write(`${value ?? "fixture denied"}\n`); process.exitCode = 2; break;
  case "rewrite":
    console.log(JSON.stringify({ hookSpecificOutput: {
      hookEventName: "PreToolUse", permissionDecision: "allow", updatedInput: JSON.parse(value),
    } }));
    break;
  case "stop": console.log(JSON.stringify({ decision: "block", reason: JSON.stringify(input) })); break;
  case "raw": console.log(value); break;
  case "marker": await writeFile(value, "executed"); break;
  case "sleep": await new Promise((resolve) => setTimeout(resolve, Number(value))); break;
  case "delayed-marker": await new Promise((resolve) => setTimeout(resolve, 250)); await writeFile(value, "late"); break;
  case "overflow": process.stdout.write("x".repeat(1_048_577)); break;
  case "error": process.stderr.write("fixture failure"); process.exitCode = 1; break;
  default: throw new Error(`Unknown fixture mode: ${mode}`);
}
