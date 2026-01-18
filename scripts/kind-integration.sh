#!/usr/bin/env bash
set -euo pipefail

CLUSTER_NAME="reaper-ci"
SHIM_BIN="containerd-shim-reaper-v2"
RUNTIME_BIN="reaper-runtime"

# Check if binaries are already available (e.g., from CI pipeline)
if [ -f "target/release/$SHIM_BIN" ] && [ -f "target/release/$RUNTIME_BIN" ]; then
  echo "✅ Using pre-built binaries from target/release/"
  SHIM_BIN_PATH="$(pwd)/target/release/$SHIM_BIN"
  RUNTIME_BIN_PATH="$(pwd)/target/release/$RUNTIME_BIN"
  PRE_BUILT=true
else
  PRE_BUILT=false
fi

# Run Rust integration tests first to validate binaries
echo ""
echo "================================================"
echo "Running Rust integration tests..."
echo "================================================"
cargo test --test integration_basic_binary
cargo test --test integration_user_management
cargo test --test integration_shim
echo "✅ All Rust integration tests passed!"
echo ""

echo "Ensuring kind is installed..."
if ! command -v kind >/dev/null 2>&1; then
  curl -Lo ./kind https://kind.sigs.k8s.io/dl/v0.23.0/kind-$(uname | tr '[:upper:]' '[:lower:]')-amd64
  chmod +x ./kind
  sudo mv ./kind /usr/local/bin/kind
fi

echo "Creating kind cluster..."
# Use kind-config.yaml if it exists, otherwise use defaults
if [ -f "kind-config.yaml" ]; then
  kind create cluster --name "$CLUSTER_NAME" --config kind-config.yaml
else
  kind create cluster --name "$CLUSTER_NAME"
fi

# Only build if binaries weren't pre-built
if [ "$PRE_BUILT" = false ]; then
  echo "Detecting kind node architecture..."
  NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
  NODE_ARCH=$(docker exec "$NODE_ID" uname -m)
  echo "Node arch: $NODE_ARCH"

  echo "Building static (musl) Linux binaries inside Docker..."
  # Build statically linked musl binaries to avoid glibc mismatch.
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
      echo "Unsupported node arch: $NODE_ARCH" >&2
      exit 1
      ;;
  esac

  # Build both binaries in one docker run
  docker run --rm \
    -v "$(pwd)":/work \
    -w /work \
    "$MUSL_IMAGE" \
    cargo build --release --bin "$SHIM_BIN" --bin "$RUNTIME_BIN" --target "$TARGET_TRIPLE"

  SHIM_BIN_PATH="$(pwd)/target/$TARGET_TRIPLE/release/$SHIM_BIN"
  RUNTIME_BIN_PATH="$(pwd)/target/$TARGET_TRIPLE/release/$RUNTIME_BIN"
else
  echo "Skipping build step - using pre-built binaries"
  NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
fi

echo "Copy binaries into kind node..."
docker cp "$SHIM_BIN_PATH" "$NODE_ID":/usr/local/bin/$SHIM_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$SHIM_BIN
docker cp "$RUNTIME_BIN_PATH" "$NODE_ID":/usr/local/bin/$RUNTIME_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$RUNTIME_BIN

echo "Configuring containerd to use reaper-v2 shim runtime..."
./scripts/configure-containerd.sh kind "$NODE_ID"

echo "Verifying containerd config..."
docker exec "$NODE_ID" grep -A 3 'reaper-v2' /etc/containerd/config.toml

echo "Waiting for Kubernetes API server to be ready..."
kubectl wait --for=condition=Ready node --all --timeout=300s 2>/dev/null || true
sleep 5

echo "Creating RuntimeClass..."
# Create RuntimeClass (ignore pod creation failure due to missing service account)
cat << 'EOF' | kubectl apply -f - --validate=false
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper-v2
handler: reaper-v2
EOF

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

# Wait for default service account to be created
echo "Waiting for default service account..."
kubectl wait --for=jsonpath='{.metadata.name}'=default serviceaccount/default -n default --timeout=60s 2>/dev/null || {
  echo "Waiting for service account creation..."
  for i in {1..30}; do
    if kubectl get serviceaccount default -n default &>/dev/null; then
      echo "✅ Default service account is ready"
      break
    fi
    echo "Attempt $i/30: Service account not ready yet..."
    sleep 2
  done
}

# Create example pod
echo "Creating example pod..."
cat << 'EOF' | kubectl apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: reaper-example
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/echo", "Hello from Reaper runtime!"]
EOF

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
# Wait for pod to finish (either Succeeded or Failed)
FINAL_PHASE=""
for i in {1..60}; do
  PHASE=$(kubectl get pod reaper-integration-test -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
  if [ "$PHASE" = "Succeeded" ] || [ "$PHASE" = "Failed" ]; then
    FINAL_PHASE="$PHASE"
    echo "✅ Pod completed with phase: $PHASE"
    break
  fi
  if [ $i -eq 1 ]; then
    echo "Waiting for pod completion (current phase: $PHASE)..."
  fi
  sleep 2
done

echo "✅ Test pod logs:"
kubectl logs pod/reaper-integration-test || true

# Verify test pod succeeded
if [ "$FINAL_PHASE" != "Succeeded" ]; then
  echo "❌ Test pod did not succeed! Final phase: $FINAL_PHASE"
  exit 1
fi

echo ""
echo "================================================"
echo "✅ Kind integration test complete!"
echo "================================================"
echo "Both binaries deployed:"
echo "  - Shim: /usr/local/bin/$SHIM_BIN"
echo "  - Runtime: /usr/local/bin/$RUNTIME_BIN"
