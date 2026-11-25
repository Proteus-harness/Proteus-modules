import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { join, resolve } from 'node:path';

export const root = fileURLToPath(new URL('../../../', import.meta.url));
export const node = join(root, 'workflow/opencode-runtime/node_modules/node-linux-x64/bin/node');
export const worker = join(root, 'workflow/opencode-runtime/worker.mjs');
export const core = process.env.PROTEUS_TEST_CORE_BIN ?? resolve(root, '../Proteus-desktop/build/Proteus/bin/proteus');
export const moduleBin = name => join(process.env.PROTEUS_TEST_MODULES_BIN_DIR ?? resolve(core, '..'), name);
export const flash = 'deepseek-v4.1-flash';
export const second = 'synthetic-second';

// Actual models.dev input, parsed by the pinned OpenCode SDK, never an adapter stub.
export function nativeCatalog(api = 'http://127.0.0.1:1/v1') {
    const model = (id, name, efforts) => ({ id, name, release_date: '2026-01-01',
        modalities: { input: ['text'], output: ['text'] }, limit: { context: 128000, output: 8192 },
        reasoning: true, reasoning_options: [{ type: 'effort', values: efforts }], tool_call: true,
        cost: { input: 0, output: 0 } });
    return { 'opencode-go': { id: 'opencode-go', name: 'Synthetic Go', env: ['OPENCODE_API_KEY'],
        npm: '@ai-sdk/openai-compatible', api,
        models: { [flash]: model(flash, 'Synthetic Flash', ['low', 'high']),
            [second]: model(second, 'Discovered second', ['low']) } } };
}

export async function fixture() {
    const target = join(root, 'target/opencode-model-process');
    await mkdir(target, { recursive: true });
    const dir = await mkdtemp(join(target, 'run-'));
    const workspace = join(dir, 'workspace');
    await mkdir(workspace);
    const database = join(dir, 'opencode.db'), catalog = join(dir, 'models.json');
    const db = new DatabaseSync(database);
    db.exec('CREATE TABLE credential(id TEXT PRIMARY KEY,integration_id TEXT,value TEXT,active INTEGER,time_created INTEGER)');
    db.prepare('INSERT INTO credential VALUES(?,?,?,?,?)').run('go', 'opencode-go', JSON.stringify({ type: 'key', key: 'synthetic-local-only' }), 1, 1);
    db.close();
    await writeFile(catalog, JSON.stringify(nativeCatalog()));
    const config = { provider: 'opencode-go', database_path: database, models_file: catalog };
    const env = { PATH: process.env.PATH, HOME: dir, XDG_DATA_HOME: join(dir, 'data'),
        XDG_CONFIG_HOME: join(dir, 'config'), XDG_STATE_HOME: join(dir, 'state'),
        PROTEUS_HOME: join(dir, 'proteus'), PROTEUS_CONFIG_HOME: join(dir, 'config'),
        NO_PROXY: '127.0.0.1,localhost', no_proxy: '127.0.0.1,localhost' };
    return { dir, workspace, database, catalog, config, env };
}

export function component(f, config = f.config, connections = { opencode: config }) {
    const child = spawn(node, [worker], { cwd: f.workspace, env: f.env, stdio: ['pipe', 'pipe', 'pipe'] });
    const pending = new Map(); let seq = 0, stderr = '';
    child.stderr.on('data', chunk => { stderr += chunk; });
    const exit = new Promise(resolve => child.once('exit', (code, signal) => resolve({ code, signal })));
    child.on('exit', () => {
        for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(new Error('Worker exited: ' + stderr)); }
        pending.clear();
    });
    const lines = createInterface({ input: child.stdout });
    lines.on('line', line => {
        const frame = JSON.parse(line), entry = pending.get(frame.id);
        if (!entry) throw new Error('Unexpected worker frame: ' + line);
        pending.delete(frame.id); clearTimeout(entry.timer); entry.resolve(frame);
    });
    function request(method, params, id = `h:1:${++seq}`) {
        const result = new Promise((resolve, reject) => {
            const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Worker ${method} timed out: ${stderr}`)); }, 15000);
            pending.set(id, { resolve, reject, timer });
        });
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
        return result;
    }
    return { child, exit, initialize: () => request('initialize', { protocol_version: 'v3', component_id: 'synthetic-opencode',
        exports: Object.entries(connections).map(([module_id, module_config]) => ({ slot: 'model', module_id,
            contract_version: 'v14', composition: 'select_one', module_config,
            host_features: ['model.catalog.v1', 'model.quota.v1'] })) }, 'h:1:0'),
    invoke(method, params = null, module_id = 'opencode') { const id = `h:1:${++seq}`; return { id, result: request(method, {
        export: { slot: 'model', module_id }, lineage: { root_invocation_id: id, parent_invocation_id: null, depth: 0 }, params }, id) }; },
    cancel(id) { child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: '$/cancelRequest', params: { invocation_id: id, cause: 'user' } }) + '\n'); },
    async close() { child.stdin.end(); const timer = setTimeout(() => child.kill('SIGKILL'), 5000); try { return await exit; } finally { clearTimeout(timer); } } };
}

export async function appServer(f, name, overrides = {}) {
    const configPath = join(f.dir, 'synthetic.json');
    const config = { active_provider: 'go',
        providers: { go: { provider: 'opencode', model: flash, stream: true, reasoning: { effort: 'high', summary: true } } },
        modules: { session_storage: 'jsonl' },
        components: {
            model: { command: node, args: [...(f.preload ? ['--import', f.preload] : []), worker],
                env_allowlist: Object.keys(f.env), exports: { model: { opencode: {} } } },
            storage: { command: moduleBin('proteus-session-storage-module'),
                exports: { session_storage: { jsonl: {} } } },
        },
        module_config: { model: { opencode: f.config } } };
    await writeFile(configPath, JSON.stringify({ ...config, ...overrides,
        modules: { ...config.modules, ...overrides.modules },
        components: { ...config.components, ...overrides.components },
        module_config: { ...config.module_config, ...overrides.module_config } }));
    const child = spawn(core, ['--config', configPath, '--cwd', f.workspace, 'server', 'http', '--host', '127.0.0.1', '--port', '0', '--token', 'synthetic-token', '--ready-stdout'],
        { cwd: f.workspace, env: f.env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const exit = new Promise(resolve => child.once('exit', (code, signal) => resolve({ code, signal })));
    let origin;
    try { origin = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`Core readiness timed out: ${stderr}`)), 15000);
        child.once('exit', code => { clearTimeout(timer); reject(new Error(`Core exited ${code}: ${stderr}`)); });
        createInterface({ input: child.stdout }).on('line', line => {
            if (!line.startsWith('{')) return;
            const frame = JSON.parse(line);
            if (frame.type === 'http_ready') { clearTimeout(timer); resolve(frame.origin); }
        });
    }); } catch (error) { child.kill('SIGKILL'); await exit; throw error; }
    async function request(path, body) {
        const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST',
            headers: { Authorization: 'Bearer synthetic-token', 'Content-Type': 'application/json' },
            ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(15000) });
        const value = await response.json();
        if (!response.ok) throw new Error(`Core ${path} HTTP ${response.status}: ${JSON.stringify(value)}`);
        return value;
    }
    return { child, request, async close() {
        const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
        try { await request('/shutdown', {}); return await exit; }
        finally { clearTimeout(timer); await writeFile(join(f.dir, `${name}.log`), stdout + '\n' + stderr); }
    } };
}
