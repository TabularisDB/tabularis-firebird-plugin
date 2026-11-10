# Windows React/web UI validation

Validated on Windows against the actual Tabularis frontend, web transport and application APIs at host commit `ec8590e37405bb23bf7efea88ac0a23e25074f1a`, using portable Firebird 5.0.4. Playwright 1.62.1/Chromium headless 151.0.7922.34 drove UI controls and keyboard input; no application RPCs were invoked directly by the browser test. The host's native Tauri transport was not exercised.

The published, signed Beta.3 was installed and updated to Beta.4 through Plugin Center. A fresh profile also installed Beta.4 directly. Connection testing and saving succeeded, but opening the saved connection exposed an actual compatibility failure: the host rejected connection-local metadata because the plugin categorized `BOOLEAN` as `boolean`, outside its accepted categories. The earlier apparent missing-path error did not reproduce after verifying the input value and is not treated as a host defect.

The Beta.5 candidate changes the category to `other`, preserving the SQL type and Boolean values. A new manifest contract regression failed before the fix and passed afterwards; live metadata is checked against the same category enum. For the UI check the disposable Beta.4 driver was disabled through Plugin Center, its local files were replaced with the Beta.5 release build and manifest, and the driver was enabled through the UI. This candidate check is not evidence of downloading Beta.5 from the registry.

The same saved connection then opened successfully. UI-driven checks passed:

- Explorer loaded the fixture's tables, views, routines and triggers.
- SQL SELECT returned the expected value; CREATE TABLE, INSERT RETURNING and ALTER TABLE succeeded.
- BEGIN/UPDATE displayed the open-transaction indicator. ROLLBACK restored the original value; a later COMMIT persisted the replacement.
- An invalid SELECT rolled back pending changes, preserved the earlier committed value and cleared the transaction indicator.
- Stopping a long recursive cross-join query rolled back its pending update, cleared the indicator and allowed the next SELECT to return the committed value.
- Opening the created table displayed its columns/data. Editing a cell and applying the pending change persisted it. Deleting the row and applying it returned an empty grid; an independent SQL COUNT through the console confirmed zero rows.
- The temporary table was dropped through the console.
- Plugin Center removed the candidate after confirmation; its disposable driver directory was absent afterwards.

The launcher uses explicit disposable runtime paths plus `TABULARIS_DATA_DIR`, refuses OS secrets, disables legacy plugin/connection migration, binds only loopback and requires a temporary password. An initial environment-only launcher was stopped after it loaded existing connection metadata; no connections were opened or edited in that session. The corrected launcher began with zero connections. Hashes of the normal profile's config, connection and SSH-connection files were captured before the corrected run and checked afterwards.

After closing the browser and creating the stop marker, the launcher exited successfully. The Firebird server, UI host, plugin process, disposable profile, fixture directory and runtime-information file were absent. The three normal-profile file hashes matched their baseline; the Host working tree was clean.

Reproduce with `./tests/web-ui-firebird.ps1 -HostPath target/tabularis-host` after building that host's frontend. Runtime credentials and the stop-marker path are emitted only into ignored `target/web-ui-firebird-info.json` and its disposable profile. The launcher removes the profile, credentials, Firebird database/server and temporary Host example, and restores the Host manifest. Browser binaries used for automation are tooling cached under ignored `target/tooling`, not a Firebird installation.

Still separate: native Windows desktop IPC/OS integration, native Linux/macOS GUI and live ARM64/macOS checks, dedicated table/routine/trigger editor dialogs, opt-in PSQL UI execution after a compatible host/registry release, and long-term soak validation. The host companion patch remains a pending upstream PR.
