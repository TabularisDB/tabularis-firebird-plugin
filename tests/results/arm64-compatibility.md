# Native ARM64 client compatibility with Firebird 3/4

The [Check run](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42812560129) at source commit `c4c931f73815f7c1f09f0d3a6a0f9019f823d283` passed both new jobs on `ubuntu-24.04-arm`:

- Firebird 3.0.14: 15 live Rust tests, the stdio EOF rollback test, and complete-server crash/restart recovery.
- Firebird 4.0.7: 16 live Rust tests, the stdio EOF rollback test, and complete-server crash/restart recovery.

Firebird 3 omits the existing Firebird-4+-specific type test. Both logs explicitly report `Native plugin host: aarch64`, a `linux/amd64` server platform and an `ELF ... ARM aarch64` plugin executable. Both contain the restart-success marker. The native Rust client is tested against real Firebird server code; only the x64 server containers are emulated. This is not evidence of running native ARM64 Firebird 3/4 servers. The existing Firebird 5 ARM64 job uses a native ARM64 server too.

The official Firebird 3/4 bookworm container manifests used here contain x64 server builds. CI registers only the required x64 emulator through the official Docker QEMU setup action, pinned by commit, with its helper image pinned by digest and image caching disabled. Server images retain their existing pinned digests. Local scripts do not install QEMU. The fixtures retain loopback-only ports, random credentials, read-only readiness probes and cleanup of their containers, anonymous volumes, newly pulled Firebird images and temporary files.

The attempted macOS extension found no macOS packages among the current official GitHub release assets for [3.0.14](https://github.com/FirebirdSQL/firebird/releases/tag/v3.0.14) or [4.0.7](https://github.com/FirebirdSQL/firebird/releases/tag/v4.0.7). No older macOS package was substituted, and no macOS 3/4 coverage is claimed. Those native-client tests remain open, as do native desktop interaction and multi-hour soak validation. Nothing was installed on the Windows development PC.
