# Beta.7 publication verification, 2026-11-10

[v0.2.0-beta.7](https://github.com/TabularisDB/tabularis-firebird-plugin/releases/tag/v0.2.0-beta.7)
is a published prerelease at `673c2dcda9dc46b6d2d862a89a0fd223ae158492`.

- [Source CI](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42898074175)
  passed all seventeen jobs at `d909e40`, including the five tested platform packages.
  The tag differs from that revision only in DEVELOPMENT.md and two validation
  reports. Binary sources, lockfiles, manifest and licenses are identical.
- The tag's documentation commit contained a CI-skip marker, so the automatic
  release workflow did not start. Publication used those validated CI packages
  directly after checking that documentation-only difference. No tag was moved
  and no published asset was overwritten.
- Downloaded all six published assets. Each SHA-256 matches GitHub's digest.
  All five ZIPs contain the matching tagged manifest, executable and both license
  documents; Unix executable permissions are present. The downloaded Windows
  executable passed initialization, session-state and invalid-connection RPCs.
  Verification downloads were removed.
- The approved official registry entry includes Beta.7.
  `tests/verify-registry.mjs 0.2.0-beta.7` verified the Ed25519 JWS, key and release
  identities, and all six downloaded assets against their signed hashes.
- Native five-platform tests, the corrected macOS Firebird 3/4 compatibility
  coverage and the completed two-hour core soak are recorded in
  [the validation report](beta7.md).
- `tests/host-registry-lifecycle.ps1 -OldVersion 0.2.0-beta.6` passed with Host
  `d13cc8fa`: signed Beta.6 install, rejected hash/version/cancelled updates
  retaining the running driver, signed Beta.7 update, disable/re-enable and
  uninstall. The harness uses actual application/installer/RPC code with
  disposable paths and denied OS secrets. It restores Host source and removes
  the profile; this does not claim native installer-button coverage.
- Final cleanup found no owned Firebird/plugin test processes, server folders,
  native/registry profiles or import snapshots. Publication downloads and the
  disposable native Host binary were removed. The Host worktree is clean.

The companion Host PR #1470 remains open, with passing checks and no remaining
automatic review findings. This session has no upstream merge permission.
Published Host is still v0.32.0. The deployed registry schema still omits
`column_stats`, `query_timeout` and `psql_blocks`; those flags remain gated.
The prepared operator patch is `registry-driver-capabilities.patch.json`.
This prerelease does not claim deployment of those upstream changes or support
for the logical SQL representations explicitly excluded in README.
