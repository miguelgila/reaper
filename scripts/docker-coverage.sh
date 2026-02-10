#!/usr/bin/env bash
# docker-coverage.sh — Run tarpaulin inside Docker for CI-parity coverage on macOS
#
# Uses the same tarpaulin config (tarpaulin.toml) that CI uses.
# A named Docker volume caches the cargo registry for faster repeat runs.
#
# Usage:
#   ./scripts/docker-coverage.sh          # Build image + run coverage
#   make coverage                          # Same thing, via Makefile

set -euo pipefail

IMAGE_NAME=reaper-dev
CACHE_VOL=reaper-cargo-cache

echo "Building image ${IMAGE_NAME}..."
docker build -q -t "${IMAGE_NAME}" .

# Create persistent cache volume for cargo registry (skip if exists)
docker volume create "${CACHE_VOL}" > /dev/null 2>&1 || true

echo "Running coverage (tarpaulin) inside container..."
# tarpaulin requires ptrace; add SYS_PTRACE cap and loosen seccomp.
# tarpaulin.toml in the project root provides --out, --timeout, --fail-under.
docker run --rm \
  --cap-add=SYS_PTRACE \
  --security-opt seccomp=unconfined \
  -v "$(pwd)":/usr/src/reaper \
  -v "${CACHE_VOL}":/usr/local/cargo/registry \
  -w /usr/src/reaper \
  -e CARGO_TERM_COLOR=always \
  -e RUST_BACKTRACE=1 \
  "${IMAGE_NAME}" \
  cargo tarpaulin

echo "Coverage report: cobertura.xml"
