# Operational baseline

`windows-operational-baseline.json` records local validation of the Windows socket-readiness change on portable Firebird 3.0.14, 4.0.7 and 5.0.4. It was captured before the Beta.4 version-only bump, with the pending socket change applied and a **debug** plugin build. The PowerShell harness performs JSON serialization, parsing and result validation; request rate and latency therefore describe the complete harness roundtrip, not standalone database speed.

Each run terminates the entire temporary server with an uncommitted insert, observes failure through the existing plugin session, restarts the server, verifies rollback and resumes work without restarting the plugin. Five warmup queries precede the measured 30-second workload. Each request returns 1,000 rows with a 512-character text payload; the harness verifies the count, last key and text length. Working-set measurements belong to the plugin process, excluding the harness and server. The first sample is taken after warmup; retained allocator pages can change the final sample. The measurements do not establish a leak-free long-term bound or a portable throughput target.

Reproduce with `./tests/live-firebird.ps1 -Version 5 -OperationalTests` (also `3`/`4`). Add `-OperationalDurationSeconds 300` for a longer run. Fresh reports go to ignored `target/operations/`; Windows CI retains its own reports as artifacts. Temporary servers, databases, downloads and credentials are removed on success or failure.

Validation for this change includes the complete workspace tests and the repository's standard Clippy command. An additional strict Clippy run over the whole workspace reports 21 findings in unchanged vendored files; these are outside the socket change and were not suppressed or rewritten.
