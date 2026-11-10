#!/usr/bin/env bash
# Linux live tests with disposable official Firebird containers. No system installation.
set -euo pipefail
umask 077

version=${1:-5}
# Official firebirdsql/firebird bookworm manifests. ARM64 Firebird 5 pinned 2026-11-10.
# https://github.com/FirebirdSQL/firebird-docker
architecture=$(uname -m)
case "$version:$architecture" in
  3:x86_64) digest=1592da0e07e8303646de13bdcd7a80272c08a0c3dcf55bedeef632168508b5ac ;;
  4:x86_64) digest=030e734f34a03f0a445764b9989c33885f2810a8ad63d2e420190c51dfd75dc2 ;;
  5:x86_64) digest=411db5ff019f8dd4308c8bbb2c85b7a49fc76290cdef81f848b6655bd72f599d ;;
  5:aarch64|5:arm64) digest=14669e1a8b9c33e36d463bf52773e3ce7a7b5f7350bcb878a3db44f122f93d3b ;;
  *) echo "Unsupported Firebird version/architecture: $version/$architecture (x64: 3/4/5; ARM64: 5)" >&2; exit 2 ;;
esac
image="firebirdsql/firebird@sha256:$digest"
repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
mkdir -p target
test_dir=$(mktemp -d "$repo_root/target/firebird-linux.XXXXXX")
container=''
owned_image=false
cleanup() {
  if [[ -n "$container" ]]; then docker rm --force --volumes "$container" >/dev/null || true; fi
  if [[ "$owned_image" == true ]]; then docker image rm "$image" >/dev/null || true; fi
  if [[ "$test_dir" != "$repo_root/target/firebird-linux."* ]]; then
    echo 'Unsafe Firebird cleanup path' >&2
    return 1
  fi
  rm -rf -- "$test_dir"
}
trap cleanup EXIT
if ! docker image inspect "$image" >/dev/null 2>&1; then owned_image=true; fi
export FIREBIRD_ROOT_PASSWORD
FIREBIRD_ROOT_PASSWORD=$(openssl rand -hex 16)
export ISC_USER=SYSDBA ISC_PASSWORD="$FIREBIRD_ROOT_PASSWORD"
# Keep the endpoint stable across a hard container stop/start.
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
container=$(docker run --detach --publish "127.0.0.1:$port:3050" \
  --env FIREBIRD_ROOT_PASSWORD \
  --env FIREBIRD_DATABASE=plugin-test.fdb \
  --env FIREBIRD_DATABASE_DEFAULT_CHARSET=UTF8 \
  --env FIREBIRD_CONF_WireCrypt=Required \
  --mount "type=bind,src=$repo_root/tests/fixture.sql,dst=/docker-entrypoint-initdb.d/01-fixture.sql,readonly" \
  "$image")

ready=false
for ((attempt=0; attempt<60; attempt++)); do
  if docker exec --interactive --env ISC_USER --env ISC_PASSWORD "$container" \
      /opt/firebird/bin/isql -bail -quiet localhost:/var/lib/firebird/data/plugin-test.fdb \
      <<< 'SELECT COUNT(*) FROM FB_CHILD;' >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
if [[ "$ready" != true ]]; then echo 'Firebird fixture did not become ready' >&2; exit 1; fi
binding=$(docker port "$container" 3050/tcp)
export FIREBIRD_TEST_PORT=${binding##*:}
export FIREBIRD_TEST_CONFIG="$test_dir/connection.json"
unset FIREBIRD_TEST_CIPHER
python3 - <<'PY'
import json
import os
from pathlib import Path

Path(os.environ["FIREBIRD_TEST_CONFIG"]).write_text(json.dumps({
    "host": "127.0.0.1",
    "port": int(os.environ["FIREBIRD_TEST_PORT"]),
    "database": "/var/lib/firebird/data/plugin-test.fdb",
    "username": "SYSDBA",
    "password": os.environ["FIREBIRD_ROOT_PASSWORD"],
}), encoding="utf-8")
PY
args=(--ignored --test-threads=1)
if [[ "$version" == 3 ]]; then args+=(--skip _firebird4); fi
cargo test --locked live_ -- "${args[@]}"
cargo build --locked --bin firebird-plugin
python3 tests/restart-firebird.py --container "$container"
