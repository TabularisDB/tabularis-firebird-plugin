// Run only with the disposable fixture from tests/live-firebird.ps1 or .sh.
// Usage: node tests/host-psql-smoke.mjs <host-checkout>
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {readFile} from 'node:fs/promises';
import {createRequire} from 'node:module';
import {resolve, join} from 'node:path';
import {createInterface} from 'node:readline';
import {pathToFileURL} from 'node:url';

const host = resolve(process.argv[2] ?? 'target/tabularis-host');
const require = createRequire(pathToFileURL(join(host, 'package.json')));
const {build} = require('esbuild');
const bundle = await build({
    stdin: {
        contents: `export {resolveSqlSyntax} from './packages/web-ui/src/utils/sqlSyntax';
export {splitBatches} from './packages/web-ui/src/utils/sqlSplitter';`,
        loader: 'ts',
        resolveDir: host,
    },
    tsconfig: join(host, 'packages/web-ui/tsconfig.app.json'),
    bundle: true,
    platform: 'node',
    format: 'esm',
    write: false,
});
const {resolveSqlSyntax, splitBatches} = await import(`data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].contents).toString('base64')}`);
const config = JSON.parse(await readFile(process.env.FIREBIRD_TEST_CONFIG, 'utf8'));
const child = spawn(resolve(`target/debug/firebird-plugin${process.platform === 'win32' ? '.exe' : ''}`), [], {stdio: ['pipe', 'pipe', 'inherit'], windowsHide: true});
const pending = new Map();
let nextId = 1;
function failAll(error) {
    for (const call of pending.values()) { clearTimeout(call.timer); call.reject(error); }
    pending.clear();
}
child.on('error', failAll);
child.stdin.on('error', failAll);
child.on('exit', () => failAll(new Error('Plugin exited before replying')));
createInterface({input: child.stdout}).on('line', (line) => {
    const response = JSON.parse(line);
    const call = pending.get(response.id);
    if (!call) return;
    pending.delete(response.id);
    clearTimeout(call.timer);
    if (response.error) call.reject(new Error(response.error.message));
    else call.resolve(response.result);
});
function rpc(method, params) {
    return new Promise((resolveCall, reject) => {
        const id = nextId++;
        const timer = setTimeout(() => { pending.delete(id); reject(new Error(`${method} timed out`)); }, 15000);
        pending.set(id, {resolve: resolveCall, reject, timer});
        child.stdin.write(`${JSON.stringify({jsonrpc: '2.0', id, method, params})}\n`);
    });
}
const query = (sql, session_id) => rpc('execute_query', {params: config, query: sql, session_id});
const script = `CREATE TABLE FB_HOST_PSQL (ID INTEGER PRIMARY KEY, N INTEGER, T VARCHAR(80));
SET TERM !!;
CREATE FUNCTION FB_HOST_FN RETURNS VARCHAR(80) AS
BEGIN RETURN q'[it's; -- END BEGIN]'; END!!
CREATE OR ALTER PROCEDURE FB_HOST_PROC RETURNS (N INTEGER) AS
DECLARE VARIABLE X INTEGER;
DECLARE FUNCTION LOCAL_F RETURNS INTEGER AS BEGIN RETURN 7; END
BEGIN
  X=LOCAL_F();
  IF (X=7) THEN BEGIN N=CASE WHEN X=7 THEN 9 ELSE 0 END; SUSPEND; END
END!!
CREATE TRIGGER FB_HOST_TRIGGER FOR FB_HOST_PSQL ACTIVE BEFORE INSERT AS
BEGIN NEW.N=COALESCE(NEW.N,0)+1; NEW.T=FB_HOST_FN(); END!!
CREATE PACKAGE FB_HOST_PKG AS BEGIN FUNCTION F RETURNS INTEGER; PROCEDURE P RETURNS(N INTEGER); END!!
RECREATE PACKAGE BODY FB_HOST_PKG AS
BEGIN
  FUNCTION F RETURNS INTEGER AS BEGIN RETURN 12; END
  PROCEDURE P RETURNS(N INTEGER) AS BEGIN N=F(); SUSPEND; END
END!!
EXECUTE BLOCK AS DECLARE VARIABLE X INTEGER;
BEGIN X=5; INSERT INTO FB_HOST_PSQL(ID,N) VALUES(1,:X); END!!
SET TERM ;!!
SELECT FB_HOST_FN() AS HOST_TEXT FROM RDB$DATABASE;
SELECT N AS HOST_PROC FROM FB_HOST_PROC;
SELECT N AS HOST_PACKAGE FROM FB_HOST_PKG.P;
SELECT N AS HOST_TRIGGER, T AS HOST_TRIGGER_TEXT FROM FB_HOST_PSQL;`;
let completed = false;
try {
    const syntax = resolveSqlSyntax({sql_syntax: {extends: 'generic', psql_blocks: true, nested_block_comments: true}});
    const statements = splitBatches(script, syntax).map((statement) => statement.text);
    const seen = new Map();
    for (const statement of statements) {
        const result = await query(statement);
        for (const [index, column] of result.columns.entries()) seen.set(column, result.rows[0]?.[index]);
    }
    assert.equal(seen.get('HOST_TEXT'), "it's; -- END BEGIN");
    assert.equal(seen.get('HOST_PROC'), 9);
    assert.equal(seen.get('HOST_PACKAGE'), 12);
    assert.equal(seen.get('HOST_TRIGGER'), 6);
    assert.equal(seen.get('HOST_TRIGGER_TEXT'), "it's; -- END BEGIN");
    await query('BEGIN', 'host-state-smoke');
    assert.deepEqual(await rpc('get_session_state', {session_id: 'host-state-smoke'}), {in_transaction: true});
    await query('ROLLBACK', 'host-state-smoke');
    assert.deepEqual(await rpc('get_session_state', {session_id: 'host-state-smoke'}), {in_transaction: false});
    completed = true;
    console.log(`Verified actual host splitting and Firebird execution of ${statements.length} statements, plus live session-state RPC.`);
} finally {
    try {
        for (const sql of ['DROP TABLE FB_HOST_PSQL', 'DROP PACKAGE FB_HOST_PKG', 'DROP PROCEDURE FB_HOST_PROC', 'DROP FUNCTION FB_HOST_FN']) {
            await query(sql).catch((error) => { if (completed) throw error; });
        }
    } finally { child.stdin.end(); child.kill(); }
}
