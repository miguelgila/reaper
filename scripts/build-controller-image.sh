#!/usr/bin/env bash
# build-controller-image.sh — Build reaper-controller container image and load into Kind
#
# This script builds the reaper-controller binary using musl cross-compilation,
# packages it into a minimal container image, and loads it into a Kind cluster.
#
# Usage:
#   ./scripts/build-controller-image.sh --cluster-name <name>
#   ./scripts/build-controller-image.sh --cluster-name <name> --quiet
#   ./scripts/build-controller-image.sh --cluster-name <name> --skip-build

set -euo pipefail

CLUSTER_NAME=""
SKIP_BUILD=false
QUIET=false
IMAGE_NAME="ghcr.io/miguelgila/reaper-controller:latest"
LOG_FILE="/tmp/reaper-controller-build.log"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
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
      echo "Build reaper-controller image and load into Kind cluster."
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

# ---------------------------------------------------------------------------
# Detect architecture
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Build controller binary
# ---------------------------------------------------------------------------
cd "$REPO_ROOT"

if ! $SKIP_BUILD; then
  info "Building reaper-controller for $TARGET_TRIPLE" | if_log

  if $QUIET; then
    docker run --rm \
      -v "$(pwd)":/work \
      -w /work \
      "$MUSL_IMAGE" \
      cargo build --release --features controller \
        --bin reaper-controller \
        --target "$TARGET_TRIPLE" \
      >> "$LOG_FILE" 2>&1 || fail "Controller build failed. See $LOG_FILE"
  else
    docker run --rm \
      -v "$(pwd)":/work \
      -w /work \
      "$MUSL_IMAGE" \
      cargo build --release --features controller \
        --bin reaper-controller \
        --target "$TARGET_TRIPLE" \
      2>&1 | tee -a "$LOG_FILE" || fail "Controller build failed. See $LOG_FILE"
  fi

  ok "Controller binary built." | if_log
fi

CONTROLLER_BINARY="target/$TARGET_TRIPLE/release/reaper-controller"
[[ -f "$CONTROLLER_BINARY" ]] || fail "Controller binary not found at $CONTROLLER_BINARY"

# ---------------------------------------------------------------------------
# Build minimal container image
# ---------------------------------------------------------------------------
info "Building container image $IMAGE_NAME" | if_log

TEMP_DOCKERFILE=$(mktemp /tmp/Dockerfile.controller-XXXXXX)
trap "rm -f '$TEMP_DOCKERFILE'" EXIT

cat > "$TEMP_DOCKERFILE" <<'DOCKERFILE'
FROM gcr.io/distroless/static-debian12:nonroot
COPY reaper-controller /reaper-controller
USER nonroot:nonroot
ENTRYPOINT ["/reaper-controller"]
DOCKERFILE

TEMP_CONTEXT=$(mktemp -d /tmp/reaper-controller-context-XXXXXX)
trap "rm -rf '$TEMP_CONTEXT' '$TEMP_DOCKERFILE'" EXIT

cp "$CONTROLLER_BINARY" "$TEMP_CONTEXT/reaper-controller"

if $QUIET; then
  docker build -f "$TEMP_DOCKERFILE" -t "$IMAGE_NAME" "$TEMP_CONTEXT" >> "$LOG_FILE" 2>&1
else
  docker build -f "$TEMP_DOCKERFILE" -t "$IMAGE_NAME" "$TEMP_CONTEXT" 2>&1 | tee -a "$LOG_FILE"
fi

ok "Image built: $IMAGE_NAME" | if_log

# ---------------------------------------------------------------------------
# Load image into Kind
# ---------------------------------------------------------------------------
info "Loading image into Kind cluster '$CLUSTER_NAME'" | if_log

if $QUIET; then
  kind load docker-image "$IMAGE_NAME" --name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1
else
  kind load docker-image "$IMAGE_NAME" --name "$CLUSTER_NAME" 2>&1 | tee -a "$LOG_FILE"
fi

ok "Image loaded into Kind." | if_log
