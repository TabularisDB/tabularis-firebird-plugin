# Firebird driver 0.2.0-beta.5

Writable prerelease from [TabularisDB/tabularis-firebird-plugin](https://github.com/TabularisDB/tabularis-firebird-plugin). Earlier release assets and tags remain unchanged; the previously published v0.1.0 is the read-only stable release in the original repository.

## Changes since 0.2.0-beta.4

- Fix connection startup in hosts that validate connection-local data types. Firebird's `BOOLEAN` type now uses the supported `other` category; the SQL type and values remain Boolean. The previous `boolean` category made the host reject metadata after a successful connection test. Contract tests now reject unsupported categories in both the static manifest and live metadata responses.
- Add an isolated Windows launcher for testing the actual React frontend, web transport and application APIs with disposable Firebird. Explicit runtime paths and a secrets provider that refuses OS credential access protect the normal profile. Native Tauri transport and Linux/macOS GUI validation remain separate checks.

## Changes since 0.2.0-beta.3

- Fix Windows cancellation when a TCP write is blocked by a full send buffer. Cancellable sockets use nonblocking reads/writes and bounded WSAPoll readiness waits. Waiting does not add a query deadline or replay bytes after a genuine I/O failure. A regression first establishes stalled send progress, then requires cancellation within two seconds.
- Add disposable Windows operational tests on Firebird 3/4/5: terminate the complete server with an open transaction, verify the plugin reports failure, restart the server, check rollback and resume work in the same plugin process. All three local versions passed.
- Record a bounded 30-second workload per engine with 1,000 rows and 512-character payloads per request. CI retains JSON reports for request rate, p50/p95 latency and the plugin working set. Durations up to ten minutes can be requested locally. These measurements are not long-term performance guarantees.
- Add a dependency-free registry verification script for the Ed25519 JWS, signed release identity and all six signed download hashes. Verification passed for the registry's approved Beta.3 entry; native registry install/update/uninstall testing remains outstanding.

## Changes since 0.2.0-beta.1

- Fix Windows receives that can remain blocked after socket shutdown. A shared attachment cancellation signal is checked between short receive waits, including during authentication and session reuse. Ordinary receive waits keep retrying until data or cancellation arrives, so an unlimited query does not acquire a new query deadline. Failed writes are not retried.
- Add optional `get_session_state` with a required `session_id`. It returns the actual retained transaction state without opening a connection or executing SQL; unknown sessions report false. Compatible hosts can refresh their transaction indicator after a cancelled or failed call.
- Explicitly extend generic SQL syntax instead of inheriting the host's PostgreSQL defaults. Firebird PSQL execution support is prepared in [host PR #1462](https://github.com/TabularisDB/tabularis/pull/1462); its syntax flag and routine menus remain disabled pending a compatible host release, registry schema and native UI validation.
- Add bounded workload and failure-injection tests: large row/BLOB results, combined batch limits and rollback, concurrent writers and row-lock conflicts, loss of an already-committed COMMIT reply, server attachment termination, and stdio request-queue pressure. Unknown commit outcomes are never replayed.
- Run live Firebird 3/4/5 suites on native Linux x64 CI using pinned official Docker images, alongside the Windows suites. Verify the real host editor splitter and plugin stdio together with procedures/local routines, functions, triggers, packages and EXECUTE BLOCK on Firebird 3/4/5.

## Included from 0.2.0-beta.1

- Execute parameterized INSERT/UPDATE/DELETE, RETURNING and atomic query batches. Grid mutations require the complete primary key and roll back if they match multiple rows. Unknown commit outcomes are reported without retrying.
- Keep interactive transactions on the editor's session, including savepoints, isolation options and retaining operations. Errors, cancellation, deadlines, session release and process exit roll back pending changes.
- Interrupt matching requests and honor connect/query timeouts. Bound retained rows, JSON result size and decoded BLOB size; oversized results fail without returning partial data.
- Generate table, column, index, foreign-key and view DDL. Discover Firebird 4+ types, public package routines, legacy UDF parameters, triggers and sequence metadata. Add routine-call/template/edit/drop and trigger-management RPCs.
- Return detailed prepare-only execution plans without executing the statement.
- Negotiate ChaCha64/ChaCha encryption, retaining Arc4 compatibility for Firebird 3. Require wire encryption by default, with explicit policy overrides. Only Firebird 3+ with SRP authentication is supported.
- Configure SQL roles and connection character sets globally or per connection. Preserve non-ASCII metadata names and mixed-charset text BLOBs, including long bound text; binary bytes remain unchanged. Unrepresentable characters fail explicitly.

## Compatibility and limitations

- This beta enables writes. Check transaction boundaries before running statements; explicit earlier commits cannot be undone by a later batch failure. Firebird generator increments are not transactional.
- Servers with wire encryption disabled require an explicit `wire_crypt` setting of `enabled` or `disabled`. Arc4 can be disabled for modern-only connections. Firebird wire encryption is separate from TLS; embedded mode and TLS configuration are unsupported.
- Released hosts can break Firebird PSQL bodies. Routine-management menus remain disabled; send complete statements through the RPC interface. The companion host patch is not included in these plugin archives. Native editor/DDL/transaction UX, long-term load/soak validation, server-restart validation outside Windows and live database validation on Linux ARM64/macOS remain outstanding.
- Dedicated BLOB export/preview, lossless SQL export/import, runtime query analysis, arrays and custom BLOB subtypes are not implemented. Primary-key/identity status changes require explicit SQL.
- Host query-timeout UI support depends on the registry schema; use the plugin timeout setting or explicit connection parameter. Older hosts that discard a cancelled response can temporarily show stale transaction state. The new state RPC requires a compatible host before it can refresh that indicator.
- UTF8 is recommended. Firebird applies the chosen connection character set and rejects text it cannot represent; other legacy codecs have not each had a language-specific live roundtrip.

See [DEVELOPMENT.md](https://github.com/TabularisDB/tabularis-firebird-plugin/blob/v0.2.0-beta.5/DEVELOPMENT.md) for the complete checklist and transaction semantics.

## Validation and packages

Local workspace tests, formatting and Clippy pass. Portable Windows and native Linux x64 tests against Firebird 3/4/5 cover writes/rollback, sessions, metadata, DDL, plans, cancellation/timeouts, roles, text/BLOB encoding, encryption policies and failure injection. Windows fixtures use Firebird 3.0.14, 4.0.7 and 5.0.4. Temporary test servers, databases and credentials are removed after testing.

The release workflow must pass all checks before publishing: native build/unit/protocol/package tests for Linux x64/ARM64, macOS x64/ARM64 and Windows x64, plus live Firebird 3/4/5 tests. Each ZIP includes the executable, manifest and license notices; `default.tabularium` is the standalone manifest asset. GitHub publication does not itself confirm registry ingestion or registry signing.
