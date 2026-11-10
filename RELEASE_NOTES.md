# Firebird driver 0.2.0-beta.6

Writable prerelease from [TabularisDB/tabularis-firebird-plugin](https://github.com/TabularisDB/tabularis-firebird-plugin). The published v0.1.0 remains the read-only stable release. This release does not claim complete database backup/restore or Stable readiness.

## Changes since Beta.5

- Complete binary BLOBs now use the host wire format with byte count, detected MIME type and base64 data. Add primary-key-guarded preview and file-export RPCs; existing byte-array input parameters remain accepted. No truncation or implicit filesystem references.
- Add Firebird single-row INSERT rendering for existing compatible tables, preserving binary/text BLOBs, Unicode and NULs, exact decimal strings, NULLs and supplied identity values while omitting computed columns. Rows over an 8 MiB SQL statement fail explicitly. This requires the companion [Host renderer/dump fix #1469](https://github.com/TabularisDB/tabularis/pull/1469).
- Add SELECT runtime measurement through `explain_query` with `analyze:true`. The default remains prepare-only. Measurements cover total rows and client wall-clock time including fetch/network work; per-node engine counters remain unavailable. Analysis executes expressions, whose generators/external routines can have effects beyond transactional table writes.
- Add exact, filtered and masked column statistics in a read-only snapshot transaction. BLOBs support counts only. The published manifest keeps `column_stats` disabled until the registry schema accepts it.
- Explicitly declare the `generic` SQL dialect, preventing the Host from treating Firebird as PostgreSQL. Unsupported guided trigger editing is safely rejected; actual SQL trigger editing and native RPCs work.
- Expand live CI to Linux x64/ARM64 clients with Firebird 3/4/5 and native macOS Intel/Apple Silicon with Firebird 5, including server crash/restart recovery. ARM64 clients use emulated x64 Firebird 3/4 servers; those two server builds are not native ARM64.
- Record native Windows Tauri/WebView2 tests of SQL editing, grid updates, transactions/error rollback, PSQL/routine SQL editing, BLOB/statistics/analysis IPC, SQL data roundtrip and signed Beta.4-to-Beta.5 installation/update/uninstall. This uses a private unreleased Host build; gated manifest flags were enabled only in the disposable test profile. See [native evidence](https://github.com/TabularisDB/tabularis-firebird-plugin/blob/main/tests/results/windows-native.md).
- Add manual two-hour Windows Firebird 3/4/5 soak jobs. A workflow definition or running job is not a completed measurement; retained results are reported separately.

## Remaining limits

- Full database SQL export/import including domains, routines, triggers, grants, dependency ordering and generator states is still open. The new INSERT renderer transfers data to existing compatible tables and is not a complete backup.
- The Host PSQL/cancellation patch is merged, but the latest published Host checked is still v0.32.0 without it. A compatible Host release and registry schema deployment are required before enabling the PSQL, routine-management, query-timeout and column-statistics integration flags.
- Native Linux/macOS desktop flows, macOS clients against Firebird 3/4, guided Firebird trigger editing and sustained-load validation remain separate completion items. Native file dialogs and installation UI buttons were not covered by the automated Tauri IPC checks.
- Arrays, nonstandard BLOB subtypes, embedded access and TLS configuration remain unsupported. Firebird wire encryption is implemented separately from TLS.
- Writes and explicit commits have real effects. Generator increments are not transactional, and unknown commit outcomes must not be automatically retried.

## Validation and packages

Formatting, workspace tests and plugin Clippy pass. The extended portable Windows suites passed Firebird 3.0.14, 4.0.7 and 5.0.4, including SQL data roundtrips with long binary/Unicode BLOBs, exact decimals, identity and computed columns. The 17-job native platform/live matrix passed on the Beta.6 runtime and dialect correction. Every temporary local server, database, credential and test profile was removed; normal Tabularis configuration hashes were unchanged.

The release workflow reruns its required checks before publishing Linux x64/ARM64, macOS x64/ARM64 and Windows x64 packages. Each ZIP includes executable, manifest and license notices. `default.tabularium` is the standalone manifest asset. GitHub publication does not confirm registry ingestion/signing or replace the remaining validation above.
