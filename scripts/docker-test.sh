#!/usr/bin/env bash
set -euo pipefail

IMAGE_NAME=reaper-dev

echo "Building image ${IMAGE_NAME}..."
docker build -t "${IMAGE_NAME}" .

echo "Running tests inside container..."
docker run --rm \
  --user "$(id -u):$(id -g)" \
  -v "$(pwd)":/usr/src/reaper \
  -w /usr/src/reaper \
  -e CARGO_TERM_COLOR=always \
  -e RUST_BACKTRACE=1 \
  "${IMAGE_NAME}" \
  cargo test --verbose
