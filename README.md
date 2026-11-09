# Firebird — Tabularis Plugin

Read-only Firebird network driver for [Tabularis](https://github.com/TabularisDB/tabularis), built with a locally patched `rsfbclient-rust` wire driver. No Firebird client DLL is required. This is an initial implementation, not a complete driver yet.

## Build and install

```powershell
cargo build --locked
just dev-install
```

Restart Tabularis or toggle the plugin in Settings. Configure:

- Host: your Firebird server (`localhost` for a local server).
- Port: `3050` by default.
- Database: a database alias or a file path **on the server**, not on the plugin machine.
- Username and password: an existing Firebird database user. No default password is assumed.

The connection uses UTF-8 and SQL dialect 3. A local Firebird server is supported over TCP; embedded mode and TLS configuration are not implemented. Live integration tests pass against Firebird 5.0.4 on Windows. Other versions are not yet validated; the metadata queries require Firebird 3+ catalogs.

## Implemented

| Method | Status |
|---|---|
| `test_connection`, `ping` | Open a real connection and run `SELECT 1 FROM RDB$DATABASE`; failures return JSON-RPC errors. |
| `execute_query` | Execute a query in a read-only transaction; return columns, rows, total count and elapsed milliseconds. |
| `get_databases` | Return the configured database after checking connectivity. Firebird has no server-wide SQL database catalog. |
| `get_tables`, `get_columns` | User tables, ordered columns, types, nullability, defaults, primary keys, identity flags and comments. |
| `get_indexes`, `get_foreign_keys` | Ordered index columns, unique/primary flags, expression source and composite foreign-key mappings with update/delete rules. |
| Schemas | Empty list; Firebird does not use SQL schemas. |
| `get_views`, `get_view_definition`, `get_view_columns` | User views, SQL source and ordered columns; view queries use the normal read-only execution path. |
| `get_routines`, `get_routine_parameters`, `get_routine_definition` | Top-level stored procedures and functions, ordered IN/OUT parameters, function return values and PSQL bodies. |
| `get_schema_snapshot` | All user tables with columns and foreign keys for the ER diagram; views and system tables are excluded. |
| `get_all_columns_batch`, `get_all_foreign_keys_batch` | Metadata grouped by requested table name, reusing one connection per request. |
| CRUD, DDL, EXPLAIN | Not implemented. The manifest disables editing and table management. |

Each request opens its own connection. `execute_query` accepts optional positive `page` and `page_size` values. Pagination streams the full result to compute `total_count`, retaining only the requested page in memory; large queries can therefore be slow. Without `page_size`, all rows are returned. Use Firebird's `FIRST`/`SKIP` or `ROWS` clauses to bound expensive queries.

Routine discovery excludes package members and legacy external UDFs. A function's return value is an `OUT` parameter named `RETURN_VALUE`, listed before its inputs. External routines without stored PSQL source return an explicit error for definition requests. If a procedure and function share a name, detail requests return an ambiguity error because the host supplies no routine-type discriminator. Routine discovery does not execute the routines.

Batch metadata requests require a `tables` array of non-empty names without trailing spaces. Duplicate names are queried once, unknown names map to empty arrays, and an empty list returns `{}`. Names are bound parameters. Queries run in groups of up to 500 table names to stay below Firebird 3's IN-list limit. A schema snapshot uses table discovery and these same bulk queries, rather than connecting or querying separately for every table. Each catalog query uses its own read-only transaction; the snapshot is not atomic across concurrent schema changes.

Result representation and remaining limitations:

- Column names come from prepared statement metadata, including for empty results and pages beyond the last row.
- Large integers outside JavaScript's exact integer range, and INT128 values, are returned as decimal strings.
- NUMERIC/DECIMAL values are returned as exact decimal strings, including zero-scale decimals and 38-digit values. Conversion happens on the server before transmission; no floating-point conversion is involved. FLOAT/DOUBLE values remain JSON numbers.
- DATE, TIME and TIMESTAMP are server-formatted strings preserving their original kind and fractional seconds. WITH TIME ZONE values also retain their named zone or numeric offset; the plugin performs no conversion to the PC's time zone.
- DECFLOAT(16/34) values are decimal strings, preserving all digits and exponents. Special values such as `NaN`, `Infinity` and `-Infinity` are strings too, so the response remains valid JSON.
- Binary BLOBs are JSON byte arrays; text BLOBs are strings. Arrays and BLOB subtypes other than binary/text remain unsupported and return errors.

The local driver source and its patches are documented in [vendor/rsfbclient-rust/README.md](vendor/rsfbclient-rust/README.md). It is MIT-licensed; releases include [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Queries use an explicit read-only transaction, accept only SELECT statement types, and release statements and transactions on success or error.

## Tests

```powershell
cargo test --workspace --locked
cargo clippy --all-targets --no-deps --locked -- -D warnings
cargo fmt --all -- --check
```

For a repeatable live test on Windows:

```powershell
./tests/live-firebird.ps1
# Optional if port 13050 is occupied:
./tests/live-firebird.ps1 -Port 13051
```

The script downloads the official portable Firebird 5.0.4 ZIP (verifying a pinned SHA-256), creates an isolated fixture database and random local test credentials under the ignored `target/firebird-test` directory, binds to `127.0.0.1`, and runs live catalog/query tests plus a stdio test of the executable. Its `finally` block stops its server and deletes the entire test directory, including the download, extracted server, database, lock files and credentials, on success or failure. It installs no service. It refuses to overwrite a pre-existing test directory. Windows CI runs the same script.

For the live integration test, create a JSON connection file **outside the repository** with `host`, `port`, `database`, `username`, and `password`. Keep credentials out of source control. Then:

```powershell
$env:FIREBIRD_TEST_CONFIG = 'C:/path/to/firebird-test.json'
cargo test --locked live_connection_select_and_read_only_transaction -- --ignored
```

This single test checks connectivity, a SELECT result, the transaction's read-only flag and SQL error propagation against an existing database. It does not create a database or test instance. Use the portable script to also test metadata, domain defaults, composite keys, identifier binding, rejected writes and invalid credentials against `tests/fixture.sql`.

The live suite also covers empty result headers, exact positive/negative decimals and INT128-backed decimals, zero-scale decimals, ordinary floating-point values, NULLs, and reusing a connection after a failed query. Temporal/DECFLOAT coverage includes leap-day dates, four fractional-second digits, named zones in summer and winter, a non-whole-hour offset, all 34 DECFLOAT digits, large exponents and special values, also mixed with a text BLOB. These newer types require Firebird 4+; the portable suite uses 5.0.4. Workspace tests include the vendored wire driver's tests; Clippy targets plugin code without linting third-party implementation details.

For a direct JSON-RPC smoke test without a server:

```powershell
'{"jsonrpc":"2.0","method":"test_connection","params":{"params":{}},"id":1}' | cargo run --quiet --bin firebird-plugin
```

This must return an invalid-params error rather than success. The generated `just repl` command only echoes request shapes; it does not invoke the driver.

## Layout

- `src/client.rs`: Firebird connection and row conversion.
- `src/handlers/query.rs`: connection checks, query validation and pagination.
- `src/models.rs`: connection parameter validation.
- `src/rpc.rs`: JSON-RPC dispatch.
- `src/handlers/metadata.rs`: table, view and routine catalog discovery.
- `src/handlers/crud.rs`, `ddl.rs`: remaining scaffold methods.

## Publishing

The included release workflow builds Linux, macOS and Windows bundles when a `v*` tag is pushed. Complete metadata support and live validation before publishing. Submit the released plugin to the Tabularis registry for the [Firebird bounty](https://tabularis.dev/plugins/bounties/firebird).

## References

- [Plugin guide](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md)
- [Manifest schema](https://github.com/TabularisDB/tabularis/blob/main/plugins/manifest.schema.json)

## License

Apache-2.0
