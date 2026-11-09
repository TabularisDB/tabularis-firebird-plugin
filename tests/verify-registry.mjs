// Run after publication: node tests/verify-registry.mjs [version]. No installed plugin is changed.
import assert from 'node:assert/strict';
import {createHash, createPublicKey, verify} from 'node:crypto';
import {readFile} from 'node:fs/promises';

const registry = 'https://registry.tabularis.dev';
const repository = 'https://github.com/TabularisDB/tabularis-firebird-plugin';
const version = process.argv[2] ?? JSON.parse(await readFile(new URL('../.tabularium', import.meta.url), 'utf8')).version;
assert.match(version, /^[0-9A-Za-z.+-]+$/);
async function json(path) {
    const response = await fetch(`${registry}${path}`, {signal: AbortSignal.timeout(15000)});
    assert(response.ok, `Registry HTTP ${response.status}: ${path}`);
    return response.json();
}
const [entry, jwks] = await Promise.all([json('/api/plugins/firebird'), json('/.well-known/registry-key.json')]);
assert.equal(entry.repoUrl, repository);
assert.equal(entry.status, 'approved');
const release = entry.releases.find((release) => release.version === version);
assert(release, `Version ${version} is absent from the registry`);
const [headerPart, payloadPart, signaturePart, extra] = release.integrity.jws.split('.');
assert(headerPart && payloadPart && signaturePart && extra === undefined);
const header = JSON.parse(Buffer.from(headerPart, 'base64url'));
assert.equal(header.alg, 'EdDSA');
const jwk = jwks.keys.find((key) => key.kid === header.kid);
assert(jwk && jwk.kty === 'OKP' && jwk.crv === 'Ed25519', 'Unknown or unsupported signing key');
const key = createPublicKey({key: jwk, format: 'jwk'});
assert(verify(null, Buffer.from(`${headerPart}.${payloadPart}`), key, Buffer.from(signaturePart, 'base64url')), 'Invalid registry signature');
const payload = JSON.parse(Buffer.from(payloadPart, 'base64url'));
assert.equal(payload.plugin_slug, 'firebird');
assert.equal(payload.release_version, version);
assert.equal(payload.registry.replace(/\/$/, ''), registry);
const names = ['default.tabularium', ...['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win-x64'].map((platform) => `firebird-plugin-${platform}.zip`)];
assert.deepEqual(payload.assets.map((asset) => asset.name).sort(), names.sort());
for (const asset of payload.assets) {
    assert.match(asset.sha256, /^[a-f0-9]{64}$/);
    const response = await fetch(`${repository}/releases/download/v${version}/${asset.name}`, {signal: AbortSignal.timeout(30000)});
    assert(response.ok, `Asset HTTP ${response.status}: ${asset.name}`);
    const hash = createHash('sha256');
    let bytes = 0;
    for await (const chunk of response.body) {
        bytes += chunk.length;
        assert(bytes <= 32 * 1024 * 1024, 'Asset exceeds verification size limit');
        hash.update(chunk);
    }
    assert.equal(hash.digest('hex'), asset.sha256, `Signed hash mismatch: ${asset.name}`);
    if (asset.size !== undefined) assert.equal(bytes, asset.size);
}
console.log(`Verified registry Ed25519 signature, release identity and six signed downloads for ${version}.`);
