# Driver completion (unreleased)

The published v0.1.0 tag and registry-signed assets remain unchanged. Work continues on `feat/driver-completion`; completed items below are development code, not a new published release.

- [x] Host `cancel` notifications interrupt the matching request, including a stalled authentication handshake. No notification response is emitted; queued requests remain usable.
- [x] Honor `connect_timeout_secs` (default 10) and `query_timeout_secs` (default 0, unlimited). Zero disables the corresponding deadline.
- [x] Abort only the cancelled request's dedicated attachment. Live Firebird 3/4/5 tests verify the attachment disappears from MON$ATTACHMENTS after cancellation/timeout.
- [x] Configurable buffer limits: 100,000 retained rows, 64 MiB of row/header JSON, and 8 MiB per decoded BLOB by default. Limits reject the result instead of returning partial data. BLOB results fetch one row per batch to bound prefetching.
- [ ] Safe writes, typed parameters, affected counts, RETURNING and transaction semantics.
- [ ] Table/column/key/index/view management and complete SQL type catalog.
- [ ] Package routines, triggers, sequences, routine calls and ambiguity handling.
- [ ] EXPLAIN, column statistics, BLOB export/preview and lossless SQL export/import.
- [ ] Modern wire encryption, roles and character-set options; explicitly decide embedded and legacy-version scope.
- [ ] Large/concurrent/failure-path workloads, restricted-user tests and native Linux/macOS live validation.
- [ ] Scaffold cleanup, wire-patch maintenance, end-to-end registry install/update/uninstall and new-release verification.

Cancellation uses Firebird's documented `fb_cancel_abort` behavior (socket shutdown), because each request owns its attachment. It does not preserve an aborted connection. TCP connect waits until the configured connect deadline or OS result; a blocked OS DNS resolver can finish in the background after the caller has stopped waiting. The deadline begins before DNS and authentication. Results are still fully scanned for their exact count; set a query deadline or use SQL row limits for costly queries.

This is a bounded-memory policy for decoded BLOBs and retained results, not a strict process-RSS limit. JSON serialization, wire buffers and the current row add transient allocations. These defaults apply to metadata as well as query results and can be changed in plugin settings. Long-running metadata statements honor the same request cancellation and query deadline.
