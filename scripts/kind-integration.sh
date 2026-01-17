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

echo "Building shim and runtime binaries for Linux..."
# Build for x86_64 Linux (required for kind cluster)
# Note: This requires the x86_64-unknown-linux-gnu target to be installed
cargo build --release --target x86_64-unknown-linux-gnu --bin "$SHIM_BIN" --bin "$RUNTIME_BIN"
SHIM_BIN_PATH="$(pwd)/target/x86_64-unknown-linux-gnu/release/$SHIM_BIN"
RUNTIME_BIN_PATH="$(pwd)/target/x86_64-unknown-linux-gnu/release/$RUNTIME_BIN"

echo "Copy binaries into kind node..."
NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
docker cp "$SHIM_BIN_PATH" "$NODE_ID":/usr/local/bin/$SHIM_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$SHIM_BIN
docker cp "$RUNTIME_BIN_PATH" "$NODE_ID":/usr/local/bin/$RUNTIME_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$RUNTIME_BIN

echo "Waiting for Kubernetes API server to be ready..."
kubectl wait --for=condition=Ready node --all --timeout=300s 2>/dev/null || true
sleep 5

echo "Apply RuntimeClass and test pod..."
kubectl apply -f kubernetes/runtimeclass.yaml --validate=false 2>&1 || true

# Wait for RuntimeClass to be established
echo "Waiting for RuntimeClass to be established..."
for i in {1..30}; do
  if kubectl get runtimeclass reaper-v2 &>/dev/null; then
    echo "✅ RuntimeClass reaper-v2 is ready"
    break
  fi
  echo "Attempt $i/30: RuntimeClass not ready yet..."
  sleep 1
done

# Create a test pod that uses the Reaper runtime
echo "Creating test pod..."
cat << 'EOF' | kubectl apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: reaper-integration-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
  - name: test
    image: busybox
    command: ["/bin/echo", "Hello from Reaper!"]
EOF

echo "Waiting for pod to complete..."
kubectl wait --for=condition=Ready --timeout=30s pod/reaper-integration-test 2>/dev/null || echo "Pod did not reach Ready state (may complete directly)"
kubectl wait --for=jsonpath='{.status.phase}'=Succeeded --timeout=120s pod/reaper-integration-test

echo "✅ Test pod logs:"
kubectl logs pod/reaper-integration-test || true

echo ""
echo "✅ Kind integration test complete."
echo "Both binaries deployed:"
echo "  - Shim: /usr/local/bin/$SHIM_BIN"
echo "  - Runtime: /usr/local/bin/$RUNTIME_BIN"
