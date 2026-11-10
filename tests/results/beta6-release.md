# Beta.6 publication verification, 2026-11-10

[v0.2.0-beta.6](https://github.com/TabularisDB/tabularis-firebird-plugin/releases/tag/v0.2.0-beta.6) is a published prerelease at `f05b5db4d04f88e8811242f6d0d15bd7f54e3892`.

- [Source CI](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42821856578): successful.
- [Release workflow](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42822113815): all 18 jobs successful, including the required native platform/live matrix and publication.
- Downloaded all six actual assets. Each SHA-256 matches GitHub's published digest. All five ZIPs contain the expected executable, tagged manifest, LICENSE and THIRD_PARTY_NOTICES.md; Unix executable permissions are present. The downloaded Windows executable passed initialization, session-state and invalid-connection JSON-RPC checks. Verification downloads were removed.
- The approved official registry entry contains Beta.6. `tests/verify-registry.mjs 0.2.0-beta.6` verified its Ed25519 JWS, key identity, plugin/version/registry identity and all six actual downloads against the signed hashes.
- `tests/host-registry-lifecycle.ps1 -HostPath target/tabularis-host -OldVersion 0.2.0-beta.5` passed using Host patch `3da642463b21d1df5b947ac628fc1b52bd9a7706`: signed Beta.5 install, rejection of bad hashes/versions/cancelled updates while retaining the running old driver, signed update to Beta.6, disable/re-enable and uninstall. This is actual application/installer/RPC code, without native UI buttons. Explicit disposable paths and denied OS-secret operations were used; the profile was removed and Host source/manifest restored.
- Normal Tabularis config, connection and SSH configuration hashes remained unchanged; no local Firebird server was needed for these publication checks.

The companion [Host PR #1469](https://github.com/TabularisDB/tabularis/pull/1469) has all eight checks passed and no blocking automatic review findings. It is still open; upstream write/merge permissions are unavailable in this session. Published Host remains v0.32.0. The deployed registry schema still omits `column_stats`, `psql_blocks` and `query_timeout`; the corresponding plugin flags remain disabled.

The two-hour soak workflow is still running against the earlier Beta.5 baseline `a606c9d`; it is not a completed Beta.6 soak result. Complete database SQL export/import, native Linux/macOS desktop workflows, macOS Firebird 3/4 client coverage and guided Firebird trigger editing remain open. This publication verification does not establish Stable readiness.
