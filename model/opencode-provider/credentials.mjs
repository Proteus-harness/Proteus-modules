// Read the pinned OpenCode v2 CredentialTable without a writer or migrations.
// SQLite owns WAL coordination sidecars; do not use immutable mode, which can
// hide recently committed credentials still present only in the WAL.
import { DatabaseSync } from "node:sqlite";
import { access } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import { Schema } from "../../workflow/opencode-runtime/node_modules/effect/dist/index.js";
import { Value } from "../../workflow/opencode-runtime/node_modules/@opencode/core/dist/credential.js";

export function credentialDatabasePath(value, environment = process.env) {
    const home = environment.HOME || homedir();
    if (value !== undefined && (typeof value !== "string" || !value.trim())) throw new Error("Invalid OpenCode database path");
    if (value === "~" || value === "$HOME" || value === "${HOME}") return home;
    for (const prefix of ["~/", "$HOME/", "${HOME}/"]) {
        if (value?.startsWith(prefix)) return join(home, value.slice(prefix.length));
    }
    if (value !== undefined) {
        return value;
    }
    const data = environment.XDG_DATA_HOME;
    return join(data && isAbsolute(data) ? data : join(home, ".local/share"), "opencode", "opencode.db");
}

function decodeCredential(value) {
    let credential;
    try { credential = Schema.decodeUnknownSync(Value)(JSON.parse(value)); }
    catch { throw new Error("Stored OpenCode credential has an unsupported format"); }
    if (credential.type !== "key") throw new Error("OpenCode OAuth requires native refresh; this model adapter accepts API-key credentials");
    if (!credential.key.trim()) throw new Error("Stored OpenCode API key is empty");
    return credential;
}

/** Native precedence: saved active/latest credential first, provider environment second. */
export function createCredentialReader({
    providerID, databasePath, apiKey, envNames: staticEnvNames = [], resolveEnvNames, environment = process.env,
}) {
    if (typeof providerID !== "string" || !providerID.trim()) throw new Error("OpenCode provider id is required");
    if (apiKey !== undefined && (typeof apiKey !== "string" || !apiKey.trim()))
        throw new Error("OpenCode explicit API key must be nonempty text");
    const path = credentialDatabasePath(databasePath, environment);
    return async (signal) => {
        signal?.throwIfAborted();
        // An explicit per-connection override never mutates the shared native
        // credential store. Omitting it preserves native saved-key/env precedence.
        if (apiKey !== undefined) return { type: "key", key: apiKey };
        let present = true;
        try { await access(path); }
        catch (error) {
            if (error.code === "ENOENT") present = false;
            else throw new Error("OpenCode credential database is unavailable");
        }
        signal?.throwIfAborted();
        if (present) {
            let db;
            let row;
            try {
                db = new DatabaseSync(path, { readOnly: true });
                row = db.prepare("SELECT value FROM credential WHERE integration_id = ? ORDER BY active DESC, time_created DESC, id DESC LIMIT 1").get(providerID);
            } catch { throw new Error("OpenCode credential database could not be read"); }
            finally { db?.close(); }
            signal?.throwIfAborted();
            if (row) return decodeCredential(row.value);
        }
        // Only an absent DB/row may consult the slower provider metadata source.
        let envNames = staticEnvNames;
        if (resolveEnvNames !== undefined) {
            signal?.throwIfAborted();
            const resolved = await resolveEnvNames(signal);
            signal?.throwIfAborted();
            envNames = resolved === undefined ? [] : resolved;
        }
        for (const name of envNames) {
            const key = environment[name];
            if (typeof key === "string" && key.trim()) return { type: "key", key };
        }
        throw new Error(`No OpenCode API-key credential for provider ${providerID}; connect the provider in OpenCode`);
    };
}
