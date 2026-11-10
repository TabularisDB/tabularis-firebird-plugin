# Firebird — Tabularis Plugin

Firebird network driver for [Tabularis](https://github.com/TabularisDB/tabularis), built with a locally patched `rsfbclient-rust` wire driver. No Firebird client DLL is required. The stable v0.1.0 is read-only. The **0.2.0-beta.7 prerelease includes writes, interactive transactions, atomic batches, DDL, prepare-only query plans, roles and character sets**; see [DEVELOPMENT.md](DEVELOPMENT.md) for completed work and remaining limitations.

Development and future releases are maintained in [TabularisDB/tabularis-firebird-plugin](https://github.com/TabularisDB/tabularis-firebird-plugin).

Download the current [v0.2.0-beta.5 prerelease](https://github.com/TabularisDB/tabularis-firebird-plugin/releases/tag/v0.2.0-beta.5) for the new functionality. Its release notes describe compatibility changes and outstanding validation.

The [Tabularis registry entry](https://registry.tabularis.dev/plugins/firebird) is managed separately from GitHub releases; end-to-end registry install/update/uninstall verification remains on the release checklist. The historical [release v0.1.0](https://github.com/lejyfps/tabularis-firebird-plugin/releases/tag/v0.1.0) remains available from the original repository.

## Build and install

```powershell
cargo build --locked
just dev-install
```

Restart Tabularis or toggle the plugin in Settings. Configure:

- Host: your Firebird server (`localhost` for a local server).
- Port: `3050` by default.
- Database: on the **Databases** tab, enter a database alias or a file path **on the Firebird server** in the labeled input. Keep **All databases** selected to browse this configured database. Firebird cannot enumerate all databases on a server.
- Username and password: an existing Firebird database user. No default password is assumed.

The connection defaults to UTF-8 and SQL dialect 3. A local Firebird server is supported over TCP; embedded mode and TLS configuration are not implemented. Live integration tests pass against Firebird 3.0.14, 4.0.7 and 5.0.4 on Windows. Other patch versions are not individually validated; the driver requires Firebird 3+ with SRP authentication.

Development settings also expose `role` and `charset`, with per-connection overrides such as `extra: {"role":"READER","charset":"WIN1252"}`. An empty role clears the plugin default; quote case-sensitive names, for example `"Reader"` (including the quotes in the value). Roles must already be granted to the user. UTF8 is recommended; legacy character sets use strict encoding/decoding and reject unrepresentable text instead of replacing characters. NONE and OCTETS are not text-encoding options. Text BLOBs retain explicit character-set information, including long bound parameters, and are returned as UTF-8 JSON strings. Firebird still applies the connection character set: for example, an ASCII connection cannot read a Japanese text BLOB. Binary BLOB bytes are unchanged. Changing role or character set while a session transaction is open is rejected until it is finished or released.

The development branch requires Firebird wire encryption by default, preferring ChaCha64, then ChaCha, with Arc4 allowed for Firebird 3 compatibility. Plugin settings expose `wire_crypt` (`required`, `enabled`, `disabled`) and `allow_arc4` (default true). Disable Arc4 with `required` to require a modern cipher. `enabled` also accepts an unencrypted server; `disabled` explicitly requests plaintext and fails when the server requires encryption. Per-connection RPC options, or `extra: {"wire_crypt":"required","allow_arc4":"false"}`, override plugin defaults. Changing policy while an editor transaction is open is rejected until that session is finished or released. Wire encryption uses Firebird's native protocol; it does not enable the host's TLS settings.

For a local deployment, run a Firebird server on the same PC and connect to `127.0.0.1:3050` with an absolute local database path or an alias from that server's `databases.conf`. For a remote deployment, enter the server's hostname and a path or alias valid on that server, for example `/srv/firebird/customer.fdb` on Linux. The Firebird service account needs access to that file. The plugin does not create databases, install a server, or open `.fdb` files directly. Server firewall and authentication settings must permit the connection; only expose the port to the intended clients.

## Implemented

| Method | Status |
|---|---|
| `test_connection`, `ping` | Open a real connection and run `SELECT 1 FROM RDB$DATABASE`; failures return JSON-RPC errors. |
| `execute_query` | Execute a single SELECT, DML, DDL or procedure statement; commit successful writes, roll back failures, and return rows, affected count, host pagination and elapsed time. Optional `values` bind positional parameters. |
| `execute_query_batch` | Execute up to 1000 statements atomically by default; with `session_id`, explicit BEGIN/COMMIT/ROLLBACK boundaries are honored across calls. |
| Interactive sessions | Keep an explicit transaction on its editor's attachment; support savepoints and retaining operations. Release, errors, cancellation, timeouts and process EOF roll back pending changes. |
| `get_session_state` | Return `{in_transaction}` for a required `session_id`, without opening a connection or executing SQL. Missing sessions report false. Compatible hosts can refresh their indicator after cancellation. |
| `get_connection_metadata` | Discover the Firebird version and expose the appropriate SQL types; Firebird 4+ adds INT128, DECFLOAT and zoned temporal types. |
| `get_table_query_template` | Firebird SELECT previews with `FIRST`, quoted identifiers and optional row limits; UPDATE/DELETE previews include `WHERE 1 = 0` and do not enable execution of writes. |
| `get_databases` | Return the configured database after checking connectivity. Firebird has no server-wide SQL database catalog. |
| `get_tables`, `get_columns` | User tables, ordered columns, types, nullability, defaults, primary keys, identity flags and comments. |
| `get_indexes`, `get_foreign_keys` | Ordered index columns, unique/primary flags, expression source and composite foreign-key mappings with update/delete rules. |
| Schemas | Empty list; Firebird does not use SQL schemas. |
| `get_views`, `get_view_definition`, `get_view_columns` | User views, SQL source and ordered columns; view queries use the normal read-only execution path. |
| `get_routines`, `get_routine_parameters`, `get_routine_definition` | Top-level and public package routines, legacy UDF parameters, ordered IN/OUT arguments and PSQL/package source. |
| `build_routine_call_sql` | Firebird-specific call previews with catalog-ordered arguments, including selectable procedures and package functions. |
| `routine_create_template`, `get_routine_edit_script`, `drop_routine` | Creation templates, catalog-backed PSQL editing and type-specific drops. Preserve declarations, comments, grants and SQL SECURITY; package edits include the whole body and legacy UDF edits alter library bindings. Package-member drops and engine-specific UDR edits return explicit errors. |
| Trigger management | Discover table/database/DDL triggers, reconstruct PSQL definitions and create/alter/drop triggers with their status, position and events preserved. |
| `get_sequences` | Read initial values, increments and comments without advancing a sequence. This is an extension RPC; the host has no sequence explorer yet. |
| `get_schema_snapshot` | All user tables with columns and foreign keys for the ER diagram; views and system tables are excluded. |
| `get_all_columns_batch`, `get_all_foreign_keys_batch` | Metadata grouped by requested table name, reusing one connection per request. |
| Grid CRUD | Parameterized inserts and complete-primary-key updates/deletes; multi-row grid mutations roll back. |
| DDL and view management | Generate table/column/index/FK SQL, including composite keys and comments; drop indexes/FKs and create/alter/drop views. PK/identity status changes require explicit SQL. |
| `explain_query` | Prepare-only detailed plans as a host-compatible tree with original text. Runtime analysis is rejected; the query is never executed to explain it. |

Requests open their own connection unless `session_id` retains an explicit transaction for an editor. `execute_query` accepts the host's positive `page` and `limit` values (`page_size` remains an alias). The response includes `pagination: {page, page_size, total_rows, has_more}`, or `null` for an unpaged request. Pagination streams the full result to compute the total, retaining only the requested page in memory; large queries can therefore be slow. Without a page limit, all rows are returned up to the configured buffer limits. Use Firebird's `FIRST`/`SKIP` or `ROWS` clauses to bound expensive queries. Session behavior and transaction syntax are documented in [DEVELOPMENT.md](DEVELOPMENT.md).

Tabularis 0.32's data-grid limit field generates a generic `LIMIT n`, sometimes inside a `limited_subset` wrapper. The plugin translates those terminal clauses to Firebird `ROWS` syntax, with optional `OFFSET`, without treating quoted names, string literals or comments as SQL clauses. SQL templates already use native `FIRST` syntax.

The host's table **New Console** action also emits `SELECT * FROM "database"."table"`. The plugin removes that qualifier only for this exact query shape and only when it matches the connected database. Firebird SQL entered manually should use unqualified table names; other qualified expressions are passed through and may be rejected by Firebird.

Routine discovery includes public package members and legacy external UDFs. Package identities use separately quoted components such as `"PACKAGE"."ROUTINE"`; a literal dot inside a top-level name remains one quoted identifier. Private package members are excluded. A function's return value is an `OUT` parameter named `RETURN_VALUE`, listed before its inputs; unnamed legacy inputs use `ARGUMENT_n`. Parameters include the host-required `ordinal_position`. External routines without stored PSQL source return an explicit error for definition requests; packaged routines can return the containing package body when individual source is unavailable. Colliding procedure/function names carry `identity_args: "PROCEDURE"` or `"FUNCTION"`, which recent hosts forward on parameter/definition requests. Older clients without that discriminator receive an ambiguity error rather than another routine's data. Discovery does not execute routines.

Routine-management RPCs are implemented, but their UI capability remains disabled: released hosts can break Firebird PSQL at internal semicolons. [Host PR #1462](https://github.com/TabularisDB/tabularis/pull/1462) adds opt-in `sql_syntax.psql_blocks` support, tested through the actual editor splitter and this plugin against Firebird 3/4/5. The new flag and routine menus must wait for a compatible host release, deployed registry schema and native editor validation. The plugin explicitly extends generic SQL syntax. The complete PSQL statement must reach `execute_query` intact.

The runtime contract follows Tabularis's actual driver implementation: view details use `view_name`, routine details use `routine_name`, foreign keys use `name`/`ref_table`/`ref_column`/`seq_in_fk`, and indexes return one row per segment with `name`/`column_name`/`seq_in_index`. The older `view` and `routine` request names remain accepted. Some plugin-guide examples show older shapes; live tests deserialize responses against the required host fields in `src/host_contract_tests.rs`.

Batch metadata requests require a `tables` array of non-empty names without trailing spaces. Duplicate names are queried once, unknown names map to empty arrays, and an empty list returns `{}`. Names are bound parameters. Queries run in groups of up to 500 table names to stay below Firebird 3's IN-list limit. A schema snapshot uses table discovery and these same bulk queries, rather than connecting or querying separately for every table. Each catalog query uses its own read-only transaction; the snapshot is not atomic across concurrent schema changes.

Full logical database SQL export and restore are available through the optional
`get_database_sql_capabilities`, `export_database_sql` and `import_database_sql`
RPCs. The companion [Host PR #1469](https://github.com/TabularisDB/tabularis/pull/1469)
connects them to database dump/import and provides native trigger SQL editing for
generic dialects. Older hosts retain their previous transfer path.

The exporter reads one read-only snapshot and streams rows to an atomic, private
pending file. It includes domains, defaults/checks, collations, character-set
defaults, ordinary/GTT tables, views in dependency order, computed/identity
columns, sequences and counters, PSQL routines/packages, legacy UDF and BLOB-filter
declarations, indexes and composite constraints, triggers, comments, role system
privileges, grants and local authentication mappings. External library declarations
require the corresponding modules/plugins on the target server.

Restore validates a private snapshot of the entire file before connecting to the
target. Choose a new server-side path/alias to create a database, or an existing
empty database. It requires the source engine major version and object owner;
the attachment encoding may differ from the database default. Creation never
overwrites an existing database. A failed newly-created restore attempts bounded
cleanup and reports any cleanup failure; an existing empty target can remain
partly populated after a SQL error. Generator counters are nontransactional and
are sampled at the end of export. This is a logical SQL transfer, not a physical
backup of server configuration, authentication accounts or encryption keys.

Arrays, nonstandard BLOB subtypes, external-table target paths, mixed object owners,
unknown catalog encodings and missing expression source are rejected explicitly.
An unsupported export never replaces an existing backup. Individual SQL records
are limited to 8 MiB, including encoded data; use an engine backup for databases
outside these limits. Whole-schema export requires all user tables; data-only
requests retain the existing table renderer.

Result representation and remaining limitations:

- Column names come from prepared statement metadata, including for empty results and pages beyond the last row.
- Large integers outside JavaScript's exact integer range, and INT128 values, are returned as decimal strings.
- NUMERIC/DECIMAL values are returned as exact decimal strings, including zero-scale decimals and 38-digit values. Conversion happens on the server before transmission; no floating-point conversion is involved. FLOAT/DOUBLE values remain JSON numbers.
- DATE, TIME and TIMESTAMP are server-formatted strings preserving their original kind and fractional seconds. Firebird 3 formats TIMESTAMP dates as `29-FEB-2024`, while Firebird 4/5 use `2024-02-29`. WITH TIME ZONE values also retain their named zone or numeric offset; the plugin performs no conversion to the PC's time zone.
- DECFLOAT(16/34) values are decimal strings, preserving all digits and exponents. Special values such as `NaN`, `Infinity` and `-Infinity` are strings too, so the response remains valid JSON.
- Binary BLOBs use the host's complete `BLOB:<byte-count>:<mime>:<base64>` wire format; text BLOBs remain strings. Legacy byte arrays are still accepted as bound binary parameters. Arrays and BLOB subtypes other than binary/text remain unsupported and return errors.

The local driver source and its patches are documented in [vendor/rsfbclient-rust/README.md](vendor/rsfbclient-rust/README.md). It is MIT-licensed; releases include [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Metadata uses read-only transactions. User statements and batches use read-write transactions with explicit commit/rollback and resource cleanup. Unknown commit outcomes are reported explicitly and must not be blindly retried.

## Tests

### Beta.6 BLOB, export and analysis scope

- `fetch_blob_as_data_url` returns the complete host BLOB wire value for a binary/text BLOB selected by its **complete primary key**. NULL, missing rows, incomplete keys and ordinary scalar columns return errors. `save_blob_to_file` writes those bytes to a new file and refuses to overwrite an existing file. The host normally downloads through the fetch RPC.
- `render_insert_statements` renders one Firebird INSERT per row, quotes column names, omits computed columns, preserves supplied identity values, NULLs, exact decimal strings, Unicode text (including NULs), and complete binary BLOBs. Text and binary literals are distinguished using connection metadata. Oversized rows fail explicitly above an 8 MiB SQL statement; use parameterized transfers for larger values. This is a **data transfer to an existing compatible table**, not a complete database backup. Table schema reconstruction, dependent domains/routines/triggers/grants and identity-generator reseeding are outside this RPC. Import executes actual writes; test the target schema and constraints first.
- SQL export needs a Host that forwards bound connection parameters to the optional renderer. Full database data dumps additionally need the Host to call that renderer instead of generating generic multi-row VALUES. An older Host may fail the export; the plugin never guesses the source connection. The companion Host patch is required before advertising this workflow as supported in a released app.
- `explain_query` remains prepare-only by default. `analyze:true` executes a SELECT/WITH query in a read-only transaction and reports total rows and client wall-clock time, including fetch/network time. No result rows are retained and query cancellation/deadlines apply. Per-node engine counters remain unavailable. Analysis actually executes expressions: Firebird generators and external routines can have effects beyond transactional table writes.
- `get_column_stats` supports exact row/NULL counts, optional distinct/min/max/top values, a SQL filter and a bounded top list, within one read-only snapshot transaction. Disabled parts are never selected. BLOBs support counts only. The registry must accept `column_stats` before this can be enabled in the UI; the published manifest deliberately omits that flag until then.

```powershell
cargo test --workspace --locked
cargo clippy --all-targets --no-deps --locked -- -D warnings
cargo fmt --all -- --check
```

For a repeatable live test on Windows:

```powershell
./tests/live-firebird.ps1
# Check the pinned Firebird 3 or 4 release instead of the default 5:
./tests/live-firebird.ps1 -Version 3
./tests/live-firebird.ps1 -Version 4
# Optional if port 13050 is occupied:
./tests/live-firebird.ps1 -Port 13051
# Full process restart, rollback/recovery and 30-second measured load:
./tests/live-firebird.ps1 -Version 5 -OperationalTests
# Longer local run (seconds, at most 7200; opt-in two-hour run):
./tests/live-firebird.ps1 -Version 5 -OperationalTests -OperationalDurationSeconds 7200
```

The script downloads the official portable Firebird 3.0.14, 4.0.7 or 5.0.4 ZIP (verifying its pinned SHA-256), creates an isolated fixture database and random local test credentials under the ignored `target/firebird-test` directory, binds to `127.0.0.1`, and runs live catalog/query tests plus a stdio test of the executable. Its `finally` block stops its server and deletes the entire test directory, including the download, extracted server, database, lock files and credentials, on success or failure. It installs no service. It refuses to overwrite a pre-existing test directory. Windows CI runs the same script for all three versions.

Linux x64 CI runs the same live Rust suites with `bash tests/live-firebird.sh 3` (or `4`/`5`), using official Firebird Docker images pinned by digest. This requires Docker, Python 3, OpenSSL and Rust on the Linux runner. The script removes its container, anonymous data volume, temporary credentials and any image it newly pulled. The Windows development PC does not need Docker.

Native Linux ARM64 CI runs the same live suites against Firebird 3/4/5. Firebird 5 uses a separately pinned native ARM64 server image. Firebird 3/4 use the pinned official x64 server images through QEMU; the plugin client runs natively on ARM64, and the executable architecture is checked explicitly. The QEMU action and helper image are pinned and enabled only in those two CI jobs. This validates ARM64 client compatibility with real Firebird 3/4 servers, not native ARM64 server builds. Local ARM64 runs of those two versions require an existing x64 container emulator; this script never installs one. See [ARM64 client evidence](tests/results/arm64-compatibility.md).

Native macOS Intel and Apple Silicon CI run `bash tests/live-firebird-macos.sh` against Firebird 5.0.4. The launcher verifies the official architecture-specific package hash and extracts its framework into an owned temporary directory without executing installer scripts, creating users, loading a service or writing to `/Library/Frameworks`. Explicit framework/library and Firebird paths keep the fixture isolated. It creates random credentials, binds the server to loopback, runs the same live Rust and stdio suites, and stops the server and removes its package, database and credentials on exit. This needs macOS's `pkgutil`, Python 3, Rust and standard command-line tools; it installs nothing on the Windows development PC. See [macOS live evidence](tests/results/macos-live.md). Firebird 3/4 and native desktop UI validation remain separate macOS checks.

Both Linux and macOS launchers also run `tests/restart-firebird.py` inside their disposable fixture. This hard-kills the complete server with an open transaction, verifies failure/cleared transaction state, restarts on the same endpoint and checks rollback plus fresh committed writes through the same plugin process. An independent attachment confirms the new commit. Docker readiness uses an actual read-only SQL probe because its TCP proxy can accept connections before Firebird is ready; only readiness reads are retried. The helper owns the replacement macOS server and always stops it before fixture removal. See [server restart evidence](tests/results/server-restart.md). Never run the helper against a non-disposable database/server.

Windows CI also runs the operational checks on Firebird 3/4/5 and uploads JSON measurements from `target/operations/`. These report a bounded workload, not a universal memory or throughput guarantee. The plugin's working set is measured separately from the PowerShell harness. Server restart, database and credential cleanup remain inside the fixture's `finally` block.

After registry publication, `node tests/verify-registry.mjs 0.2.0-beta.3` checks the registry's Ed25519 signature, release identity and all six signed download hashes without modifying an installed plugin. This does not replace native install/update/uninstall testing.

To exercise actual Windows host application code for registry installation, update and removal:

```powershell
./tests/host-registry-lifecycle.ps1 -HostPath target/tabularis-host
```

This requires a Tabularis source checkout, Rust and the host's native build prerequisites (including Perl for vendored OpenSSL on Windows). The script temporarily compiles `tests/host/registry_lifecycle.rs` as a host example, restores the host manifest afterwards and removes the example and isolated application directories in `finally`. It never uses your installed plugin directory. It uses the real registry and published packages, tests rejected updates while the old driver remains runnable, and exercises update, disable/enable and uninstall through host application APIs. Override `-OldVersion` to select another published predecessor; the target version comes from this repository's manifest and must already be registry-signed. Build/output details are retained in ignored `target/host-registry-lifecycle.log`. This is a manual backend integration test, not a native GUI test or part of the default plugin CI.

For interactive Windows testing of the actual React UI, build the host checkout's frontend with its pinned pnpm version, then run `./tests/web-ui-firebird.ps1 -HostPath target/tabularis-host`. The launcher builds a temporary example using the real web server/application APIs, starts a disposable Firebird 5 fixture and writes `target/web-ui-firebird-info.json` with the loopback URL and paths to its temporary login password, database credentials and stop marker. Use those credentials only in the disposable UI; keep OS keychain storage unchecked. Create the indicated `stop_file` when finished. The launcher also expires automatically after 20 minutes (`-LifetimeSeconds`, maximum 30 minutes), stops its owned processes, restores the host manifest and removes the profile, database, server and credentials. Windows application-data environment variables alone do not isolate Known Folder paths: the example additionally supplies explicit runtime paths and `TABULARIS_DATA_DIR`, disables legacy plugin/connection migration and refuses OS credential access. This exercises the shared frontend and web backend, not native Tauri IPC. See [Windows UI evidence](tests/results/windows-web-ui.md) for the tested scope and the metadata regression found during this check.

To verify a host checkout containing the companion PSQL patch, install that host's dependencies with its pinned pnpm version and run the following from this plugin repository (Node must be on PATH):

```powershell
./tests/live-firebird.ps1 -Version 5 -AfterTests {
    node tests/host-psql-smoke.mjs ../tabularis
    if ($LASTEXITCODE -ne 0) { throw 'Host PSQL smoke test failed' }
}
```

The smoke test bundles the host's actual `resolveSqlSyntax` and `splitBatches`, then executes a procedure with a local function, a function, a trigger, package declarations/body and an EXECUTE BLOCK through plugin stdio. It checks results and session-state transitions. Use only the disposable fixture; the surrounding script removes its server and files even if this additional test fails. This does not replace native desktop UI testing.

Post-beta.1 regression coverage also exercises 100,001-row results, default-limit BLOB overflow, combined-batch limit rollback, eight parallel writers, row-lock conflicts, server-terminated attachments and loss of an already committed transaction's reply. Stdio tests verify queue overload, duplicate IDs and cancellation of queued work. These fixed-size correctness tests do not establish sustained throughput or a strict RSS bound.

For the live integration test, create a JSON connection file **outside the repository** with `host`, `port`, `database`, `username`, and `password`. Keep credentials out of source control. Then:

```powershell
$env:FIREBIRD_TEST_CONFIG = 'C:/path/to/firebird-test.json'
cargo test --locked live_connection_select_and_read_only_transaction -- --ignored
```

This single test checks connectivity, a SELECT result, the transaction's read-only flag and SQL error propagation against an existing database. It does not create a database or test instance. Use the portable script to also test metadata, domain defaults, composite keys, identifier binding, rejected writes and invalid credentials against `tests/fixture.sql`.

The live suite also covers empty result headers, exact positive/negative decimals, zero-scale decimals, ordinary floating-point values, NULLs, leap-day dates, four fractional-second digits and reusing a connection after a failed query. Firebird 4/5 additionally test INT128-backed decimals, named zones in summer and winter, a non-whole-hour offset, all 34 DECFLOAT digits, large exponents and special values, also mixed with a text BLOB. The script explicitly skips this newer-type test on Firebird 3, which lacks these types. Workspace tests include the vendored wire driver's tests; Clippy targets plugin code without linting third-party implementation details.

An installed Tabularis 0.32.0 was also checked in Web mode on Windows with an isolated profile and Firebird fixture: driver discovery, saved connection, table and view results, routine parameters/definition, the ER diagram with composite-FK edges, and the grid's explicit row limit. Native Windows desktop verification additionally covers creating a connection through the UI, the server-side database input, invalid-password diagnostics, successful connection-test logs, saving/reconnecting, table results and the table's New Console SELECT. Temporary profiles, plugin copies, databases and servers were removed afterward. The native host hides Generate SQL for read-only drivers; SELECT/UPDATE/DELETE previews are covered by live RPC tests instead. For additional host checks, `tests/live-firebird.ps1 -AfterTests { ... }` runs a callback while the fixture is alive and retains automatic cleanup.

For a direct JSON-RPC smoke test without a server:

```powershell
'{"jsonrpc":"2.0","method":"test_connection","params":{"params":{}},"id":1}' | cargo run --quiet --bin firebird-plugin
```

This must return an invalid-params error rather than success. `just repl` builds and starts the actual driver; enter a method name with empty params or a complete JSON-RPC request with your own connection params.

## Layout

- `src/client.rs`: Firebird connection and row conversion.
- `src/handlers/query.rs`: connection checks, query validation and pagination.
- `src/models.rs`: connection parameter validation.
- `src/rpc.rs`: JSON-RPC dispatch.
- `src/handlers/metadata.rs`: table, view and routine catalog discovery.
- `src/handlers/crud.rs`, `ddl.rs`: parameterized grid writes and Firebird structure management.

## Publishing

The shared package workflow builds and tests Linux x64/ARM64, macOS x64/ARM64 and Windows x64 bundles on native GitHub runners on every push/PR, on manual dispatch, and before a tagged release. Each ZIP includes the executable, `.tabularium`, Apache license and third-party notices. `tests/package.ps1` extracts each bundle and checks executable discovery, JSON-RPC initialization and invalid connection parameters. Packaging includes dotfiles explicitly, so the manifest is present on Windows too. The standalone manifest asset uses GitHub's normalized `default.tabularium` name, and publishing skips existing files to preserve registry-signed hashes. Do not rerun an older publishing workflow that overwrites release assets. A tag must equal `v` plus the Cargo/manifest version. Linux x64 builds use Ubuntu 22.04 (glibc 2.35); ARM64 uses Ubuntu 24.04 (glibc 2.39). Older Linux runtimes are not validated.

CI validates the manifest with `tests/validate_manifest.py` against the official registry driver schema captured on 2026-11-09 in `tests/driver-manifest.schema.json`. The snapshot avoids HTTP 403 responses from the registry to GitHub runners; refresh it from `https://registry.tabularis.dev/manifest.schema.json?kind=driver` when registry fields change. The strict schema forbids an IDE `$schema` field in the manifest itself.

The [public Firebird request](https://registry.tabularis.dev/api/requests/01M4VG6Z7G81RR5G9A694JCPGR) asks for connection setup, metadata inspection, query execution, type handling and documentation for local/server deployments. Its next step explicitly defines a first read-only feature set; type handling is marked as stretch. This implementation covers those functional areas using local/server TCP connections. The brief does not explicitly settle whether embedded mode is required for final acceptance; it is not implemented or advertised here.

Version 0.1.0 is published and approved in the [registry](https://registry.tabularis.dev/plugins/firebird), with five platform assets and registry integrity signatures. The public Firebird request is marked fulfilled; this status does not constitute a separate manual certification of every optional feature. For future versions, update the Cargo/manifest version together, review the candidate, then push its matching `v*` tag. Keep published tags and signed release assets unchanged.

## Remaining work

Remaining validation:

- The public brief defines a read-only first scope but does not explicitly settle embedded mode. Any additional maintainer acceptance requirements need separate confirmation; embedded access is not advertised here.
- Linux/macOS builds and extracted-package RPC checks are validated on native runners; live Firebird/Tabularis GUI checks on those operating systems remain outside the Windows validation performed here.

Known limits and optional extensions:

- Pagination still scans the full result for an exact count. Buffered results and BLOBs have configurable limits; host cancellation and connect/query deadlines are supported and tested on Firebird 3/4/5. Large-result performance needs further validation.
- Network connections use SQL dialect 3, with UTF-8 by default and configurable character sets and roles in development builds. Embedded mode and TLS configuration remain unsupported. Modern Firebird wire encryption is implemented separately from TLS.
- Private package routines, arrays and nonstandard BLOB subtypes remain unsupported. Procedure/function name collisions require the returned identity discriminator. Full routine management and native trigger/editor UX still need validation.
- BLOB preview/export, SQL data rendering, runtime SELECT measurements and column statistics are implemented in Beta.6. See the scope and Host requirements below.
- Tabularis 0.32 displays a Create View action for the published read-only v0.1.0, which rejects view mutations. The writable development branch implements view mutations.
- The published v0.1.0 remains read-only. The 0.2.0 betas enable writes and interactive transactions; native desktop tests of the new editing/DDL/session flows are still outstanding. Shared React/web UI coverage is recorded in [Windows UI evidence](tests/results/windows-web-ui.md).
- The local wire-driver patches need maintenance until equivalent upstream APIs are available.

## References

- [Plugin guide](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md)
- [Manifest schema](https://github.com/TabularisDB/tabularis/blob/main/plugins/manifest.schema.json)

## License

Apache-2.0
