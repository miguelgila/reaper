#!/usr/bin/env bash
set -euo pipefail

CLUSTER_NAME="reaper-ci"
RUNTIME_BIN="reaper-runtime"

echo "Ensuring kind is installed..."
if ! command -v kind >/dev/null 2>&1; then
  curl -Lo ./kind https://kind.sigs.k8s.io/dl/v0.23.0/kind-$(uname | tr '[:upper:]' '[:lower:]')-amd64
  chmod +x ./kind
  sudo mv ./kind /usr/local/bin/kind
fi

echo "Creating kind cluster with containerd patch..."
kind create cluster --name "$CLUSTER_NAME" --config kind-config.yaml

echo "Building runtime binary..."
cargo build --release --bin "$RUNTIME_BIN"
BIN_PATH="$(pwd)/target/release/$RUNTIME_BIN"

echo "Copy runtime binary into kind node..."
NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
docker cp "$BIN_PATH" "$NODE_ID":/usr/local/bin/$RUNTIME_BIN

echo "Restarting containerd inside kind node..."
docker exec "$NODE_ID" bash -lc "pkill -HUP containerd || true"

echo "Apply RuntimeClass and test pod..."
kubectl apply -f k8s/runtimeclass.yaml
kubectl apply -f k8s/pod-reaper.yaml
kubectl wait --for=condition=Succeeded --timeout=120s pod/reaper-dummy || {
  echo "Pod did not succeed; showing logs:";
  kubectl logs pod/reaper-dummy || true;
  exit 1;
}
kubectl logs pod/reaper-dummy || true

echo "Kind integration test complete."
