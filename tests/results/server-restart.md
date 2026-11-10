# Complete-server crash and recovery

The [native Check workflow](https://github.com/TabularisDB/tabularis-firebird-plugin/actions/runs/42810326242) at source commit `416e86ee4720c0a341ee65b519be245b5666e41d` passed the shared `tests/restart-firebird.py` test in all six live configurations:

- Linux x64: Firebird 3.0.14, 4.0.7 and 5.0.4.
- Linux ARM64: Firebird 5.0.4.
- macOS Intel and Apple Silicon: Firebird 5.0.4 on each.

Each completed job's log contains the explicit restart-success marker, checked after the jobs completed. The helper launches one actual stdio plugin process, creates its fixture table, opens a pinned transaction and inserts an uncommitted row. It hard-kills the complete disposable server (Docker SIGKILL or the owned macOS server PID). The old plugin session must report an error and `in_transaction: false`. After restart on the original endpoint, the uncommitted row must be absent. A fresh transaction then inserts and commits a row; a separate attachment must read that committed row. The table is dropped after verification. The plugin process is never restarted during the scenario.

The first Linux attempt exposed a fixture readiness race: Docker's TCP proxy accepted connections before Firebird could execute queries, causing the first recovery SELECT to fail. The corrected helper waits for a native read-only isql probe inside the restarted container. No write, transaction control or unknown commit is retried. The Docker host port is explicitly allocated once and retained across restart; the existing container and database volume are reused.

The helper refuses unsupported operating systems, non-full Docker IDs and unsafe macOS PIDs; the macOS executable must resolve within the temporary fixture. Its finalizer closes/stops the plugin and stops its owned replacement macOS server. The surrounding launchers remove the temporary database, credentials, package/framework or container/volume and newly pulled image. Tests run on native CI runners; no Firebird/Docker installation was added to the Windows development PC.

This adds to the existing Windows Firebird 3/4/5 restart checks. It is a single hard server-process crash per fixture, not host power-loss testing, repeated restart stress, a native desktop test or multi-hour soak validation. Linux ARM64 and macOS Firebird 3/4 remain separate gaps.
