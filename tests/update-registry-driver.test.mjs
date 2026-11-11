import assert from 'node:assert/strict';
import {test} from 'node:test';
import {patchDriver} from './update-registry-driver.mjs';

test('capability additions preserve unrelated schema and operator metadata, and are idempotent', () => {
    const kind = {key: 'driver', label: 'Driver', description: 'Keep me', prosePre: 'Operator text', extensionsSchema: {capabilities: {properties: {custom: {type: 'string'}}}}};
    const patch = [{op: 'add', path: '/capabilities/properties/column_stats', value: {type: 'boolean'}}];
    const updated = patchDriver(kind, patch);
    assert.equal(updated.description, kind.description);
    assert.equal(updated.prosePre, kind.prosePre);
    assert.deepEqual(updated.extensionsSchema.capabilities.properties.custom, {type: 'string'});
    assert(!Object.hasOwn(kind.extensionsSchema.capabilities.properties, 'column_stats'));
    assert.deepEqual(patchDriver(updated, patch), updated);
    assert.throws(() => patchDriver(updated, [{...patch[0], value: {type: 'string'}}]), /Conflicting/);
    assert.throws(() => patchDriver(kind, [{...patch[0], path: '/__proto__/polluted'}]));
    assert.throws(() => patchDriver(kind, [{...patch[0], path: '/missing/field'}]), /Missing/);
});
