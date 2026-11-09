# Firebird driver 0.2.0-beta.1

First prerelease from [TabularisDB/firebird-plugin](https://github.com/TabularisDB/firebird-plugin), containing the current driver-completion work. This is a writable beta; the previously published v0.1.0 remains the read-only stable release in the original repository.

## Changes since 0.1.0

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
- The host's generic SQL splitter can break Firebird PSQL bodies. Routine-management menus remain disabled; send complete statements through the RPC interface. Native editor/DDL/transaction UX and Linux/macOS live database validation remain outstanding.
- Dedicated BLOB export/preview, lossless SQL export/import, runtime query analysis, arrays and custom BLOB subtypes are not implemented. Primary-key/identity status changes require explicit SQL.
- Host query-timeout UI support depends on the registry schema; use the plugin timeout setting or explicit connection parameter. A host that discards a cancelled response can temporarily show stale transaction state, although the transaction has been aborted.
- UTF8 is recommended. Firebird applies the chosen connection character set and rejects text it cannot represent; other legacy codecs have not each had a language-specific live roundtrip.

See [DEVELOPMENT.md](https://github.com/TabularisDB/firebird-plugin/blob/v0.2.0-beta.1/DEVELOPMENT.md) for the complete checklist and transaction semantics.

## Validation and packages

Local workspace tests, formatting and Clippy pass. Portable Windows tests against Firebird 3.0.14, 4.0.7 and 5.0.4 cover writes/rollback, sessions, metadata, DDL, plans, cancellation/timeouts, roles, text/BLOB encoding and encryption policies. Temporary test servers and databases are removed after testing.

The release workflow must pass all checks before publishing: native build/unit/protocol/package tests for Linux x64/ARM64, macOS x64/ARM64 and Windows x64, plus live Firebird 3/4/5 tests. Each ZIP includes the executable, manifest and license notices; `default.tabularium` is the standalone manifest asset. GitHub publication does not itself confirm registry ingestion or registry signing.
