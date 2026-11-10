# Native macOS Firebird 5 live validation

The [Check workflow](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42805092323) at commit `96b61e23aae8a6575bc33e2d370c12f11147a8f6` passed the new native database jobs on both `macos-15` (Apple Silicon) and `macos-15-intel`. Each ran all 16 live Rust tests and the stdio EOF rollback test against official Firebird 5.0.4. The test-selection output reported zero failures and no ignored live tests on either architecture.

Coverage includes connection metadata, exact numeric/temporal types, roles and character sets, encryption policies, CRUD, DDL, routines/triggers, prepare-only plans, pinned sessions, cancellation/timeouts, bounded results, concurrent writers, attachment termination and lost-COMMIT-reply handling. This uses the actual plugin and native Firebird server, without emulation. It does not run the Tabularis desktop app.

`tests/live-firebird-macos.sh` selects and verifies the architecture-specific package by pinned SHA-256, uses `pkgutil --expand-full` without invoking installer scripts, and supplies its temporary framework, library, configuration and lock paths explicitly. It creates no system user/service or framework installation and requires no sudo. The server binds only `127.0.0.1:13050`; database credentials are randomly generated in a private temporary directory. An EXIT trap stops/waits for the owned server and removes that directory, including package, framework, database, locks and credentials. INT/TERM also exit through the cleanup trap. No Firebird files were downloaded to the Windows development PC for this change.

The shell syntax and the early non-macOS guard were checked locally; the real database behavior was tested on the native CI runners. Reproduce on macOS with `bash tests/live-firebird-macos.sh` from the repository root. The script refuses unsupported operating systems/architectures before creating a fixture.

Still unvalidated by these jobs: Firebird 3/4 on macOS, native desktop interaction/OS integration, full-server restart recovery on macOS, and multi-hour load/soak behavior. Published Beta.5 tags and assets are unchanged; the change adds test coverage.
