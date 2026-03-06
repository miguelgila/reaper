#!/usr/bin/env bash
# Build the reaper-controller container image for the current architecture.
# Usage: ./scripts/build-controller-image.sh [--load-kind CLUSTER_NAME]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)
        RUST_TARGET="x86_64-unknown-linux-musl"
        MUSL_IMAGE="x86_64-musl"
        ;;
    aarch64|arm64)
        RUST_TARGET="aarch64-unknown-linux-musl"
        MUSL_IMAGE="aarch64-musl"
        ;;
    *)
        echo "Unsupported architecture: $ARCH" >&2
        exit 1
        ;;
esac

IMAGE_NAME="ghcr.io/miguelgila/reaper-controller:latest"

echo "Building reaper-controller image for $ARCH ($RUST_TARGET)..."
docker build \
    -f "$PROJECT_DIR/Dockerfile.controller" \
    --build-arg RUST_TARGET="$RUST_TARGET" \
    --build-arg MUSL_IMAGE="$MUSL_IMAGE" \
    -t "$IMAGE_NAME" \
    "$PROJECT_DIR"

echo "Built: $IMAGE_NAME"

# Optionally load into Kind cluster
if [[ "${1:-}" == "--load-kind" ]]; then
    CLUSTER="${2:-reaper-playground}"
    echo "Loading image into Kind cluster: $CLUSTER"
    kind load docker-image "$IMAGE_NAME" --name "$CLUSTER"
    echo "Loaded into Kind: $CLUSTER"
fi
