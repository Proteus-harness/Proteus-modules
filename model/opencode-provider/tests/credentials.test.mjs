import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { credentialDatabasePath, createCredentialReader } from "../credentials.mjs";

const signal = () => new AbortController().signal;
async function fixture(fn) {
    const root = await mkdtemp(join(tmpdir(), "proteus-opencode-credentials-"));
    const path = join(root, "native.db");
    const db = new DatabaseSync(path);
    db.exec("CREATE TABLE credential(id TEXT PRIMARY KEY,integration_id TEXT,value TEXT,active INTEGER,time_created INTEGER)");
    const insert = (id, provider, value, active, created) => db.prepare("INSERT INTO credential VALUES(?,?,?,?,?)").run(id, provider, JSON.stringify(value), active, created);
    try { await fn({ root, path, db, insert }); }
    finally { db.close(); await rm(root, { recursive: true, force: true }); }
}

test("native active/latest selection precedes environment and keeps providers separate", () => fixture(async ({ path, insert }) => {
    insert("go-old", "opencode-go", { type: "key", key: "active-old", configuration: { region: "example" } }, 1, 1);
    insert("go-new", "opencode-go", { type: "key", key: "inactive-new" }, 0, 99);
    insert("zen", "opencode", { type: "key", key: "other-provider" }, 1, 100);
    const read = createCredentialReader({ providerID: "opencode-go", databasePath: path, envNames: ["OPENCODE_API_KEY"], environment: { OPENCODE_API_KEY: "environment-key" } });
    assert.deepEqual(await read(signal()), { type: "key", key: "active-old", configuration: { region: "example" } });
    insert("go-later", "opencode-go", { type: "key", key: "active-later" }, 1, 2);
    assert.equal((await read(signal())).key, "active-later");
    insert("zz-go", "opencode-go", { type: "key", key: "id-tiebreak" }, 1, 2);
    assert.equal((await read(signal())).key, "id-tiebreak");
}));

test("credential lookup does not change a DELETE-mode database or open a writer", () => fixture(async ({ root, path, insert }) => {
    insert("go", "opencode-go", { type: "key", key: "synthetic-only" }, 1, 1);
    const before = await readFile(path), stamp = await stat(path);
    const read = createCredentialReader({ providerID: "opencode-go", databasePath: path, environment: {} });
    assert.equal((await read(signal())).type, "key");
    assert.deepEqual(await readFile(path), before);
    assert.equal((await stat(path)).mtimeMs, stamp.mtimeMs);
    for (const suffix of ["-wal", "-shm", "-journal"]) await assert.rejects(stat(path + suffix), { code: "ENOENT" });
}));

test("read-only lookup sees committed WAL credentials without checkpointing or editing the database", () => fixture(async ({ path, db, insert }) => {
    db.exec("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA wal_checkpoint(TRUNCATE)");
    const before = await readFile(path), stamp = await stat(path);
    insert("wal-key", "opencode-go", { type: "key", key: "committed-in-wal" }, 1, 1);
    const walBefore = await readFile(path + "-wal");
    assert.ok(walBefore.length > 0);
    const read = createCredentialReader({ providerID: "opencode-go", databasePath: path, environment: {} });
    assert.equal((await read(signal())).key, "committed-in-wal");
    assert.deepEqual(await readFile(path), before);
    assert.equal((await stat(path)).mtimeMs, stamp.mtimeMs);
    assert.deepEqual(await readFile(path + "-wal"), walBefore);
    // SQLite may create/update WAL coordination sidecars even for read-only
    // connections. immutable=1 would incorrectly ignore this committed key.
}));

test("missing provider or database can use provider-declared environment without creating storage", () => fixture(async ({ root, path }) => {
    const env = { FIRST_KEY: "first", SECOND_KEY: "second" };
    for (const databasePath of [path, join(root, "missing.db")]) {
        const read = createCredentialReader({ providerID: "opencode-go", databasePath, envNames: ["FIRST_KEY", "SECOND_KEY"], environment: env });
        assert.deepEqual(await read(signal()), { type: "key", key: "first" });
    }
    await assert.rejects(stat(join(root, "missing.db")), { code: "ENOENT" });
    await assert.rejects(createCredentialReader({ providerID: "opencode-go", databasePath: path, environment: {} })(signal()), /No OpenCode API-key credential/);
}));

test("stored OAuth and invalid credentials fail explicitly without leaking values or switching identity", () => fixture(async ({ path, insert }) => {
    const sentinel = "SYNTHETIC_SECRET_MUST_NOT_APPEAR";
    const read = createCredentialReader({ providerID: "opencode-go", databasePath: path, envNames: ["OPENCODE_API_KEY"], environment: { OPENCODE_API_KEY: "do-not-fallback" } });
    insert("invalid", "opencode-go", { type: "key", key: { private: sentinel } }, 1, 1);
    await assert.rejects(read(signal()), error => /unsupported format/.test(error.message) && !error.message.includes(sentinel));
    insert("oauth", "opencode-go", { type: "oauth", methodID: "oauth", access: sentinel, refresh: sentinel, expires: 123 }, 1, 2);
    await assert.rejects(read(signal()), error => /OAuth requires native refresh/.test(error.message) && !error.message.includes(sentinel));
}));

test("unreadable schema is an error and cancellation stops before credential access", () => fixture(async ({ path, db }) => {
    db.exec("DROP TABLE credential");
    const read = createCredentialReader({ providerID: "opencode-go", databasePath: path, envNames: ["KEY"], environment: { KEY: "do-not-fallback" } });
    await assert.rejects(read(signal()), /database could not be read/);
    const controller = new AbortController(), reason = new Error("synthetic cancellation");
    controller.abort(reason);
    await assert.rejects(read(controller.signal), error => error === reason);
}));

test("default database follows XDG and explicit path expansion stays local to the model implementation", () => {
    assert.equal(credentialDatabasePath(undefined, { HOME: "/example", XDG_DATA_HOME: "/data" }), "/data/opencode/opencode.db");
    assert.equal(credentialDatabasePath(undefined, { HOME: "/example", XDG_DATA_HOME: "relative" }), "/example/.local/share/opencode/opencode.db");
    for (const value of ["~/db", "$HOME/db", "${HOME}/db"]) assert.equal(credentialDatabasePath(value, { HOME: "/example" }), "/example/db");
    for (const value of [null, 1, ""]) assert.throws(() => credentialDatabasePath(value), /Invalid OpenCode database path/);
});

test("an explicit connection key is isolated and never edits the native credential database", () => fixture(async ({ path, insert }) => {
    insert("native", "opencode-go", { type: "key", key: "native-key" }, 1, 1);
    const before = await readFile(path), stamp = await stat(path);
    const override = createCredentialReader({ providerID: "opencode-go", databasePath: path, apiKey: "connection-key",
        envNames: ["KEY"], environment: { KEY: "environment-key" } });
    assert.deepEqual(await override(signal()), { type: "key", key: "connection-key" });
    assert.deepEqual(await readFile(path), before);
    assert.equal((await stat(path)).mtimeMs, stamp.mtimeMs);
    assert.equal((await createCredentialReader({ providerID: "opencode-go", databasePath: path })(signal())).key, "native-key");
    for (const apiKey of [null, false, "", " "]) assert.throws(() => createCredentialReader({ providerID: "route", apiKey }), /nonempty text/);
    const controller = new AbortController(); controller.abort(new Error("cancel override"));
    await assert.rejects(override(controller.signal), /cancel override/);
}));
