import { open } from "node:fs/promises";
import { constants } from "node:fs";
import { isAbsolute } from "node:path";
import { matcher } from "./matcher.mjs";

export const MAX_CONFIG_BYTES = 1_048_576;
const supportedEvents = new Set(["PreToolUse", "Stop"]);
const own = (value, key) => Object.hasOwn(value, key);
export const object = value => value !== null && typeof value === "object" && !Array.isArray(value);
const keyName = value => typeof value === "string" && /^[A-Za-z_][A-Za-z0-9_]*$/.test(value)
  && !["__proto__", "prototype", "constructor"].includes(value);

export async function readJson(path) {
  const file = await open(path, constants.O_RDONLY | constants.O_NONBLOCK);
  try {
    if (!(await file.stat()).isFile()) throw new Error("configuration must be a regular file");
    const buffer = Buffer.alloc(MAX_CONFIG_BYTES + 1);
    let length = 0;
    while (length < buffer.length) {
      const { bytesRead } = await file.read(buffer, length, buffer.length - length);
      if (!bytesRead) break;
      length += bytesRead;
    }
    if (length > MAX_CONFIG_BYTES) throw new Error("configuration exceeds 1 MiB");
    return JSON.parse(buffer.subarray(0, length).toString("utf8"));
  } finally { await file.close(); }
}

/** Read and classify explicit configuration. Never imports code or runs commands. */
export async function inspectConfig(settings) {
  const diagnostics = [];
  const report = { compatible: false, mode: "strict_subset", diagnostics, hooks: [] };
  const problem = (path, message, level = "error") => diagnostics.push({ level, path, message });
  const plan = { settings, handlers: [], mappings: new Map() };
  if (!object(settings)) {
    problem("settings", "settings must be an object");
    return { report, plan };
  }
  const allowed = new Set(["mode", "allow_execution", "config_path", "sdk_root", "plugin_root", "events", "tool_mappings"]);
  for (const key of Object.keys(settings)) if (!allowed.has(key)) problem(`settings.${key}`, "unsupported adapter setting");
  if (settings.mode !== "strict_subset") problem("settings.mode", "must explicitly select strict_subset (not full Claude compatibility)");
  if (own(settings, "allow_execution") && typeof settings.allow_execution !== "boolean") problem("settings.allow_execution", "must be boolean");
  for (const key of ["config_path", "sdk_root", "plugin_root"]) {
    if (key === "plugin_root" && !own(settings, key)) continue;
    if (typeof settings[key] !== "string" || !isAbsolute(settings[key]) || settings[key].includes("\0")) problem(`settings.${key}`, "must be an explicit absolute path");
  }
  if (!Array.isArray(settings.events) || !settings.events.length || settings.events.length > 2
      || new Set(settings.events).size !== settings.events.length
      || settings.events.some(event => !supportedEvents.has(event))) {
    problem("settings.events", "select a nonempty unique subset of PreToolUse and Stop");
  }
  const mappings = own(settings, "tool_mappings") ? settings.tool_mappings : {};
  if (!object(mappings) || Object.keys(mappings).length > 128) problem("settings.tool_mappings", "must be an object with at most 128 mappings");
  else for (const [nativeName, mapping] of Object.entries(mappings)) {
    const path = `settings.tool_mappings.${nativeName}`;
    if (!nativeName.trim() || nativeName.length > 256 || !object(mapping)
        || Object.keys(mapping).some(key => !["name", "arguments"].includes(key))
        || typeof mapping.name !== "string" || !mapping.name.trim() || mapping.name.length > 256
        || (own(mapping, "arguments") && !object(mapping.arguments))) {
      problem(path, "expected {name, arguments?: {native_key: foreign_key}}"); continue;
    }
    const entries = Object.entries(mapping.arguments ?? {});
    if (entries.length > 128 || entries.some(([from, to]) => !keyName(from) || !keyName(to))
        || new Set(entries.map(([, to]) => to)).size !== entries.length) {
      problem(path, "argument names must be safe, unique top-level renames"); continue;
    }
    plan.mappings.set(nativeName, { name: mapping.name, arguments: entries });
  }
  if (diagnostics.some(item => item.level === "error")) return { report, plan };
  let source;
  try { source = await readJson(settings.config_path); }
  catch (error) { problem("config_path", error.message); return { report, plan }; }
  if (!object(source) || !object(source.hooks)) {
    problem("hooks", "expected a hooks.json/settings.json object with an object-valued hooks field");
    return { report, plan };
  }
  for (const key of Object.keys(source)) if (!["hooks", "description", "disableAllHooks"].includes(key)) {
    problem(key, "not imported: only the explicit hooks field is used", "warning");
  }
  if (own(source, "disableAllHooks") && typeof source.disableAllHooks !== "boolean") problem("disableAllHooks", "must be boolean");
  if (source.disableAllHooks === true) {
    problem("disableAllHooks", "hooks disabled by the selected file", "warning");
    report.compatible = true;
    return { report, plan };
  }
  let count = 0;
  for (const [event, groups] of Object.entries(source.hooks)) {
    if (!settings.events.includes(event)) {
      problem(`hooks.${event}`, "not selected; no handlers imported", "warning"); continue;
    }
    if (!Array.isArray(groups) || groups.length > 64) { problem(`hooks.${event}`, "expected at most 64 matcher groups"); continue; }
    for (const [index, group] of groups.entries()) {
      const path = `hooks.${event}[${index}]`;
      if (!object(group) || Object.keys(group).some(key => !["matcher", "hooks"].includes(key))
          || !Array.isArray(group.hooks) || !group.hooks.length || group.hooks.length > 64) {
        problem(path, "expected {matcher?, hooks: nonempty array}; unsupported group fields"); continue;
      }
      let matches;
      try {
        matches = event === "Stop" ? () => true : matcher(group.matcher);
        if (event === "Stop" && own(group, "matcher")) problem(`${path}.matcher`, "Stop ignores matcher, as upstream does", "warning");
      } catch (error) { problem(`${path}.matcher`, error.message); continue; }
      for (const [number, hook] of group.hooks.entries()) {
        const hookPath = `${path}.hooks[${number}]`;
        if (++count > 64) { problem(hookPath, "at most 64 selected handlers are supported"); break; }
        if (!object(hook) || hook.type !== "command") { problem(hookPath, "only synchronous type=command is supported"); continue; }
        const keys = new Set(["type", "command", "args", "timeout", "shell", "async", "asyncRewake"]);
        const unsupported = Object.keys(hook).filter(key => !keys.has(key));
        if (unsupported.length) { problem(hookPath, `unsupported command fields: ${unsupported.join(", ")}`); continue; }
        if (typeof hook.command !== "string" || !hook.command.trim() || hook.command.length > 65_536 || hook.command.includes("\0")) {
          problem(`${hookPath}.command`, "expected a nonblank command of at most 65536 characters"); continue;
        }
        if (["async", "asyncRewake"].some(key => own(hook, key) && hook[key] !== false)) {
          problem(hookPath, "background hooks are not supported"); continue;
        }
        if (own(hook, "args") && (!Array.isArray(hook.args) || hook.args.length > 128
            || hook.args.some(arg => typeof arg !== "string" || arg.length > 65_536 || arg.includes("\0")))) {
          problem(`${hookPath}.args`, "expected at most 128 bounded string arguments"); continue;
        }
        if (own(hook, "args") && (hook.command.startsWith("-") || hook.command.includes("="))) {
          problem(`${hookPath}.command`, "exec executable must not begin with '-' or contain '=' (env launcher boundary)"); continue;
        }
        if (own(hook, "shell") && hook.shell !== "bash") { problem(`${hookPath}.shell`, "only bash/default POSIX shell is supported"); continue; }
        const timeout = own(hook, "timeout") ? hook.timeout : 600;
        if (!Number.isFinite(timeout) || timeout <= 0 || timeout > 600) { problem(`${hookPath}.timeout`, "timeout must be > 0 and <= 600 seconds"); continue; }
        const texts = [hook.command, ...(hook.args ?? [])];
        if (texts.some(text => /\$\{user_config\.|\$\{?CLAUDE_PLUGIN_DATA\b/.test(text))) {
          problem(hookPath, "user_config and CLAUDE_PLUGIN_DATA are not provided"); continue;
        }
        if (!settings.plugin_root && texts.some(text => /\$\{?CLAUDE_PLUGIN_ROOT\b/.test(text))) {
          problem(hookPath, "CLAUDE_PLUGIN_ROOT requires explicit plugin_root"); continue;
        }
        plan.handlers.push({ event, matches, hook: structuredClone(hook), timeout, path: hookPath });
        report.hooks.push({ event, matcher: group.matcher ?? "*", command: hook.command, args: hook.args, timeout });
      }
    }
  }
  if (plan.handlers.filter(handler => handler.event === "Stop").length > 1) {
    problem("hooks.Stop", "multiple Stop handlers always overlap; strict_subset supports only one matching handler");
  }
  if (plan.handlers.filter(handler => handler.event === "PreToolUse").length > 1) {
    problem("hooks.PreToolUse", "matcher overlap is checked at invocation before any command runs", "warning");
  }
  report.compatible = !diagnostics.some(item => item.level === "error");
  return { report, plan };
}
