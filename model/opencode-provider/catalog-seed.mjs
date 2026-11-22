import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';

const revision = 'a177c5468699a3ce48b27bc417847a0804af1564';
const seeds = new Map([
    ['opencode-go', [22380, '7a87809e9992b264e3649cb3573a96231d95bff98293817869338c12792f8cf3']],
    ['opencode', [75647, '17262a403753acb2d5899721ba2007c888b49d2c16ac4a70aa328c689818db62']],
]);

export function hasPackageCatalogSeed(config) {
    return !config.models_file && !config.models_url && seeds.has(config.provider);
}

export async function readPackageCatalogSeed(provider, signal) {
    const [length, digest] = seeds.get(provider);
    const url = new URL(`./catalog-seeds/${provider}.${revision}.json`, import.meta.url);
    let file;
    try {
        signal.throwIfAborted();
        file = await open(url, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
        const stat = await file.stat();
        if (!stat.isFile() || stat.size !== length) throw new Error('Invalid seed extent');
        const raw = Buffer.alloc(length + 1);
        let at = 0;
        while (at < raw.length) {
            signal.throwIfAborted();
            const { bytesRead } = await file.read(raw, at, raw.length - at, at);
            if (!bytesRead) break;
            at += bytesRead;
        }
        signal.throwIfAborted();
        if (at !== length || createHash('sha256').update(raw.subarray(0, at)).digest('hex') !== digest)
            throw new Error('Invalid seed digest');
        const catalog = JSON.parse(raw.subarray(0, at));
        if (Object.keys(catalog).length !== 1 || catalog[provider]?.id !== provider)
            throw new Error('Invalid seed identity');
        return catalog;
    } catch (error) {
        signal.throwIfAborted();
        // An installation defect must not silently move a restarted Worker to
        // different model metadata while Core still has a local descriptor.
        throw new Error('OpenCode packaged model catalog is missing or invalid', { cause: error });
    } finally {
        await file?.close();
    }
}
