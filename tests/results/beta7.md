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
Candidate CI/release evidence is appended after completion.
The successful two-hour baseline soak run was 42813658201 at a606c9d; it is
evidence for the prior runtime, not a Beta.7 measurement.

SQL transfer limits and required external modules are documented in README.
Arrays, nonstandard BLOB subtypes, external table file mappings and mixed object
owners remain explicit errors. This logical transfer does not replace engine
backup for unsupported databases or server/security-database configuration.
