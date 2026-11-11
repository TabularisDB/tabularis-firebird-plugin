// Apply only the prepared capability additions to the operator-owned driver kind.
// node tests/update-registry-driver.mjs --token-file <private-file>
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';

export function patchDriver(kind, patch) {
    assert.equal(kind.key, 'driver');
    const result = structuredClone(kind);
    assert(result.extensionsSchema && typeof result.extensionsSchema === 'object');
    for (const change of patch) {
        assert.equal(change.op, 'add');
        const parts = change.path.split('/').slice(1);
        assert(parts.length && parts.every((p) => p && !['__proto__', 'constructor', 'prototype'].includes(p)));
        let parent = result.extensionsSchema;
        for (const part of parts.slice(0, -1)) {
            assert(parent[part] && typeof parent[part] === 'object', `Missing schema parent: ${change.path}`);
            parent = parent[part];
        }
        const key = parts.at(-1);
        if (Object.hasOwn(parent, key)) assert.deepEqual(parent[key], change.value, `Conflicting existing field: ${change.path}`);
        else parent[key] = structuredClone(change.value);
    }
    return result;
}

async function main() {
    assert.equal(process.argv[2], '--token-file', 'Provide --token-file <private-file>');
    assert(process.argv[3] && process.argv.length === 4);
    const token = (await readFile(process.argv[3], 'utf8')).trim();
    assert(token && !/[\r\n]/.test(token), 'Expected one admin token');
    const base = 'https://registry.tabularis.dev';
    const headers = {Authorization: `Bearer ${token}`, 'Content-Type': 'application/json'};
    async function request(path, options = {}) {
        const response = await fetch(`${base}${path}`, {...options, headers, signal: AbortSignal.timeout(30000)});
        assert(response.ok, `Registry HTTP ${response.status} for ${options.method ?? 'GET'} ${path}`);
        return response.json();
    }
    const path = '/api/admin/kinds/driver';
    const {kind} = await request(path);
    const patch = JSON.parse(await readFile(new URL('results/registry-driver-capabilities.patch.json', import.meta.url), 'utf8'));
    const updated = patchDriver(kind, patch);
    // Re-read before writing: do not silently overwrite an operator's concurrent edit.
    assert.deepEqual((await request(path)).kind, kind, 'Driver kind changed; retry from the current schema');
    await request(path, {method: 'PUT', body: JSON.stringify(updated)});
    assert.deepEqual((await request(path)).kind, updated, 'Registry did not retain the complete kind');
    const response = await fetch(`${base}/manifest.schema.json?kind=driver`, {signal: AbortSignal.timeout(30000), cache: 'no-store'});
    assert(response.ok);
    const schema = await response.text();
    for (const field of ['column_stats', 'query_timeout', 'psql_blocks']) assert(schema.includes(`"${field}"`), `Public schema still lacks ${field}`);
    console.log('Driver capability additions saved and verified in the public registry schema.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
    main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
