#!/usr/bin/env bash
# Extract the official package; never execute its installer or service scripts.
set -euo pipefail
umask 077
[[ $(uname -s) == Darwin ]] || { echo 'This fixture requires macOS' >&2; exit 2; }
case $(uname -m) in
  arm64) platform=arm64; digest=0c3495ec457720f3b3b51e1be0f36485f589732d6af38c9b04f1eb171d8d5509 ;;
  x86_64) platform=x64; digest=f6b587d8d16f528b9ce94bf006e204cc171620fbb64b90730790e7af612c2325 ;;
  *) echo 'Unsupported macOS architecture' >&2; exit 2 ;;
esac
repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
mkdir -p target
test_dir=$(mktemp -d "$repo_root/target/firebird-macos.XXXXXX")
server_pid=''
cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    for ((attempt=0; attempt<50; attempt++)); do
      kill -0 "$server_pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -KILL "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  [[ "$test_dir" == "$repo_root/target/firebird-macos."* ]] || { echo 'Unsafe cleanup path' >&2; return 1; }
  rm -rf -- "$test_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
package="Firebird-5.0.4.1812-0-macos-$platform.pkg"
curl --fail --location --retry 3 "https://github.com/FirebirdSQL/firebird/releases/download/v5.0.4/$package" --output "$test_dir/firebird.pkg"
printf '%s  %s\n' "$digest" "$test_dir/firebird.pkg" | shasum -a 256 --check
pkgutil --expand-full "$test_dir/firebird.pkg" "$test_dir/extracted"
config=$(find "$test_dir/extracted" -type f -name firebird.conf -print)
[[ -n "$config" && "$config" != *$'\n'* ]] || { echo 'Expected one Firebird configuration' >&2; exit 1; }
export FIREBIRD
FIREBIRD=$(dirname "$config")
# The component payload is the framework root (Versions/A/Resources below it).
framework=$(dirname "$(dirname "$(dirname "$FIREBIRD")")")
mkdir "$test_dir/frameworks" "$test_dir/lock"
ln -s "$framework" "$test_dir/frameworks/Firebird.framework"
export DYLD_FRAMEWORK_PATH="$test_dir/frameworks"
export DYLD_LIBRARY_PATH="$FIREBIRD/lib:$FIREBIRD/../Libraries:$FIREBIRD/.."
export FIREBIRD_LOCK="$test_dir/lock"
unset ISC_USER ISC_PASSWORD FIREBIRD_TEST_CIPHER
cat >> "$config" <<EOF

RemoteBindAddress = 127.0.0.1
RemoteServicePort = 13050
IpcName = tabularis_macos_test
WireCrypt = Required
SecurityDatabase = $FIREBIRD/security5.fdb
EOF
export FIREBIRD_TEST_CONFIG="$test_dir/connection.json"
export FIREBIRD_MACOS_TEST_ROOT="$test_dir"
python3 - <<'PY'
import json, os, secrets, socket
from pathlib import Path
with socket.socket() as probe:
    probe.bind(('127.0.0.1', 13050))
root = Path(os.environ['FIREBIRD_MACOS_TEST_ROOT'])
database = str(root / 'plugin-test.fdb')
password = secrets.token_hex(16)
Path(os.environ['FIREBIRD_TEST_CONFIG']).write_text(json.dumps(dict(
    host='127.0.0.1', port=13050, database=database, username='SYSDBA', password=password)))
escaped = database.replace("'", "''")
(root / 'bootstrap.sql').write_text(
    f"CREATE DATABASE '{escaped}' USER 'SYSDBA' DEFAULT CHARACTER SET UTF8;\n"
    f"CREATE OR ALTER USER SYSDBA PASSWORD '{password}';\nCOMMIT;\nQUIT;\n")
PY
"$FIREBIRD/bin/isql" -bail -quiet -input "$test_dir/bootstrap.sql"
"$FIREBIRD/bin/firebird" > "$test_dir/server.log" 2>&1 &
server_pid=$!
ready=false
for ((attempt=0; attempt<100; attempt++)); do
  if ! kill -0 "$server_pid" 2>/dev/null; then cat "$test_dir/server.log"; exit 1; fi
  if nc -z 127.0.0.1 13050; then ready=true; break; fi
  sleep 0.1
done
[[ "$ready" == true ]] || { cat "$test_dir/server.log"; echo 'Server did not start' >&2; exit 1; }
export ISC_USER=SYSDBA
ISC_PASSWORD=$(python3 -c 'import json,os; print(json.load(open(os.environ["FIREBIRD_TEST_CONFIG"]))["password"])')
export ISC_PASSWORD
"$FIREBIRD/bin/isql" -bail -quiet -input tests/fixture.sql "127.0.0.1/13050:$test_dir/plugin-test.fdb"
cargo test --locked live_ -- --ignored --test-threads=1
