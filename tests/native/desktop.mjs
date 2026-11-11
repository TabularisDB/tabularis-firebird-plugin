// Real native WebView and Tauri IPC via the Host's existing debug automation plugin.
// OS file pickers use the Host e2e-dialog mock and only owned fixture paths.
import fs from 'node:fs';
import path from 'node:path';
import {spawn} from 'node:child_process';
import assert from 'node:assert/strict';

process.umask(0o077);
const root = process.cwd();
const binary = path.resolve(process.env.TABULARIS_NATIVE_HOST ?? '');
assert(process.env.TABULARIS_NATIVE_HOST && binary.startsWith(path.join(root, 'target') + path.sep));
const connection = JSON.parse(fs.readFileSync(process.env.FIREBIRD_TEST_CONFIG, 'utf8'));
const manifest = JSON.parse(fs.readFileSync('.tabularium', 'utf8'));
const profile = fs.mkdtempSync(path.join(root, 'target/native-ui-'));
const dump = path.join(profile, 'whole-database.sql');
const plugin = path.join(profile, 'plugins/firebird');
fs.mkdirSync(plugin, {recursive: true});
const executable = process.platform === 'win32' ? 'firebird-plugin.exe' : 'firebird-plugin';
fs.copyFileSync(path.join(root, 'target/debug', executable), path.join(plugin, executable));
fs.chmodSync(path.join(plugin, executable), 0o700);
fs.writeFileSync(path.join(plugin, '.tabularium'), JSON.stringify(manifest));
fs.writeFileSync(path.join(profile, 'config.json'), JSON.stringify({language: 'en', activeExternalDrivers: ['firebird'], showWelcome: false, checkForUpdates: false, autoCheckUpdatesOnStartup: false, pingInterval: 0}));
const output = fs.createWriteStream(path.join(profile, 'app.stdout.log'));
const errors = fs.createWriteStream(path.join(profile, 'app.stderr.log'));
const app = spawn(binary, [], {
  windowsHide: true, detached: process.platform !== 'win32',
  env: {...process.env, TABULARIS_DATA_DIR: profile, WEBVIEW2_USER_DATA_FOLDER: path.join(profile, 'webview'), E2E_DIALOG_SAVE_PATH: dump, E2E_DIALOG_OPEN_PATH: dump},
  stdio: ['ignore', 'pipe', 'pipe'],
});
let startup = '', port, startupError;
app.on('error', error => {startupError = error;});
app.stdout.on('data', data => {output.write(data); startup = (startup + data).slice(-16000); const match = startup.match(/\[webdriver\] listening on port (\d+)/); if (match) port = Number(match[1]);});
app.stderr.pipe(errors);
const checks = [];
const pass = label => {checks.push(label);console.log(`Native check: ${label}`);};
const wait = async (check, label, attempts = 120) => {
  let last;
  for (let n = 0; n < attempts; n++) {
    if (startupError) throw startupError;
    if (app.exitCode !== null) throw Error(`Native app exited (${app.exitCode}) during ${label}`);
    try {const value = await check(); if (value) return value;} catch (error) {last = error;}
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  throw Error(`${label}: ${last?.message ?? 'timeout'}`);
};
const evaluate = async (fn, ...args) => {
  const script = `var done=arguments[arguments.length-1];Promise.resolve((${fn.toString()})(...Array.from(arguments).slice(0,-1))).then(v=>done(v===undefined?null:v),e=>done({error:'NativeTest',message:String(e)}));`;
  const response = await fetch(`http://127.0.0.1:${port}/script/execute-async`, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({script, args}), signal: AbortSignal.timeout(35000)});
  const result = await response.json();
  if (!response.ok) throw Error(`Native automation ${response.status}: ${JSON.stringify(result)}`);
  return result.value;
};
const invoke = (command, args = {}) => evaluate((command, args) => window.__TAURI_INTERNALS__.invoke(command, args), command, args);
const click = (name, scope = 'body') => evaluate((name, scope) => {
  const container = [...document.querySelectorAll(scope)].at(-1);
  if(!container) return false;
  const buttons = [...container.querySelectorAll('button')];
  const button = buttons.find(button => !button.disabled && [button.textContent.replace(/\s+/g,' ').trim(), button.title, button.getAttribute('aria-label')].includes(name));
  if (!button) throw Error(`Button ${name}: ${buttons.map(button => `${button.textContent.trim()} (disabled=${button.disabled})`).join(', ')}`);
  button.click(); return true;
}, name, scope);
const editor = (sql, scope = 'body') => evaluate((sql, scope) => {
  const element = [...document.querySelectorAll(scope)].at(-1)?.querySelector('.monaco-editor');
  let fiber;
  for (let node = element; node && !fiber; node = node.parentElement) {const key = Object.keys(node).find(key => key.startsWith('__reactFiber')); if (key) fiber = node[key];}
  for (; fiber; fiber = fiber.return) {let n = 0; for (let hook = fiber.memoizedState; hook && n++ < 100; hook = hook.next) {const value = hook.memoizedState?.current; if (value && typeof value.getModel === 'function' && typeof value.setValue === 'function') {value.setValue(sql); return true;}}}
  return false;
}, sql, scope);
let id;
const query = (query, database) => invoke('execute_query', {connectionId: id, query, limit: 100, page: 1, ...(database ? {database} : {})});
const modal = '[role="dialog"]';
const editTrigger = async () => {
  await wait(() => evaluate(() => {
    let label = [...document.querySelectorAll('span,div,button')].find(node => node.textContent === 'NATIVE_UI_TRG' && node.childElementCount === 0);
    if (!label) {const header = [...document.querySelectorAll('button')].find(button => /^TRIGGERS\s*\(/i.test(button.textContent.trim())); header?.click(); return false;}
    label.dispatchEvent(new MouseEvent('contextmenu', {bubbles: true, clientX: label.getBoundingClientRect().x + 8, clientY: label.getBoundingClientRect().y + 8})); return true;
  }), 'trigger context menu');
  await wait(() => evaluate(() => {const item = [...document.querySelectorAll('button,div')].find(node => node.textContent.trim() === 'Edit Trigger' && node.childElementCount <= 2); if (!item) return false; item.click(); return true;}), 'edit trigger action');
  await wait(() => evaluate(() => !!document.querySelector('[role="dialog"] .monaco-editor')), 'native trigger definition');
};
try {
  await wait(() => port, 'native automation startup', 240);
  await wait(() => evaluate(() => !!window.__TAURI_INTERNALS__), 'real Tauri IPC');
  await invoke('enable_plugin', {pluginId: 'firebird'});
  const saved = await invoke('save_connection', {name: 'Firebird native fixture', params: {...connection, driver: 'firebird', save_in_keychain: false}});
  id = saved.id; assert(id);
  await evaluate(() => {window.__nativeFixtureBeforeReload = true; setTimeout(() => location.reload(), 50);});
  await wait(() => evaluate(() => !window.__nativeFixtureBeforeReload && document.readyState === 'complete'), 'native page reload');
  await wait(() => evaluate(id => {
    if(document.querySelector('.monaco-editor')) return true;
    const node = [...document.querySelectorAll('[data-connection-id]')].find(node => node.getAttribute('data-connection-id') === id);
    if(node && !/cursor-not-allowed|pointer-events-none/.test(node.className)) node.dispatchEvent(new MouseEvent('dblclick', {bubbles: true, button: 0, detail: 2}));
    return false;
  }, id), 'native SQL editor');
  await evaluate(() => document.querySelector('button[aria-label="Dismiss"]')?.click());
  await wait(() => editor('CREATE TABLE NATIVE_UI_DATA(ID INTEGER PRIMARY KEY, TXT VARCHAR(100), BIN BLOB SUB_TYPE BINARY)'), 'editor buffer');
  assert(await click('Run'));
  await wait(async () => (await query("SELECT COUNT(*) FROM RDB$RELATIONS WHERE RDB$RELATION_NAME='NATIVE_UI_DATA'")).rows[0][0] === 1, 'native editor CREATE');
  await editor("INSERT INTO NATIVE_UI_DATA VALUES(1,'native data',x'00ff807f')");
  assert(await click('Run'));
  await wait(async () => (await query('SELECT COUNT(*) FROM NATIVE_UI_DATA')).rows[0][0] === 1, 'native editor INSERT');
  pass('native editor CREATE/INSERT through UI');
  assert(await click('Create New Trigger'));
  const sql = "CREATE TRIGGER NATIVE_UI_TRG FOR NATIVE_UI_DATA ACTIVE BEFORE INSERT POSITION 7 AS BEGIN IF (NEW.TXT IS NULL) THEN NEW.TXT='generated'; END";
  await wait(() => editor(' ', modal), 'native trigger editor');
  // Monaco installs its change listener after onMount; emulate input after that setup.
  await new Promise(resolve => setTimeout(resolve, 400));
  assert(await editor(sql, modal));
  await wait(() => click('Create Trigger', modal), 'native trigger create');
  await wait(async () => (await query("SELECT COUNT(*) FROM RDB$TRIGGERS WHERE RDB$TRIGGER_NAME='NATIVE_UI_TRG'")).rows[0][0] === 1, 'trigger persisted');
  await query("COMMENT ON TRIGGER NATIVE_UI_TRG IS 'preserve native comment'");
  await editTrigger();
  await editor(sql.replace('CREATE TRIGGER', 'CREATE OR ALTER TRIGGER').replace('generated', 'edited'), modal);
  assert(await click('Save Changes', modal));
  await wait(async () => (await query("SELECT RDB$TRIGGER_SOURCE FROM RDB$TRIGGERS WHERE RDB$TRIGGER_NAME='NATIVE_UI_TRG'")).rows[0][0].includes("'edited'"), 'latest editor buffer saved');
  await query('INSERT INTO NATIVE_UI_DATA(ID) VALUES(2)');
  assert.equal((await query('SELECT TXT FROM NATIVE_UI_DATA WHERE ID=2')).rows[0][0], 'edited');
  assert.deepEqual((await query("SELECT RDB$TRIGGER_SEQUENCE,RDB$TRIGGER_INACTIVE,RDB$DESCRIPTION FROM RDB$TRIGGERS WHERE RDB$TRIGGER_NAME='NATIVE_UI_TRG'")).rows[0], [7, 0, 'preserve native comment']);
  pass('native trigger create/edit/firing, immediate save and metadata preservation');
  await editTrigger(); await editor('CREATE OR ALTER TRIGGER NATIVE_UI_TRG ACTIVE BEFORE INSERT AS BEGIN INVALID STATEMENT; END', modal);
  assert(await click('Save Changes', modal));
  await wait(() => evaluate(() => !!document.querySelector('[role="dialog"] [role="alert"]')), 'failed save reported');
  assert((await query("SELECT RDB$TRIGGER_SOURCE FROM RDB$TRIGGERS WHERE RDB$TRIGGER_NAME='NATIVE_UI_TRG'")).rows[0][0].includes("'edited'"));
  assert(await click('Cancel', modal)); pass('failed trigger save keeps previous trigger');
  assert(await click('Dump Database')); await wait(() => click('Export', 'body > div[role="presentation"]'), 'native database export action');
  await wait(() => fs.existsSync(dump) && fs.statSync(dump).size > 0, 'whole-database SQL export', 240);
  assert(fs.readFileSync(dump, 'utf8').startsWith('-- Tabularis Firebird SQL 1 '));
  await wait(() => evaluate(() => !document.body.textContent.includes('Dump Database -')), 'export completed');
  await wait(() => click('OK', 'body > div[role="presentation"]'), 'export completion notice');
  pass('native whole-database dump action');
  assert(await click('Run SQL File...'));
  await wait(() => click('OK', '[role="dialog"]'), 'confirm selected fixture SQL file');
  await wait(() => evaluate(() => !!document.querySelector('[role="dialog"] input[type="checkbox"]')), 'restore destination dialog');
  const target = connection.database + '.native-ui.fdb';
  assert(await evaluate(target => {const input = document.querySelector('[role="dialog"] input:not([type="checkbox"])'); if (!input) return false; Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, target); input.dispatchEvent(new Event('input', {bubbles: true})); return true;}, target));
  await wait(() => click('Restore database', '[role="dialog"]'), 'explicit restore start');
  await wait(async () => (await query('SELECT COUNT(*) FROM NATIVE_UI_DATA', target)).rows[0][0] === 2, 'restored data', 240);
  assert.equal((await query('SELECT BIN FROM NATIVE_UI_DATA WHERE ID=1', target)).rows[0][0], 'BLOB:4:application/octet-stream:AP+Afw==');
  assert.equal((await query('SELECT * FROM FB_ADD(20,22)', target)).rows[0][0], 42);
  pass('native explicit new-database restore, complete binary data and routines');
  await assert.rejects(() => invoke('import_database', {connectionId: id, filePath: dump, database: target, createDatabase: true}));
  assert.equal((await query('SELECT COUNT(*) FROM NATIVE_UI_DATA', target)).rows[0][0], 2);
  pass('existing destination protected');
  await wait(() => click('Close', '[role="dialog"]'), 'close SQL restore result');
  assert(await click('Dump Database'));
  await wait(() => evaluate(() => {
    const select = [...document.querySelectorAll('select')].find(select => [...select.options].some(option => option.value === 'firebird'));
    if (!select) return false;
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(select, 'firebird');
    select.dispatchEvent(new Event('change', {bubbles:true}));
    return true;
  }), 'native Firebird backup format');
  assert(await click('Export', 'body > div[role="presentation"]'));
  await wait(() => fs.existsSync(dump) && fs.readFileSync(dump)[0] === 0, 'native FBK stream',240);
  await wait(() => click('OK', 'body > div[role="presentation"]'), 'native backup completed');
  pass('native FBK format selection and atomic export');
  assert(await click('Run SQL File...'));
  await wait(() => click('OK', '[role="dialog"]'), 'confirm native backup input');
  await wait(() => evaluate(() => !!document.querySelector('[role="dialog"] input[type="checkbox"]')), 'native restore destination dialog');
  const nativeTarget = connection.database + '.native-ui-fbk.fdb';
  assert(await evaluate(target => {const input = document.querySelector('[role="dialog"] input:not([type="checkbox"])'); if (!input) return false; Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input,target); input.dispatchEvent(new Event('input',{bubbles:true})); return true;},nativeTarget));
  assert(await click('Restore database','[role="dialog"]'));
  await wait(async () => (await query('SELECT COUNT(*) FROM NATIVE_UI_DATA',nativeTarget)).rows[0][0] === 2, 'native FBK restore',240);
  assert.equal((await query('SELECT BIN FROM NATIVE_UI_DATA WHERE ID=1',nativeTarget)).rows[0][0],'BLOB:4:application/octet-stream:AP+Afw==');
  await assert.rejects(() => invoke('import_database',{connectionId:id,filePath:dump,database:nativeTarget,createDatabase:true}));
  pass('native FBK restore from opaque content and existing-target protection');
  fs.mkdirSync('target/native-evidence', {recursive: true});
  fs.writeFileSync(`target/native-evidence/${process.platform}-${process.arch}.json`, JSON.stringify({version: manifest.version, host: process.env.TABULARIS_NATIVE_HOST_COMMIT ?? 'local patched build', platform: process.platform, arch: process.arch, checks, file_pickers: 'Host e2e mock with owned paths', secrets: 'denied by disposable Host build'}, null, 2));
  console.log(`Native ${process.platform}/${process.arch}: ${checks.length} integration groups passed.`);
} catch(error) {
  fs.mkdirSync('target/native-evidence', {recursive: true});
  if(port) {try {fs.writeFileSync(`target/native-evidence/${process.platform}-${process.arch}-failure.txt`,await evaluate(() => document.body.innerText.slice(-6000)));}catch{}}
  throw error;
} finally {
  if (app.exitCode === null && app.pid) {
    if (process.platform === 'win32') await new Promise(resolve => {const kill = spawn('taskkill', ['/PID', String(app.pid), '/T', '/F'], {windowsHide: true, stdio: 'ignore'}); kill.on('close', resolve);});
    else {try {process.kill(-app.pid, 'SIGTERM');} catch {}}
  }
  await new Promise(resolve => {if (app.exitCode !== null) return resolve(); const timer = setTimeout(resolve, 5000); app.once('close', () => {clearTimeout(timer); resolve();});});
  if (app.exitCode === null && app.pid && process.platform !== 'win32') {try {process.kill(-app.pid, 'SIGKILL');} catch {}}
  output.end(); errors.end();
  assert(profile.startsWith(path.join(root, 'target') + path.sep) && path.basename(profile).startsWith('native-ui-'));
  fs.rmSync(profile, {recursive: true, force: true, maxRetries: 30, retryDelay: 100});
}
