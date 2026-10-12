#!/usr/bin/env bash
# Remote UI/e2e test runner.
#
# Every suite that drives a real renderer — egui_kittest (software wgpu) and the
# legacy Playwright/Chromium suite — runs on the MARS Linux runner inside a
# disposable Docker container, never on a developer workstation. The container
# is capped (--cpus/--memory), always removed (--rm plus an explicit `docker rm
# -f`), and the remote working copy is deleted by an EXIT trap.
#
# Usage:
#   scripts/e2e-remote.sh cargo test -p plaudon-app --test shell_kittest
#   scripts/e2e-remote.sh playwright [--reporter=line ...]
#   scripts/e2e-remote.sh shell 'cargo xtask something && ls target'
#
#   cargo      Rust image + mesa/lavapipe software rendering. Snapshot artifacts
#              (*.diff.png, *.new.png, any */snapshots/* file) are copied back.
#   playwright Node image matching the pinned @playwright/test version; runs
#              `npm ci` then `playwright test`. playwright-report/ and
#              test-results/ are copied back.
#   shell      Arbitrary command in the cargo image (same rendering setup).
#
# Results land in $PLAUDON_E2E_RESULTS (default: target/e2e-results).
#
# Environment overrides:
#   PLAUDON_E2E_HOST         ssh destination           (devshell@192.168.0.100)
#   PLAUDON_E2E_REMOTE_ROOT  remote scratch parent     (/tmp)
#   PLAUDON_E2E_IMAGE        docker image for the mode (mode-specific default)
#   PLAUDON_E2E_CPUS         docker --cpus             (8)
#   PLAUDON_E2E_MEMORY       docker --memory           (12g)
#   PLAUDON_E2E_RESULTS      local artifact directory
#   PLAUDON_E2E_APT_PACKAGES extra apt packages for the cargo/shell image
#   PLAUDON_E2E_NO_CACHE=1   do not reuse the persistent remote cargo cache
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST="${PLAUDON_E2E_HOST:-devshell@192.168.0.100}"
REMOTE_ROOT="${PLAUDON_E2E_REMOTE_ROOT:-/tmp}"
CPUS="${PLAUDON_E2E_CPUS:-8}"
MEMORY="${PLAUDON_E2E_MEMORY:-12g}"
RESULTS_DIR="${PLAUDON_E2E_RESULTS:-$REPO_ROOT/target/e2e-results}"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=20)

usage() {
  sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

MODE="${1:-}"
case "$MODE" in
  cargo | playwright | shell) shift ;;
  -h | --help | help)
    usage
    exit 0
    ;;
  "")
    echo "error: missing mode (cargo|playwright|shell)" >&2
    usage >&2
    exit 2
    ;;
  *)
    echo "error: unknown mode '$MODE' (expected cargo|playwright|shell)" >&2
    exit 2
    ;;
esac

if [[ $# -eq 0 && "$MODE" == "shell" ]]; then
  echo "error: 'shell' mode needs a command" >&2
  exit 2
fi

# Quote the caller's arguments once so they survive ssh -> bash -> docker.
quoted_args=""
for arg in "$@"; do
  quoted_args+=" $(printf '%q' "$arg")"
done

run_id="$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
REMOTE_DIR="$REMOTE_ROOT/plaudon-e2e-$run_id"
CONTAINER="plaudon-e2e-$run_id"
REMOTE_TARBALL="$REMOTE_ROOT/$CONTAINER-artifacts.tar.gz"
CACHE_DIR="$REMOTE_ROOT/plaudon-e2e-cache"

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  # Container first (it owns the mount), then the working copy. Files written by
  # the container belong to root, so fall back to a root container for removal.
  ssh "${SSH_OPTS[@]}" "$HOST" "
    docker rm -f $(printf '%q' "$CONTAINER") >/dev/null 2>&1 || true
    rm -f $(printf '%q' "$REMOTE_TARBALL") $(printf '%q' "$REMOTE_TARBALL.list") 2>/dev/null || true
    if ! rm -rf $(printf '%q' "$REMOTE_DIR") 2>/dev/null; then
      docker run --rm -v $(printf '%q' "$REMOTE_DIR"):/victim alpine:3 \\
        sh -c 'rm -rf /victim/* /victim/.[!.]* /victim/..?* 2>/dev/null || true' >/dev/null 2>&1 || true
      rmdir $(printf '%q' "$REMOTE_DIR") 2>/dev/null || true
    fi
  " >/dev/null 2>&1 ||
    echo "warning: remote cleanup failed; check $HOST:$REMOTE_DIR and container $CONTAINER" >&2
  exit "$status"
}
trap cleanup EXIT INT TERM

if [[ "$MODE" == "playwright" ]]; then
  # The browser build is baked into the image, so it must match the pinned
  # @playwright/test version exactly or Playwright refuses to launch.
  pw_version="$(node -p "require('$REPO_ROOT/apps/desktop/package-lock.json').packages['node_modules/@playwright/test'].version")"
  IMAGE="${PLAUDON_E2E_IMAGE:-mcr.microsoft.com/playwright:v$pw_version-noble}"
else
  IMAGE="${PLAUDON_E2E_IMAGE:-rust:1-bookworm}"
fi

echo "==> host=$HOST image=$IMAGE cpus=$CPUS memory=$MEMORY"
echo "==> remote dir $REMOTE_DIR (removed on exit)"

rsync -a --delete \
  --exclude '.git/' \
  --exclude 'target/' \
  --exclude 'node_modules/' \
  --exclude 'dist/' \
  --exclude 'e2e-results/' \
  --exclude 'playwright-report/' \
  --exclude 'test-results/' \
  -e "ssh ${SSH_OPTS[*]}" \
  "$REPO_ROOT/" "$HOST:$REMOTE_DIR/"

# MARS runs rootless Docker: the container's root maps to an unprivileged host
# subuid but its gid maps to the ssh user's group. Group-writable trees plus a
# 002 umask inside the container are therefore what lets both sides read and
# delete each other's files.
ssh "${SSH_OPTS[@]}" "$HOST" "chmod -R g+rwX $(printf '%q' "$REMOTE_DIR")"

# Mesa/lavapipe gives wgpu a software Vulkan adapter; the rest are build deps
# for the native crates (audio, TLS, bindgen).
apt_packages="mesa-vulkan-drivers libvulkan1 libgl1 libegl1 libxkbcommon0 \
libx11-6 libxcursor1 libxi6 libxrandr2 fontconfig pkg-config cmake clang \
libasound2-dev libssl-dev ${PLAUDON_E2E_APT_PACKAGES:-}"

case "$MODE" in
  playwright)
    docker_mounts=()
    inner="umask 0002
set -euo pipefail
mkdir -p /tmp/home
cd /work/apps/desktop
npm ci --no-audit --no-fund
npx playwright test$quoted_args"
    docker_env=(-e CI=1 -e PLAUDON_E2E_ALLOW_LOCAL=1 -e HOME=/tmp/home -e npm_config_cache=/tmp/home/.npm)
    ;;
  cargo | shell)
    if [[ "$MODE" == "cargo" ]]; then
      command_line="cargo$quoted_args"
    else
      command_line="$*"
    fi
    inner="umask 0002
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends $apt_packages >/dev/null
cd /work
$command_line"
    docker_env=(
      -e CARGO_TERM_COLOR=always
      -e CARGO_HOME=/cache/cargo
      -e CARGO_TARGET_DIR=/cache/target
      -e CARGO_BUILD_JOBS="$CPUS"
      -e LIBGL_ALWAYS_SOFTWARE=1
      -e GALLIUM_DRIVER=llvmpipe
      -e WGPU_BACKEND=vulkan
      -e XDG_RUNTIME_DIR=/tmp
      -e RUST_BACKTRACE=1
    )
    if [[ "${PLAUDON_E2E_NO_CACHE:-0}" == "1" ]]; then
      docker_mounts=()
      docker_env+=(-e CARGO_HOME=/work/.cargo-home -e CARGO_TARGET_DIR=/work/target)
    else
      docker_mounts=(-v "$CACHE_DIR:/cache")
    fi
    ;;
esac

mount_args=""
for m in ${docker_mounts[@]+"${docker_mounts[@]}"}; do
  mount_args+=" $(printf '%q' "$m")"
done
env_args=""
for e in "${docker_env[@]}"; do
  env_args+=" $(printf '%q' "$e")"
done

status=0
ssh "${SSH_OPTS[@]}" "$HOST" "bash -s" <<EOF || status=$?
set -euo pipefail
mkdir -p $(printf '%q' "$CACHE_DIR")
chmod g+rwX $(printf '%q' "$CACHE_DIR") 2>/dev/null || true
docker run --rm --name $(printf '%q' "$CONTAINER") \
  --cpus $(printf '%q' "$CPUS") --memory $(printf '%q' "$MEMORY") --shm-size=2g \
 $env_args \
  -v $(printf '%q' "$REMOTE_DIR"):/work$mount_args \
  -w /work \
  $(printf '%q' "$IMAGE") bash -c $(printf '%q' "$inner")
EOF

# Artifacts are most valuable exactly when the run failed, so collect them
# before the trap wipes the remote directory.
mkdir -p "$RESULTS_DIR"
if [[ "$MODE" == "playwright" ]]; then
  find_expr="-path './apps/desktop/playwright-report/*' -o -path './apps/desktop/test-results/*'"
else
  # `*/visual/*` is Plaudon's own baseline directory (docs/EGUI_TESTING.md §4);
  # `*/snapshots/*` is egui_kittest's default, kept for any crate that uses it.
  find_expr="-name '*.diff.png' -o -name '*.new.png' -o -path '*/snapshots/*' -o -path '*/visual/*'"
fi
if ssh "${SSH_OPTS[@]}" "$HOST" "
    cd $(printf '%q' "$REMOTE_DIR") &&
    find . -path ./target -prune -o -type f \\( $find_expr \\) -print0 \
      > $(printf '%q' "$REMOTE_TARBALL.list") &&
    test -s $(printf '%q' "$REMOTE_TARBALL.list") &&
    tar --null --files-from=$(printf '%q' "$REMOTE_TARBALL.list") \
      -czf $(printf '%q' "$REMOTE_TARBALL")
  "; then
  scp "${SSH_OPTS[@]}" -q "$HOST:$REMOTE_TARBALL" "$RESULTS_DIR/artifacts.tar.gz"
  tar -xzf "$RESULTS_DIR/artifacts.tar.gz" -C "$RESULTS_DIR"
  rm -f "$RESULTS_DIR/artifacts.tar.gz"
  echo "==> artifacts copied to $RESULTS_DIR"
else
  echo "==> no artifacts produced"
fi

if [[ $status -eq 0 ]]; then
  echo "==> remote run succeeded"
else
  echo "==> remote run failed with status $status" >&2
fi
exit "$status"
