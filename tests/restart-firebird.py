"""Crash/restart only a disposable server owned by a live-test launcher (POSIX)."""
import argparse
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import time


def main():
    if sys.platform not in ("linux", "darwin"):
        raise SystemExit("This fixture requires Linux or macOS")
    parser = argparse.ArgumentParser(description=__doc__)
    backend = parser.add_mutually_exclusive_group(required=True)
    backend.add_argument("--container")
    backend.add_argument("--server-pid", type=int)
    parser.add_argument("--server-executable", type=Path)
    args = parser.parse_args()
    config_path = Path(os.environ["FIREBIRD_TEST_CONFIG"]).resolve()
    config = json.loads(config_path.read_text())
    if args.container is not None and (
        len(args.container) != 64 or any(c not in "0123456789abcdef" for c in args.container)
    ):
        parser.error("A full disposable Docker container ID is required")
    if args.server_pid is not None:
        if args.server_pid <= 1 or not args.server_executable:
            parser.error("A disposable server PID and executable are required")
        executable = args.server_executable.resolve()
        if not executable.is_relative_to(config_path.parent):
            parser.error("The server executable must belong to this temporary fixture")
    plugin = subprocess.Popen(
        ["target/debug/firebird-plugin"], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, text=True, encoding="utf-8",
    )
    restarted = None
    server_log = None
    request_id = 0

    def query(sql, session=""):
        nonlocal request_id
        request_id += 1
        params = dict(params=config, query=sql)
        if session:
            params["session_id"] = session
        request = dict(jsonrpc="2.0", id=request_id, method="execute_query", params=params)
        plugin.stdin.write(json.dumps(request) + "\n")
        plugin.stdin.flush()
        if not select.select([plugin.stdout], [], [], 15)[0]:
            raise RuntimeError("Restart test RPC exceeded 15 seconds")
        response = json.loads(plugin.stdout.readline())
        if response.get("id") != request_id or "error" in response:
            raise RuntimeError("Unexpected restart-test RPC response")
        return response["result"]

    def success(sql, session=""):
        result = query(sql, session)
        if "error" in result:
            raise RuntimeError(f"Restart-test SQL failed: {result['error']}")
        return result.get("result", result)

    def docker(*command):
        subprocess.run(["docker", *command, args.container], check=True, timeout=30,
                       stdout=subprocess.DEVNULL)

    def stop(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    try:
        success("CREATE TABLE FB_RESTART_TEST (ID INTEGER PRIMARY KEY)")
        success("BEGIN", "restart")
        success("INSERT INTO FB_RESTART_TEST VALUES (1)", "restart")
        if args.container:
            docker("kill", "--signal", "KILL")
        else:
            os.kill(args.server_pid, signal.SIGKILL)
        offline = query("SELECT COUNT(*) FROM FB_RESTART_TEST", "restart")
        if not offline.get("error") or offline.get("in_transaction") is not False:
            raise RuntimeError("Server crash must fail the old session and clear its transaction")
        if args.container:
            docker("start")
        else:
            server_log = (config_path.parent / "restarted-server.log").open("w")
            restarted = subprocess.Popen([str(executable)], stdout=server_log,
                                         stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 30
        while True:
            if restarted and restarted.poll() is not None:
                raise RuntimeError("Restarted server exited")
            try:
                if args.container:
                    # Docker's port proxy can accept before Firebird is ready.
                    probe = subprocess.run(
                        ["docker", "exec", "--interactive", "--env", "ISC_USER",
                         "--env", "ISC_PASSWORD", args.container, "/opt/firebird/bin/isql",
                         "-bail", "-quiet", "localhost:" + config["database"]],
                        input="SELECT 1 FROM RDB$DATABASE;\n", text=True, timeout=3,
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    )
                    if probe.returncode:
                        raise OSError("Firebird is not ready")
                else:
                    with socket.create_connection((config["host"], config["port"]), timeout=0.2):
                        pass
                break
            except (OSError, subprocess.TimeoutExpired):
                if time.monotonic() >= deadline:
                    raise RuntimeError("Server did not restart on the original endpoint")
                time.sleep(0.1)
        if success("SELECT COUNT(*) FROM FB_RESTART_TEST", "restart")["rows"] != [[0]]:
            raise RuntimeError("Uncommitted insert survived the server crash")
        success("BEGIN", "restart")
        success("INSERT INTO FB_RESTART_TEST VALUES (2)", "restart")
        success("COMMIT", "restart")
        if success("SELECT COUNT(*) FROM FB_RESTART_TEST")["rows"] != [[1]]:
            raise RuntimeError("Fresh transaction after restart failed")
        success("DROP TABLE FB_RESTART_TEST")
        print("Verified complete server crash, rollback and fresh committed work in the same plugin process.")
    finally:
        try:
            plugin.stdin.close()
            try:
                plugin.wait(timeout=5)
            except subprocess.TimeoutExpired:
                stop(plugin)
        finally:
            if restarted:
                stop(restarted)
            if server_log:
                server_log.close()


if __name__ == "__main__":
    main()
