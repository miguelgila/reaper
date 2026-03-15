#!/usr/bin/env bash
# build-node-image.sh — Build reaper-node installer image and load into Kind
#
# This image contains containerd-shim-reaper-v2, reaper-runtime, and an
# install script. It's used as an init container in the Helm chart's
# DaemonSet to install Reaper binaries onto cluster nodes.
#
# Usage:
#   ./scripts/build-node-image.sh --cluster-name <name>
#   ./scripts/build-node-image.sh --cluster-name <name> --quiet
#   ./scripts/build-node-image.sh --cluster-name <name> --skip-build

set -euo pipefail

CLUSTER_NAME=""
SKIP_BUILD=false
QUIET=false
IMAGE_NAME="ghcr.io/miguelgila/reaper-node:latest"
LOG_FILE="/tmp/reaper-node-build.log"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors
if [[ -n "${NO_COLOR:-}" ]]; then
  B="" G="" Y="" C="" R=""
elif [[ -t 1 ]] || [[ -n "${CI:-}" ]]; then
  B=$'\033[1m' G=$'\033[1;32m' Y=$'\033[1;33m' C=$'\033[1;36m' R=$'\033[0m'
else
  B="" G="" Y="" C="" R=""
fi

info()  { echo "${C}==> ${R}${B}$*${R}"; }
ok()    { echo " ${G}OK${R}  $*"; }
fail()  { echo " ${Y}ERR${R} $*" >&2; exit 1; }

if_log() {
  if $QUIET; then cat >> "$LOG_FILE"; else cat; fi
}

while [[ $# -gt 0 ]]; do
  case $1 in
    --cluster-name)
      CLUSTER_NAME="${2:-}"
      [[ -z "$CLUSTER_NAME" ]] && fail "--cluster-name requires a value"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=true
      shift
      ;;
    --quiet)
      QUIET=true
      shift
      ;;
    --image)
      IMAGE_NAME="${2:-}"
      [[ -z "$IMAGE_NAME" ]] && fail "--image requires a value"
      shift 2
      ;;
    -h|--help)
      echo "Usage: $0 --cluster-name <name> [OPTIONS]"
      echo ""
      echo "Build reaper-node installer image and load into Kind cluster."
      echo ""
      echo "Options:"
      echo "  --cluster-name <name>   Kind cluster name (required)"
      echo "  --skip-build            Skip binary compilation (use existing)"
      echo "  --image <name>          Image name (default: $IMAGE_NAME)"
      echo "  --quiet                 Suppress output"
      exit 0
      ;;
    *)
      fail "Unknown option: $1"
      ;;
  esac
done

[[ -z "$CLUSTER_NAME" ]] && fail "--cluster-name is required"

# Detect architecture
NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
[[ -z "$NODE_ID" ]] && fail "Cannot find control-plane container for cluster '$CLUSTER_NAME'"

NODE_ARCH=$(docker exec "$NODE_ID" uname -m 2>&1) || fail "Cannot detect node architecture"

case "$NODE_ARCH" in
  aarch64)
    TARGET_TRIPLE="aarch64-unknown-linux-musl"
    MUSL_IMAGE="messense/rust-musl-cross:aarch64-musl"
    ;;
  x86_64)
    TARGET_TRIPLE="x86_64-unknown-linux-musl"
    MUSL_IMAGE="messense/rust-musl-cross:x86_64-musl"
    ;;
  *)
    fail "Unsupported architecture: $NODE_ARCH"
    ;;
esac

cd "$REPO_ROOT"

# Build binaries
if ! $SKIP_BUILD; then
  info "Building shim + runtime for $TARGET_TRIPLE" | if_log

  if $QUIET; then
    docker run --rm \
      -v "$(pwd)":/work \
      -w /work \
      "$MUSL_IMAGE" \
      cargo build --release \
        --bin containerd-shim-reaper-v2 \
        --bin reaper-runtime \
        --target "$TARGET_TRIPLE" \
      >> "$LOG_FILE" 2>&1 || fail "Build failed. See $LOG_FILE"
  else
    docker run --rm \
      -v "$(pwd)":/work \
      -w /work \
      "$MUSL_IMAGE" \
      cargo build --release \
        --bin containerd-shim-reaper-v2 \
        --bin reaper-runtime \
        --target "$TARGET_TRIPLE" \
      2>&1 | tee -a "$LOG_FILE" || fail "Build failed. See $LOG_FILE"
  fi

  ok "Binaries built." | if_log
fi

SHIM_BINARY="target/$TARGET_TRIPLE/release/containerd-shim-reaper-v2"
RUNTIME_BINARY="target/$TARGET_TRIPLE/release/reaper-runtime"
[[ -f "$SHIM_BINARY" ]] || fail "Shim binary not found at $SHIM_BINARY"
[[ -f "$RUNTIME_BINARY" ]] || fail "Runtime binary not found at $RUNTIME_BINARY"

# Build container image using a temp context
info "Building container image $IMAGE_NAME" | if_log

TEMP_CONTEXT=$(mktemp -d /tmp/reaper-node-context-XXXXXX)
trap "rm -rf '$TEMP_CONTEXT'" EXIT

# Determine arch label for the image
case "$NODE_ARCH" in
  x86_64)  BINARCH="amd64" ;;
  aarch64) BINARCH="arm64" ;;
esac

mkdir -p "$TEMP_CONTEXT/binaries/$BINARCH"
cp "$SHIM_BINARY" "$TEMP_CONTEXT/binaries/$BINARCH/containerd-shim-reaper-v2"
cp "$RUNTIME_BINARY" "$TEMP_CONTEXT/binaries/$BINARCH/reaper-runtime"
cp scripts/install-node.sh "$TEMP_CONTEXT/install.sh"
chmod +x "$TEMP_CONTEXT/install.sh"

cat > "$TEMP_CONTEXT/Dockerfile" <<DOCKERFILE
FROM alpine:3.19
COPY binaries/ /binaries/
COPY install.sh /install.sh
ENTRYPOINT ["/install.sh"]
DOCKERFILE

if $QUIET; then
  docker build -t "$IMAGE_NAME" "$TEMP_CONTEXT" >> "$LOG_FILE" 2>&1
else
  docker build -t "$IMAGE_NAME" "$TEMP_CONTEXT" 2>&1 | tee -a "$LOG_FILE"
fi

docker image inspect "$IMAGE_NAME" > /dev/null 2>&1 || {
  fail "Image not found after build: $IMAGE_NAME"
}
ok "Image built: $IMAGE_NAME" | if_log

# Load into Kind
info "Loading image into Kind cluster '$CLUSTER_NAME'" | if_log

if $QUIET; then
  kind load docker-image "$IMAGE_NAME" --name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1
else
  kind load docker-image "$IMAGE_NAME" --name "$CLUSTER_NAME" 2>&1 | tee -a "$LOG_FILE"
fi

# Verify image is accessible inside the Kind node
docker exec "${CLUSTER_NAME}-control-plane" crictl images 2>/dev/null | grep -q "reaper-node" || {
  info "Image may not be loaded into Kind node (crictl check failed). Continuing..." | if_log
}
ok "Image loaded into Kind." | if_log
