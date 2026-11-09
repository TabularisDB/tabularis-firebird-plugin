# Driver completion (unreleased)

The published v0.1.0 tag and registry-signed assets remain unchanged. Work continues on `feat/driver-completion`; completed items below are development code, not a new published release.

- [x] Host `cancel` notifications interrupt the matching request, including a stalled authentication handshake. No notification response is emitted; queued requests remain usable.
- [x] Honor `connect_timeout_secs` (default 10) and `query_timeout_secs` (default 0, unlimited). Zero disables the corresponding deadline.
- [ ] Enable the host's dedicated query-timeout connection field once the deployed registry schema permits `capabilities.query_timeout`. For now the plugin setting `query_timeout_secs` supplies the default; an explicit host connection value overrides it, including zero. The current registry rejects that capability flag, so it is deliberately omitted from the static manifest.
- [x] Abort only the cancelled request's dedicated attachment. Live Firebird 3/4/5 tests verify the attachment disappears from MON$ATTACHMENTS after cancellation/timeout.
- [x] Configurable buffer limits: 100,000 retained rows, 64 MiB of row/header JSON, and 8 MiB per decoded BLOB by default. Limits reject the result instead of returning partial data. BLOB results fetch one row per batch to bound prefetching.
- [x] Parameterized INSERT/UPDATE/DELETE, complete-PK validation, affected counts, exact decimal parameters, RETURNING, automatic rollback on errors and row-count violations.
- [x] Atomic query batches with combined result limits; no implicit retry after an unknown commit outcome.
- [ ] Pinned interactive transactions across separate requests (explicit BEGIN/COMMIT/ROLLBACK remain rejected).
- [x] SQL builders for tables, columns, indexes and composite foreign keys; column comments and view create/alter/drop. Primary-key/identity status changes currently require explicit SQL.
- [x] Firebird type catalog, with additional Firebird 4+ types discovered per connection.
- [ ] Package routines, triggers, sequences, routine calls and ambiguity handling.
- [x] Prepare-only detailed execution plans, rendered as a host-compatible tree with the original text retained. Tested on Firebird 3/4/5, including DML with no side effects. Runtime analysis is explicitly rejected; unavailable costs and row estimates remain null.
- [ ] Runtime analysis, column statistics, BLOB export/preview and lossless SQL export/import.
- [ ] Modern wire encryption, roles and character-set options; explicitly decide embedded and legacy-version scope.
- [ ] Large/concurrent/failure-path workloads, restricted-user tests and native Linux/macOS live validation.
- [ ] Scaffold cleanup, wire-patch maintenance, end-to-end registry install/update/uninstall and new-release verification.

Cancellation uses Firebird's documented `fb_cancel_abort` behavior (socket shutdown), because each request owns its attachment. It does not preserve an aborted connection. TCP connect waits until the configured connect deadline or OS result; a blocked OS DNS resolver can finish in the background after the caller has stopped waiting. The deadline begins before DNS and authentication. Results are still fully scanned for their exact count; set a query deadline or use SQL row limits for costly queries.

This is a bounded-memory policy for decoded BLOBs and retained results, not a strict process-RSS limit. JSON serialization, wire buffers and the current row add transient allocations. These defaults apply to metadata as well as query results and can be changed in plugin settings. Long-running metadata statements honor the same request cancellation and query deadline.

The development manifest enables writes. Metadata still uses read-only transactions. A single user statement commits once after successful execution/fetch; a failed statement, result-limit violation or grid mutation affecting more than one row rolls back. `execute_query_batch` executes up to 1000 statements in one transaction and rolls back the entire batch on error. Its timeout covers the full batch, including commit; Firebird may reject DDL/DML combinations that require a metadata commit between statements. A commit whose reply is lost reports an unknown outcome and must not be automatically retried. Counters such as identity generators are not rolled back by Firebird.
