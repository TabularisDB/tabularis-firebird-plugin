# Firebird driver 0.2.0-beta.7

This prerelease adds complete logical database SQL export/restore through optional
whole-file RPCs. It preserves domains, routines/packages, ordinary/GTT tables,
views in dependency order, computed and identity columns, sequence definitions
and counters, indexes/constraints, triggers, collations, comments, grants,
role system privileges/default roles and local authentication mappings. Legacy
UDF and BLOB-filter declarations retain their external bindings; the target
server must provide the corresponding modules.

Export streams one read-only snapshot into an atomic private file. Restore
validates the complete input before creating a new database or requiring an
empty existing target. New database creation never overwrites an existing
database, and a failed newly-created restore attempts bounded cleanup. Existing
empty targets can retain partial work on SQL errors. Binary OCTETS columns now
retain their raw bytes through both wire decoders and SQL rendering.
Database/table/trigger SQL-security settings, original page sizes and replication
publication settings are preserved. The restored wide-index fixture verifies
the corrected little-endian creation DPB. Transfers reject SQL Dialect 1.
External UDR trigger/routine declarations and edit scripts preserve quoted
bindings and bodies; their modules must be installed on the server.

Live regressions verify export past 100,000 rows, cancellation without replacing
a prior dump, cleanup of cancelled new-database restores, and independent
restoration through Firebird's own isql parser. Lost creation replies report an
unknown outcome without retrying or deleting an unconfirmed target.

[Host PR #1470](https://github.com/TabularisDB/tabularis/pull/1470) adds the shared
desktop/web integration, explicit restore destination UI and a generic-dialect
trigger SQL editor that sends complete PSQL without dropping the old trigger.
These require that Host change to be merged and released. PSQL, routine-menu,
statistics and timeout flags remain gated on the matching Host/registry rollout.

The live roundtrip checks data, metadata, external declarations and failure
cleanup on Firebird 3/4/5. Native platform CI reruns before publication. Release
verification and remaining deployment requirements are recorded in
[the validation report](https://github.com/TabularisDB/tabularis-firebird-plugin/blob/main/tests/results/beta7.md).

Arrays, nonstandard BLOB subtypes, external-table file mappings, mixed owners and
missing/unknown catalog representations fail explicitly. SQL records are limited
to 8 MiB. Use an engine backup outside those limits. This is a logical database
transfer; server configuration, security accounts and encryption keys are separate.
Embedded access and TLS configuration remain unsupported. Wire encryption is
available. Stable v0.1.0 remains the read-only release.
