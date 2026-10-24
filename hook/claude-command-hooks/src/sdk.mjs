import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

export async function loadSdk(root) {
  const pin = JSON.parse(await readFile(new URL("../sdk-pin.json", import.meta.url), "utf8"));
  for (const [name, expected] of Object.entries(pin.sha256)) {
    const actual = createHash("sha256").update(await readFile(join(root, name))).digest("hex");
    if (actual !== expected) throw new Error(`SDK pin mismatch: ${name}; expected Core ${pin.revision}`);
  }
  const command = await import(pathToFileURL(join(root, "command.mjs")).href);
  const ports = await import(pathToFileURL(join(root, "ports.mjs")).href);
  return { ...command, ...ports };
}
