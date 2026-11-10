// Patch only the disposable CI Host checkout; production source is never built this way.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';

assert.equal(process.env.GITHUB_ACTIONS, 'true', 'CI-only Host fixture');
const root = path.resolve(process.argv[2]);
const target = path.resolve('target') + path.sep;
assert(root.startsWith(target), 'Host checkout must stay in this workspace target');
const source = path.join(root, 'src-tauri/src/transport/desktop/mod.rs');
let text = fs.readFileSync(source, 'utf8');
for (const [before, after] of [
  ['let app_paths = crate::runtime::paths::FixedRuntimePaths::system();',
   'let test_root = std::path::PathBuf::from(std::env::var("TABULARIS_DATA_DIR").expect("explicit test profile")); assert!(test_root.is_absolute()); assert_eq!(crate::paths::get_app_config_dir(), test_root); let app_paths = crate::runtime::paths::FixedRuntimePaths::new(test_root.clone(), test_root);'],
  ['std::sync::Arc::new(crate::runtime::secrets::KeyringRuntimeSecrets)', 'std::sync::Arc::new(FirebirdTestNoSecrets)'],
  ['deep_link.register("tabularis")', 'Err::<(), String>("registration disabled in disposable CI build".into())'],
]) {
  assert.equal(text.split(before).length, 2, `One fixture isolation anchor: ${before}`);
  text = text.replace(before, () => after);
}
text += `
struct FirebirdTestNoSecrets;
impl crate::runtime::secrets::RuntimeSecrets for FirebirdTestNoSecrets {
 fn get(&self, _: &str) -> Result<Option<String>,String> { Err("OS secrets disabled in CI".into()) }
 fn set(&self, _: &str, _: &str) -> Result<(),String> { Err("OS secrets disabled in CI".into()) }
 fn delete(&self, _: &str) -> Result<(),String> { Err("OS secrets disabled in CI".into()) }
}
`;
fs.writeFileSync(source, text);
const configPath = path.join(root, 'src-tauri/tauri.conf.json');
const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
config.identifier = 'dev.tabularis.firebird-validation';
config.productName = 'Firebird disposable validation';
config.build.devUrl = null;
config.app.windows[0].title = config.productName;
config.plugins['deep-link'].desktop.schemes = ['tabularis-firebird-validation'];
fs.writeFileSync(configPath, JSON.stringify(config));
const manifest = path.join(root, 'src-tauri/Cargo.toml');
fs.appendFileSync(manifest, '\n[workspace]\n');
console.log('Disposable Host paths, denied secrets and disabled URI registration configured.');
