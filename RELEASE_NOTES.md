# Unreleased

Add the Firebird logo to the Tabularium listing. The next normal release's
standalone `default.tabularium` includes the icon URL from the root manifest.

# Firebird driver 1.0.0

First stable write-capable release for Firebird 3, 4 and 5. Includes interactive
transactions, cancellation/timeouts, atomic batches, parameterized grid writes,
schema builders, routines/triggers, query plans, encrypted connections and
logical whole-database SQL export/import developed throughout the 0.2 betas.

Complete native `.fbk` backup and create-only restore now stream through
Firebird's authenticated, encrypted Services API. No local Firebird installation,
`gbak` executable or native client library is required. This covers arrays,
mixed object owners, compiled metadata and large BLOBs beyond the logical SQL
representation. User-defined BLOB subtypes also work in ordinary query results
and SQL exports as raw binary data.

Native backups publish an atomic private file only after successful completion.
Restore snapshots the input and never enables database replacement. A failed or
cancelled native restore may leave a partial new database; the error explicitly
requires checking it before retrying. Unknown targets are never automatically
deleted. External-table data becomes ordinary tables, and source shadow files
are not recreated. Server configuration, accounts, encryption keys and external
modules remain separate dependencies.

The companion [Host PR #1470](https://github.com/TabularisDB/tabularis/pull/1470)
adds `.sql`/`.fbk` selection and native restore destination handling alongside
whole-file SQL transfers and trigger editing. Older Hosts retain their previous
workflows. Native backup/restore RPCs are available directly. Routine menus,
column-statistics and the dedicated timeout field remain subject to compatible
Host and registry capability rollout; their gated manifest flags remain absent.
Registry schema changes were explicitly deferred.

See README for logical SQL limits and the distinction between native database
backup and server configuration backup. Detailed validation is recorded in
`tests/results/1.0.0.md`; earlier platform/editor and two-hour core-soak evidence
is retained in the Beta.7 reports.
