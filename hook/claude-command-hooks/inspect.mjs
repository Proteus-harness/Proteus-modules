#!/usr/bin/env node
import { inspectConfig, readJson } from "./src/config.mjs";

// Deliberately does not import entry.mjs/SDK or execute any selected command.
if (process.argv.length !== 3) {
  console.error("Usage: node inspect.mjs /absolute/path/adapter-settings.json");
  process.exitCode = 2;
} else {
  try {
    const { report } = await inspectConfig(await readJson(process.argv[2]));
    console.log(JSON.stringify(report, null, 2));
    process.exitCode = report.compatible ? 0 : 1;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
