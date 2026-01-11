#!/usr/bin/env bash
set -euo pipefail

CLUSTER_NAME="reaper-ci"
SHIM_BIN="containerd-shim-reaper-v2"
RUNTIME_BIN="reaper-runtime"

echo "Ensuring kind is installed..."
if ! command -v kind >/dev/null 2>&1; then
  curl -Lo ./kind https://kind.sigs.k8s.io/dl/v0.23.0/kind-$(uname | tr '[:upper:]' '[:lower:]')-amd64
  chmod +x ./kind
  sudo mv ./kind /usr/local/bin/kind
fi

echo "Creating kind cluster..."
kind create cluster --name "$CLUSTER_NAME" --config kind-config.yaml

echo "Building shim and runtime binaries..."
cargo build --release --bin "$SHIM_BIN" --bin "$RUNTIME_BIN"
SHIM_BIN_PATH="$(pwd)/target/release/$SHIM_BIN"
RUNTIME_BIN_PATH="$(pwd)/target/release/$RUNTIME_BIN"

echo "Copy binaries into kind node..."
NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
docker cp "$SHIM_BIN_PATH" "$NODE_ID":/usr/local/bin/$SHIM_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$SHIM_BIN
docker cp "$RUNTIME_BIN_PATH" "$NODE_ID":/usr/local/bin/$RUNTIME_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$RUNTIME_BIN

echo "Enabling shim debug logging..."
docker exec "$NODE_ID" bash -c '
    mkdir -p /etc/systemd/system/containerd.service.d
    cat > /etc/systemd/system/containerd.service.d/reaper-shim-logging.conf <<EOF
[Service]
Environment="REAPER_SHIM_LOG=/var/log/reaper-shim.log"
EOF
    systemctl daemon-reload
'

echo "Configuring containerd in kind node..."
./scripts/configure-containerd.sh kind "$NODE_ID"

echo "Shim logs will be written to /var/log/reaper-shim.log"

echo "Apply RuntimeClass and test pod..."
kubectl apply -f kubernetes/runtimeclass.yaml
kubectl wait --for=condition=Ready --timeout=60s pod/reaper-example || {
  echo "Pod did not become ready; showing events:";
  kubectl describe pod/reaper-example || true;
  exit 1;
}
kubectl logs pod/reaper-example || true

echo "Kind integration test complete."
echo "Both binaries deployed:"
echo "  - Shim: /usr/local/bin/$SHIM_BIN"
echo "  - Runtime: /usr/local/bin/$RUNTIME_BIN"
