# Native Windows desktop validation, 2026-11-10

The Beta.6 runtime (`894d833`, followed by manifest correction `54d5eed`) was exercised in a real Tauri/WebView2 desktop build of Host main `a4cb02af`, including the local SQL-dump/renderer patch. The React assets came from the earlier `ec8590e` Host build. This is a development integration check, not validation of a published Host release or every current-main frontend change.

The test build used an explicit disposable `FixedRuntimePaths`, OS-secret operations denied, a separate application identifier and WebView2 profile, and disabled URI-scheme registration. Normal `config.json`, `connections.json` and `ssh_connections.json` hashes were unchanged. The Firebird server, database, credentials, installed plugins, WebView2 profile, test application executable and its empty application directory were removed after testing.

Passed:

- Real native IPC: save a test connection, execute SQL and discover the database in Explorer.
- SQL editor buttons: CREATE TABLE, INSERT, SELECT and visible results; BEGIN/ROLLBACK; COMMIT visible through a separate attachment; query failure rolls back a pending insert.
- Native table grid: open the table, edit a text cell, submit changes and independently confirm persistence.
- SQL editor PSQL: create and run a procedure containing internal semicolons; open its actual routine Edit action, change ALTER PROCEDURE and independently verify changed execution.
- SQL editor trigger creation, actual trigger firing and native retrieval of the complete definition.
- Native IPC: exact binary BLOB fetch, masked/exact column statistics and SELECT runtime measurements.
- Native IPC: SQL data dump and import of an existing compatible table preserve text and complete binary BLOB bytes.
- Native IPC: signed registry Beta.4 installation, update to Beta.5, SQL execution by the installed Beta.5, disable/enable and uninstall.

Native keyboard input was not used to populate the hidden Monaco window: its existing editor model API set the SQL text, then the actual UI buttons ran it. Ordinary grid input and submit buttons were exercised. IPC installation calls were automated directly; installation dialogs/buttons and native file-chooser interactions were not covered.

The candidate manifest privately enabled `routine_management`, `psql_blocks`, `query_timeout` and `column_stats` to test the unreleased integration. These flags remain absent from the published manifest pending the compatible Host/schema deployment.

The trigger GUI exposed an incorrect PostgreSQL fallback when no `sql_dialect` was declared. `54d5eed` explicitly declares `generic`; the same native check confirmed the modal now refuses the unsupported guided editor. Firebird triggers can be edited through actual SQL and the plugin RPCs; a complete Firebird trigger GUI remains open.

Full database SQL export/import (domains, routines, triggers, grants, dependency order and generator states) is **not** covered by the table-data roundtrip. Native Linux/macOS desktop flows and a new release's signed installation require separate validation.
