# Firebird — Tabularis Plugin

Read-only Firebird network driver for [Tabularis](https://github.com/TabularisDB/tabularis), built with `rsfbclient`'s pure Rust wire driver. No Firebird client DLL is required. This is an initial implementation, not a complete driver yet.

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
| Views, routines | Not implemented; disabled in the manifest. |
| Batch metadata / schema snapshot | Return method-not-found so the host can fall back to standard metadata calls. |
| CRUD, DDL, EXPLAIN | Not implemented. The manifest disables editing and table management. |

Each request opens its own connection. `execute_query` accepts optional positive `page` and `page_size` values. Pagination streams the full result to compute `total_count`, retaining only the requested page in memory; large queries can therefore be slow. Without `page_size`, all rows are returned. Use Firebird's `FIRST`/`SKIP` or `ROWS` clauses to bound expensive queries.

Current result limitations:

- An empty result has no column names because the driver's high-level API exposes them through fetched rows.
- Large integers outside JavaScript's exact integer range, and INT128 values, are returned as decimal strings.
- The upstream driver converts NUMERIC/DECIMAL to floating point, which can lose precision. Use `CAST(amount AS VARCHAR(100))` when exact digits matter. This must be addressed before financial use.
- Dates and times use the driver's timestamp representation. Binary BLOBs are JSON byte arrays; text BLOBs are strings.
- Some newer Firebird types, including DECFLOAT and time-zone types, are unsupported by the upstream reader and return errors.

See the [upstream type mappings](https://docs.rs/rsfbclient/0.29.0/rsfbclient/#data-type-mappings).

## Tests

```powershell
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

For a repeatable live test on Windows:

```powershell
./tests/live-firebird.ps1
# Optional if port 13050 is occupied:
./tests/live-firebird.ps1 -Port 13051
```

The script downloads the official portable Firebird 5.0.4 ZIP (verifying a pinned SHA-256), creates an isolated fixture database and random local test credentials under the ignored `target/firebird-test` directory, binds to `127.0.0.1`, runs live catalog/query tests plus a stdio test of the executable, then stops its server. It installs no service. Each run keeps its files under `target` for inspection. Windows CI runs the same script.

For the live integration test, create a JSON connection file **outside the repository** with `host`, `port`, `database`, `username`, and `password`. Keep credentials out of source control. Then:

```powershell
$env:FIREBIRD_TEST_CONFIG = 'C:/path/to/firebird-test.json'
cargo test --locked live_connection_select_and_read_only_transaction -- --ignored
```

This single test checks connectivity, a SELECT result, the transaction's read-only flag and SQL error propagation against an existing database. It does not create a database or test instance. Use the portable script to also test metadata, domain defaults, composite keys, identifier binding, rejected writes and invalid credentials against `tests/fixture.sql`.

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
- `src/handlers/metadata.rs`: catalog discovery; views and routines remain scaffold methods.
- `src/handlers/crud.rs`, `ddl.rs`: remaining scaffold methods.

## Publishing

The included release workflow builds Linux, macOS and Windows bundles when a `v*` tag is pushed. Complete metadata support and live validation before publishing. Submit the released plugin to the Tabularis registry for the [Firebird bounty](https://tabularis.dev/plugins/bounties/firebird).

## References

- [Plugin guide](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md)
- [Manifest schema](https://github.com/TabularisDB/tabularis/blob/main/plugins/manifest.schema.json)

## License

Apache-2.0
