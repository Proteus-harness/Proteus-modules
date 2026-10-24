import { inspectConfig, object } from "./config.mjs";
import { loadSdk } from "./sdk.mjs";

function renameArguments(value, entries, reverse = false) {
  if (!object(value)) throw new Error("tool input/updatedInput must be an object");
  const names = new Map(entries.map(([from, to]) => reverse ? [to, from] : [from, to]));
  const destinations = new Set(names.values());
  const renamed = Object.create(null);
  for (const [key, item] of Object.entries(value)) {
    if (destinations.has(key) && !names.has(key)) throw new Error(`ambiguous argument mapping key: ${key}`);
    const target = names.get(key) ?? key;
    if (Object.hasOwn(renamed, target)) throw new Error(`argument mapping collision: ${target}`);
    Object.defineProperty(renamed, target, { value: item, enumerable: true, writable: true, configurable: true });
  }
  return renamed;
}

function decision(sdk, event, result) {
  if (result.code === 0 && result.stdout.trim()) {
    const output = JSON.parse(result.stdout);
    if (!object(output)) throw new Error("hook output must be a JSON object");
    // Useful existing scripts emit this explicit no-op. Never drop continue:false.
    if (Object.hasOwn(output, "continue")) {
      if (output.continue !== true) throw new Error("continue:false is unsupported by strict_subset");
      delete output.continue;
    }
    if (Object.hasOwn(output, "hookSpecificOutput")) {
      const specific = output.hookSpecificOutput;
      if (!object(specific)) throw new Error("hookSpecificOutput must be an object");
      if (Object.hasOwn(specific, "permissionDecisionReason") && typeof specific.permissionDecisionReason !== "string") {
        throw new Error("permissionDecisionReason must be a string");
      }
      if (Object.hasOwn(specific, "updatedInput") && !object(specific.updatedInput)) {
        throw new Error("updatedInput must be an object");
      }
    }
    result = { ...result, stdout: JSON.stringify(output) };
  }
  return event === "PreToolUse" ? sdk.preToolUseDecision(result) : sdk.stopDecision(result);
}

function commandArguments(handler, settings, cwd) {
  const variables = { CLAUDE_PROJECT_DIR: cwd, CLAUDE_PLUGIN_ROOT: settings.plugin_root };
  const substitute = text => text.replace(/\$\{(CLAUDE_PROJECT_DIR|CLAUDE_PLUGIN_ROOT)\}/g, (_, key) => {
    if (variables[key] === undefined) throw new Error(`${key} is unavailable`);
    return variables[key];
  });
  const { hook } = handler;
  // A literal env argv, not command-string interpolation or process.env mutation.
  const args = ["-u", "CLAUDE_PLUGIN_ROOT", "-u", "CLAUDE_PLUGIN_DATA", "--",
    `CLAUDE_PROJECT_DIR=${cwd}`];
  if (settings.plugin_root) args.push(`CLAUDE_PLUGIN_ROOT=${settings.plugin_root}`);
  if (Object.hasOwn(hook, "args")) {
    const executable = substitute(hook.command);
    if (executable.startsWith("-") || executable.includes("=")) {
      throw new Error("expanded exec executable must not begin with '-' or contain '=' (env launcher boundary)");
    }
    args.push(executable, ...hook.args.map(substitute));
  } else args.push(hook.shell === "bash" ? "/bin/bash" : "/bin/sh", "-c", hook.command);
  return args;
}

async function execute(handler, plan, sdk, payload, ctx) {
  ctx.signal.throwIfAborted();
  if (Buffer.byteLength(JSON.stringify(payload)) > 1_048_576) throw new Error("hook input exceeds 1 MiB");
  const deadline = new AbortController();
  const timer = setTimeout(() => deadline.abort(new Error(`command hook timeout after ${handler.timeout}s`)), handler.timeout * 1000);
  timer.unref();
  try {
    const result = await sdk.runCommand("/usr/bin/env", commandArguments(handler, plan.settings, ctx.cwd), payload,
      { cwd: ctx.cwd, signal: AbortSignal.any([ctx.signal, deadline.signal]) });
    return decision(sdk, handler.event, result);
  } finally { clearTimeout(timer); }
}

/** Existing Core hook-process entry API; setup reads configuration, never commands. */
export default async function setup(hooks) {
  const { report, plan } = await inspectConfig(hooks.config);
  if (!report.compatible) throw new Error(`incompatible command hooks: ${report.diagnostics.filter(item => item.level === "error").map(item => `${item.path}: ${item.message}`).join("; ")}`);
  if (hooks.config.allow_execution !== true) throw new Error("command hooks require explicit allow_execution=true; use inspect.mjs first");
  if (process.platform === "win32") throw new Error("strict_subset command hooks currently require POSIX");
  const sdk = await loadSdk(hooks.config.sdk_root);
  for (const item of report.diagnostics) console.error(`[claude-command-hooks] ${item.path}: ${item.message}`);

  async function invoke(eventName, event, ctx) {
    if (eventName === "PreToolUse" && event.blocked) return;
    const mapping = eventName === "PreToolUse" ? plan.mappings.get(event.call.name) : undefined;
    const name = mapping?.name ?? event.call?.name;
    const handlers = plan.handlers.filter(handler => handler.event === eventName && handler.matches(name));
    if (handlers.length > 1) throw new Error("strict_subset supports only one matching handler per event; parallel decision aggregation requires a separate port");
    if (!handlers.length) return;
    const payload = {
      hook_event_name: eventName, cwd: ctx.cwd,
      session_id: ctx.attribution.agent?.session_id ?? null,
      turn_id: ctx.attribution.agent?.turn_id ?? null,
    };
    if (eventName === "PreToolUse") Object.assign(payload, {
      tool_name: name, tool_use_id: event.call.id,
      tool_input: renameArguments(event.call.args, mapping?.arguments ?? []),
    });
    else Object.assign(payload, {
      stop_hook_active: event.attempt > 0, last_assistant_message: event.output.text,
    });
    const response = await execute(handlers[0], plan, sdk, payload, ctx);
    if (response?.action === "tool_arguments") response.args = renameArguments(response.args, mapping?.arguments ?? [], true);
    return response;
  }
  if (plan.handlers.some(handler => handler.event === "PreToolUse")) hooks.on("before_tool", (event, ctx) => invoke("PreToolUse", event, ctx));
  if (plan.handlers.some(handler => handler.event === "Stop")) hooks.on("before_stop", (event, ctx) => invoke("Stop", event, ctx));
}
