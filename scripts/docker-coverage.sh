#!/usr/bin/env bash
set -euo pipefail

IMAGE_NAME=reaper-dev

echo "Building image ${IMAGE_NAME}..."
docker build -t "${IMAGE_NAME}" .

echo "Running coverage (tarpaulin) inside container..."
# tarpaulin requires ptrace; add SYS_PTRACE cap and loosen seccomp.
docker run --rm \
  --cap-add=SYS_PTRACE \
  --security-opt seccomp=unconfined \
  -v "$(pwd)":/usr/src/reaper \
  -w /usr/src/reaper \
  -e CARGO_TERM_COLOR=always \
  -e RUST_BACKTRACE=1 \
  -e RUSTUP_NO_UPDATE_CHECK=1 \
  -e RUSTUP_AUTO_INSTALL=0 \
  -e RUSTUP_TOOLCHAIN=stable \
  -e CARGO_NET_OFFLINE=true \
  "${IMAGE_NAME}" \
  cargo tarpaulin --out Xml --timeout 600
