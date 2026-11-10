# Windows host registry lifecycle

Validated using the actual Tabularis application, registry, signature verification, installer, plugin manager and RPC driver implementations at host commit `ec8590e37405bb23bf7efea88ac0a23e25074f1a` (0.32.0 source), without the desktop transport.

The test passed with published, registry-signed Firebird `0.2.0-beta.3` and `0.2.0-beta.4`:

1. Both release signatures resolve to `Verified` through the host's registry client.
2. Application installation downloads Beta.3, verifies its signed hash, discovers the kind-scoped bundle and loads its executable.
3. A real `test_connection` RPC returns the plugin's expected missing-database validation error. No database server is contacted.
4. A wrong SHA-256 is rejected and the running Beta.3 driver remains usable.
5. A mismatched expected version is rejected and Beta.3 remains usable.
6. A cancelled install returns `PLUGIN_INSTALL_CANCELLED` and Beta.3 remains usable.
7. The application updates the installed, running driver to Beta.4, loads the new version and executes the same real RPC.
8. Disable removes the runtime driver but retains the bundle; enable loads the bundle again.
9. Application uninstall stops the process, unregisters the manifest and removes installed and temporary bundle directories.

The harness uses dedicated configuration/data directories, isolated Windows application-data environment variables, no-op events and a secrets provider that refuses credential access. Its finalizer shuts down external test drivers before removing the temporary profile. The PowerShell wrapper restores the host manifest byte-for-byte, removes the temporary example and test root, and restores environment variables. The host working tree was clean after the run.

Reproduce with `./tests/host-registry-lifecycle.ps1 -HostPath target/tabularis-host`. The default plugin CI does not compile this external-host harness. Native desktop interaction, OS integration and non-Windows installation remain unvalidated by this test. The existing Windows OpenSSL linker warning about missing debug PDBs does not prevent execution.
