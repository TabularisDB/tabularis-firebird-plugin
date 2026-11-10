#!/usr/bin/env bash
# Native macOS clients against Firebird 3/4 in an owned, emulated Linux CI guest.
set -euo pipefail
umask 077
[[ $(uname -s) == Darwin && ${GITHUB_ACTIONS:-} == true ]] || { echo 'This VM fixture runs on macOS GitHub Actions only' >&2; exit 2; }
version=${1:?Firebird version required}
[[ $version == 3 || $version == 4 ]] || exit 2
repo_root=$(cd "$(dirname "$0")/.." && pwd)
profile=$(mktemp -d /tmp/tabularis-fb.XXXXXX)
export LIMA_HOME="$profile/lima"
vm=firebird
cleanup() {
  local status=$?
  if [[ $status != 0 ]]; then
    for log in "$LIMA_HOME/$vm/ha.stderr.log" "$LIMA_HOME/$vm/qemu.stderr.log" "$LIMA_HOME/$vm/serial.log"; do
      if [[ -f $log ]]; then tail -n 35 "$log"; fi
    done
  fi
  if [[ -x "$profile/tools/bin/limactl" ]]; then
    "$profile/tools/bin/limactl" stop --force "$vm" >/dev/null 2>&1 || true
    "$profile/tools/bin/limactl" delete --force "$vm" >/dev/null 2>&1 || true
  fi
  [[ $profile == /tmp/tabularis-fb.* ]] || { echo 'Unsafe VM cleanup path' >&2; return 1; }
  rm -rf -- "$profile"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$profile/tools" "$profile/bin"
case $(uname -m) in
  arm64) main_hash=b7af444bd8f91d3c307477e562f2cf3bd7595d62b9429185e4fa841563578cfc
         agent_hash=f43a1523a3a0a45692ca9cb3e5c61ae11b69ef04ca80081e8e05fe4df816a35a ;;
  x86_64) main_hash=d58bc7d6b8158d638720d8c8fdb17eb58527dfead6a29a5b60f48a35ee4a65a6
          agent_hash=cecb56c17ee4aaeac9412b079f175bba52d50b657829099f37b67b424c6364f3 ;;
  *) exit 2 ;;
esac
for kind in main agent; do
  if [[ $kind == main ]]; then package="lima-2.3.0-Darwin-$(uname -m).tar.gz"; digest=$main_hash
  else package="lima-additional-guestagents-2.3.0-Darwin-$(uname -m).tar.gz"; digest=$agent_hash; fi
  curl --fail --location --retry 3 "https://github.com/lima-vm/lima/releases/download/v2.3.0/$package" --output "$profile/$package"
  printf '%s  %s\n' "$digest" "$profile/$package" | shasum -a 256 --check
  tar -xzf "$profile/$package" -C "$profile/tools"
done
export PATH="$profile/tools/bin:$PATH"
# GitHub's macOS runners do not expose nested HVF; use explicit TCG on both hosts.
export FIREBIRD_QEMU_REAL="$(command -v qemu-system-x86_64)"
cat > "$profile/bin/qemu-tcg" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
args=()
while (( $# )); do
  case $1 in
    -machine) args+=("$1" "${2/accel=hvf/accel=tcg}"); shift 2 ;;
    -cpu) args+=("$1" max); shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
exec "$FIREBIRD_QEMU_REAL" "${args[@]}"
SH
chmod 700 "$profile/bin/qemu-tcg"
export QEMU_SYSTEM_X86_64="$profile/bin/qemu-tcg"
cat > "$profile/firebird.yaml" <<YAML
vmType: qemu
arch: x86_64
cpus: 2
memory: 2GiB
disk: 10GiB
images:
- location: https://cloud-images.ubuntu.com/minimal/releases/noble/release-20261105/ubuntu-24.04-minimal-cloudimg-amd64.img
  arch: x86_64
  digest: sha256:406630c08b5e545fcc7209854b129d47e3af822db128c9f19336593b5a2d3fe0
mountType: 9p
mounts:
- location: "$repo_root/tests"
  writable: false
containerd:
  system: false
  user: false
provision:
- mode: system
  script: |
    #!/bin/sh
    set -eu
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y docker.io
    systemctl enable --now docker
probes:
- script: |
    #!/bin/sh
    sudo docker info >/dev/null
YAML
limactl start --name "$vm" --tty=false --timeout=20m "$profile/firebird.yaml"
export FIREBIRD_LIMA_BIN="$profile/tools/bin/limactl" FIREBIRD_LIMA_VM="$vm"
cat > "$profile/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
exec "$FIREBIRD_LIMA_BIN" shell --workdir=/ "$FIREBIRD_LIMA_VM" sudo env \
  "FIREBIRD_ROOT_PASSWORD=${FIREBIRD_ROOT_PASSWORD:-}" "ISC_USER=${ISC_USER:-}" \
  "ISC_PASSWORD=${ISC_PASSWORD:-}" docker "$@"
SH
chmod 700 "$profile/bin/docker"
export PATH="$profile/bin:$PATH"
limactl --version
qemu-system-x86_64 --version | head -1
bash "$repo_root/tests/live-firebird.sh" "$version"
