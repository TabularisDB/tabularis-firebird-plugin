# Beta.7 validation and deployment

The candidate implements whole-database logical SQL transfers and OCTETS/BINARY
values. The companion Host PR #1470 negotiates optional whole-file RPCs through
both transports and adds a complete-statement trigger SQL editor for generic
dialects. Existing drivers retain their default transfer implementation.

Local validation includes Windows Firebird 3.0.14, 4.0.7 and 5.0.4 roundtrips of
domains/defaults/checks, Unicode and binary/text BLOBs, raw OCTETS, a different
database default charset, custom collations and charset defaults, identity and
sequence start values/counters, routine/package computed columns, reordered view
dependencies, GTTs, composite constraints/backing indexes, PSQL and legacy UDFs,
BLOB-filter declarations, role system privileges/default roles, grants, mappings
and active DDL triggers. Existing/nonempty targets are protected; a failed newly
created restore is removed. Workspace and strict plugin Clippy checks pass.

Host checks include 21 React tests, TypeScript/build/lint checks and 149 focused
backend tests. New cases cover whole-file dispatch without metadata attachment
or PSQL splitting, preservation of old exports, prevention of generic SQL
fallback on database creation, browser path rejection, protocol fallback codes,
explicit restore destinations and preservation of triggers on save failure.

## External deployment requirements

The Host PR is open; this session has read access, without merge permission.
The deployed registry driver schema still omits `column_stats`, `query_timeout`
and `sql_syntax.psql_blocks`. Registry source inspection confirms these are
operator-managed kind-extension settings. Apply
`registry-driver-capabilities.patch.json` as JSON Patch to the driver's
`extensionsSchema`, retaining its other fields, then refresh the plugin manifest
after deploying a compatible Host release. Do not replace the entire extension
object or publish stripped capability flags.

Windows native WebView/IPC checks pass for editor execution, trigger creation,
immediate edit/save, metadata preservation, failed-save recovery, whole-database
GUI export and explicit new-database restore with binary data and routines.
Existing destinations remain protected. OS file pickers use the Host's existing
E2E mock with owned paths; the disposable build denies OS secrets.

Automatically named PK/NOT NULL constraints are preserved without name
collisions, including subsequent automatic constraint creation on restored data.
An existing database with only user triggers is rejected as an occupied target.
The actual full SQL file also restores through Firebird's independent `isql`
parser on Windows Firebird 3/4/5 with `-nodbtriggers`. This check found and fixed
the missing COMMIT between row inserts and dependent DDL; it now runs in every
Windows live fixture. Host transfer timeouts retain explicit user settings
without inheriting the ordinary two-minute default. All five new UI messages
are translated in all thirteen locales.
Candidate CI/release evidence is appended after completion.
Final regressions additionally preserve database/GTT/trigger SQL-security
settings, publication membership/active/automatic flags, and the source page
size. The 16-KiB fixture contains a UTF-8 primary key that exceeds an 8-KiB
database's index limit. This exposed and fixed the wire DPB page-size integer's
byte order; both DPB encodings and actual database page sizes are checked.
An export of 100,001 rows passes the normal buffered-result limit without
truncation. Cancellation while writing preserves the prior dump and removes
the private pending file. Cancellation during a restored SQL statement removes
the newly created database. Dialect-1 sources/targets are rejected before
publishing/modifying data. Lost creation replies report unknown ownership,
without deleting an unconfirmed target or blindly retrying creation.

External UDR declarations retain quoted entrypoints and quoted engine bodies;
trigger and routine edit scripts support those declarations as well. Binding
serialization is unit-tested; execution of application-specific external
modules requires those modules on the target and is outside the fixture.

The [native desktop workflow](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42853003581)
passed all five jobs at plugin source `5cc412e` with Host `d13cc8fa`: Windows
x64, Linux x64/ARM64 and macOS Intel/Apple Silicon. The Intel job retried a
GitHub HTTP-500 download failure before passing. All five downloaded evidence
files contain the expected version, Host revision and six successful native groups.
The [source matrix](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42853006985)
passed all seventeen jobs at that runtime revision.
The [macOS Firebird 3/4 workflow](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42845555002)
passed all four jobs at `45a8f6e`. These use native clients with isolated x64
servers in a disposable emulated Linux VM on CI; no VM is installed locally.

The [two-hour soak](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42837494423)
passed on Firebird 3.0.14, 4.0.7 and 5.0.4. Each ran for more than 7,200 seconds,
including restart/rollback/recovery, with 119,699 successful workload requests
in total. [Raw measurements](beta7-soak.json) retain request counts, latency
percentiles and first/last/peak plugin working sets. This is the core query
workload at `928658e`; it does not measure the newer SQL-transfer operations.

The final Intel compatibility rerun corrected a CI-VM DNS failure and a test
startup deadline that was too short for an emulated server. The export test
still requires observing the private pending file, cancellation, preservation
of the previous dump and removal of the pending file.
The [corrected Intel run](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42898070430)
passed both Firebird 3 and 4 at `d909e40`; the [source matrix](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42898074175)
also passed all seventeen jobs. Changes after runtime `5cc412e` are restricted
to CI networking, that test startup allowance and validation documentation.

SQL transfer limits and required external modules are documented in README.
Arrays, nonstandard BLOB subtypes, external table file mappings and mixed object
owners remain explicit errors. This logical transfer does not replace engine
backup for unsupported databases or server/security-database configuration.
