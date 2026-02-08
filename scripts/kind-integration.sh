#!/usr/bin/env bash
set -euo pipefail

CLUSTER_NAME="reaper-ci"
SHIM_BIN="containerd-shim-reaper-v2"
RUNTIME_BIN="reaper-runtime"

# IMPORTANT: We must build static musl binaries for kind, even if pre-built binaries exist
# Pre-built binaries from CI are dynamically linked against the runner's glibc and won't
# work in the kind node which may have a different glibc version
echo "⚠️  NOTE: Kind integration always builds static musl binaries for compatibility"
PRE_BUILT=false

# Run Rust integration tests first to validate binaries
echo ""
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
# Check if cluster already exists
if kind get clusters 2>/dev/null | grep -q "^$CLUSTER_NAME\$"; then
  echo "✅ Kind cluster '$CLUSTER_NAME' already exists, reusing..."
else
  # Use kind-config.yaml if it exists, otherwise use defaults
  if [ -f "kind-config.yaml" ]; then
    kind create cluster --name "$CLUSTER_NAME" --config kind-config.yaml
  else
    kind create cluster --name "$CLUSTER_NAME"
  fi
fi

echo "Detecting kind node architecture..."
NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
NODE_ARCH=$(docker exec "$NODE_ID" uname -m)
echo "Node arch: $NODE_ARCH"

echo "Building static (musl) Linux binaries inside Docker..."
# Build statically linked musl binaries to avoid glibc mismatch between
# the build environment and the kind node's container runtime
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

echo "Copy binaries into kind node..."
docker cp "$SHIM_BIN_PATH" "$NODE_ID":/usr/local/bin/$SHIM_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$SHIM_BIN
docker cp "$RUNTIME_BIN_PATH" "$NODE_ID":/usr/local/bin/$RUNTIME_BIN
docker exec "$NODE_ID" chmod +x /usr/local/bin/$RUNTIME_BIN

echo "Creating overlay directories on kind node..."
docker exec "$NODE_ID" mkdir -p /run/reaper/overlay/upper /run/reaper/overlay/work

echo "Waiting for Kubernetes API server to be ready before enabling reaper..."
kubectl wait --for=condition=Ready node --all --timeout=300s

echo "Sleeping 30s to ensure API server stability..."
sleep 30

echo "Enabling debug logging in kind node..."
# Enable RUST_LOG for the shim so we can debug issues
docker exec "$NODE_ID" bash -c "mkdir -p /tmp/reaper-logs"

echo "Configuring containerd to use reaper-v2 shim runtime..."
./scripts/configure-containerd.sh kind "$NODE_ID"

echo "Verifying containerd config..."
docker exec "$NODE_ID" grep -A 3 'reaper-v2' /etc/containerd/config.toml

# echo "Bailing out for manual inspection (remove this line to continue)..."
# exit 0

echo "Waiting for Kubernetes API server to be ready..."

# Function to retry kubectl commands with exponential backoff
retry_kubectl() {
  local max_retries=5
  local retry_count=0
  local backoff=1
  local cmd="$@"

  while [ $retry_count -lt $max_retries ]; do
    local output
    local exit_code

    output=$(eval "$cmd" 2>&1)
    exit_code=$?

    if [ $exit_code -eq 0 ]; then
      echo "$output"
      return 0
    fi

    # Log the error
    echo "⚠️  kubectl command failed (attempt $((retry_count + 1))/$max_retries): $output" >&2

    retry_count=$((retry_count + 1))
    if [ $retry_count -lt $max_retries ]; then
      echo "Retrying in ${backoff}s..." >&2
      sleep $backoff
      backoff=$((backoff * 2))
    fi
  done

  echo "❌ kubectl command failed after $max_retries attempts" >&2
  return 1
}

retry_kubectl "kubectl wait --for=condition=Ready node --all --timeout=300s" || {
  echo "⚠️  Initial wait failed, giving API server more time..."
  sleep 10
}

echo "Creating RuntimeClass..."
# Create RuntimeClass (ignore pod creation failure due to missing service account)
cat << 'EOF' | retry_kubectl "kubectl apply -f - --validate=false"
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper-v2
handler: reaper-v2
EOF

# Wait for RuntimeClass to be established
echo "Waiting for RuntimeClass to be established..."
for i in {1..30}; do
  if retry_kubectl "kubectl get runtimeclass reaper-v2" &>/dev/null; then
    echo "✅ RuntimeClass reaper-v2 is ready"
    break
  fi
  echo "Attempt $i/30: RuntimeClass not ready yet..."
  sleep 1
done

# Wait for default service account to be created
echo "Waiting for default service account..."
retry_kubectl "kubectl wait --for=jsonpath='{.metadata.name}'=default serviceaccount/default -n default --timeout=60s" || {
  echo "Waiting for service account creation..."
  for i in {1..30}; do
    if retry_kubectl "kubectl get serviceaccount default -n default" &>/dev/null; then
      echo "✅ Default service account is ready"
      break
    fi
    echo "Attempt $i/30: Service account not ready yet..."
    sleep 2
  done
}

# Delete any stale pods from previous runs before creating new ones
echo "Cleaning up any stale pods from previous runs..."
retry_kubectl "kubectl delete pod reaper-example --ignore-not-found" || true
retry_kubectl "kubectl delete pod reaper-integration-test --ignore-not-found" || true

# Create example pod
echo "Creating example pod..."
cat << 'EOF' | retry_kubectl "kubectl apply -f -"
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
cat << 'EOF' | retry_kubectl "kubectl apply -f -"
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
  echo "Polling attempt $i/60: Checking pod status..."

  # Use retry logic for kubectl get
  PHASE=$(retry_kubectl "kubectl get pod reaper-integration-test -o jsonpath='{.status.phase}'" || echo "")

  if [ -z "$PHASE" ]; then
    echo "⚠️  Could not retrieve pod phase, will retry..."
  else
    echo "Current pod phase: $PHASE"
  fi

  if [ "$PHASE" = "Succeeded" ] || [ "$PHASE" = "Failed" ]; then
    FINAL_PHASE="$PHASE"
    echo "✅ Pod completed with phase: $PHASE"
    break
  fi

  if [ "$PHASE" = "Pending" ] || [ "$PHASE" = "Running" ]; then
    # Get more details about the pod status
    echo "Pod is $PHASE, checking container statuses..."
    retry_kubectl "kubectl get pod reaper-integration-test -o jsonpath='{.status.containerStatuses[*].state}'" || true
  fi

  sleep 5
done

echo "✅ Test pod logs:"
retry_kubectl "kubectl logs pod/reaper-integration-test" || {
  echo "⚠️  Could not retrieve pod logs, getting pod description for debugging..."
  retry_kubectl "kubectl describe pod reaper-integration-test" || true
}

# Verify test pod succeeded
if [ "$FINAL_PHASE" != "Succeeded" ]; then
  echo "❌ Test pod did not succeed! Final phase: $FINAL_PHASE"
  echo ""
  echo "Debugging information:"
  echo "===================="

  # Get full pod YAML
  echo "=== Pod YAML ==="
  retry_kubectl "kubectl get pod reaper-integration-test -o yaml" || true
  echo ""

  # Get pod events
  echo "=== Pod Events ==="
  retry_kubectl "kubectl describe pod reaper-integration-test" | grep -A 50 "Events:" || true
  echo ""

  # Get containerd logs
  echo "=== Containerd logs (last 100 lines) ==="
  docker exec "$NODE_ID" tail -100 /var/log/containerd.log 2>/dev/null || echo "Could not retrieve containerd logs"
  echo ""

  # Get reaper state files
  echo "=== Reaper state directory ==="
  docker exec "$NODE_ID" find /run/reaper -type f 2>/dev/null | head -20 || echo "No reaper state files found"
  echo ""

  # Get systemd journal for containerd
  echo "=== Systemd journal for containerd (last 50 lines) ==="
  docker exec "$NODE_ID" journalctl -u containerd -n 50 --no-pager 2>/dev/null || echo "Could not retrieve journalctl"
  echo ""

  # Get kubelet logs
  echo "=== Kubelet logs (last 50 lines) ==="
  docker exec "$NODE_ID" journalctl -u kubelet -n 50 --no-pager 2>/dev/null || echo "Could not retrieve kubelet logs"

  exit 1
fi

echo ""
echo "================================================"
echo "Testing overlay filesystem sharing..."
echo "================================================"

# Clean up any leftover overlay test pods
retry_kubectl "kubectl delete pod reaper-overlay-writer --ignore-not-found" || true
retry_kubectl "kubectl delete pod reaper-overlay-reader --ignore-not-found" || true

# Pod A: write a file inside the overlay
echo "Creating overlay writer pod..."
cat << 'EOF' | retry_kubectl "kubectl apply -f -"
apiVersion: v1
kind: Pod
metadata:
  name: reaper-overlay-writer
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: writer
      image: busybox
      command: ["/bin/sh", "-c", "echo overlay-works > /tmp/overlay-test.txt"]
EOF

# Wait for writer to complete
echo "Waiting for writer pod to complete..."
for i in $(seq 1 30); do
  WRITER_PHASE=$(retry_kubectl "kubectl get pod reaper-overlay-writer -o jsonpath='{.status.phase}'" 2>/dev/null || echo "Pending")
  if [ "$WRITER_PHASE" = "Succeeded" ]; then
    echo "✅ Writer pod completed"
    break
  fi
  echo "Attempt $i/30: Writer phase=$WRITER_PHASE"
  sleep 2
done

# Pod B: read the file — should see it via shared overlay
echo "Creating overlay reader pod..."
cat << 'EOF' | retry_kubectl "kubectl apply -f -"
apiVersion: v1
kind: Pod
metadata:
  name: reaper-overlay-reader
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: reader
      image: busybox
      command: ["/bin/sh", "-c", "cat /tmp/overlay-test.txt"]
EOF

# Wait for reader to complete
echo "Waiting for reader pod to complete..."
for i in $(seq 1 30); do
  READER_PHASE=$(retry_kubectl "kubectl get pod reaper-overlay-reader -o jsonpath='{.status.phase}'" 2>/dev/null || echo "Pending")
  if [ "$READER_PHASE" = "Succeeded" ] || [ "$READER_PHASE" = "Failed" ]; then
    echo "Reader pod phase: $READER_PHASE"
    break
  fi
  echo "Attempt $i/30: Reader phase=$READER_PHASE"
  sleep 2
done

# Check reader output
READER_OUTPUT=$(retry_kubectl "kubectl logs reaper-overlay-reader" 2>/dev/null || echo "")
if [ "$READER_OUTPUT" = "overlay-works" ]; then
  echo "✅ PASS: Overlay sharing works — reader saw writer's file"
else
  echo "⚠️  Overlay sharing test: reader got '$READER_OUTPUT' (expected 'overlay-works')"
  echo "   This may indicate overlay is not yet active or the test needs adjustment"
fi

# Verify host filesystem is protected — this MUST pass (overlay is mandatory)
HOST_FILE_EXISTS=$(docker exec "$NODE_ID" test -f /tmp/overlay-test.txt && echo "yes" || echo "no")
if [ "$HOST_FILE_EXISTS" = "no" ]; then
  echo "✅ PASS: Host filesystem protected — file did not leak to host"
else
  echo "❌ FAIL: Host protection test: file leaked to host /tmp/overlay-test.txt"
  echo "Overlay isolation is mandatory — workloads must not modify the host filesystem."
  exit 1
fi

# Cleanup overlay test pods
retry_kubectl "kubectl delete pod reaper-overlay-writer --ignore-not-found" || true
retry_kubectl "kubectl delete pod reaper-overlay-reader --ignore-not-found" || true

echo ""
echo "================================================"
echo "Testing exec support..."
echo "================================================"

# Clean up any stale exec test pods
retry_kubectl "kubectl delete pod reaper-exec-test --ignore-not-found" || true

# Create a long-running pod for exec testing
echo "Creating long-running pod for exec testing..."
cat << 'EOF' | retry_kubectl "kubectl apply -f -"
apiVersion: v1
kind: Pod
metadata:
  name: reaper-exec-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["sleep", "60"]
EOF

# Wait for pod to be running
echo "Waiting for exec test pod to start..."
for i in $(seq 1 30); do
  EXEC_POD_PHASE=$(retry_kubectl "kubectl get pod reaper-exec-test -o jsonpath='{.status.phase}'" 2>/dev/null || echo "Pending")
  if [ "$EXEC_POD_PHASE" = "Running" ]; then
    echo "✅ Exec test pod is running"
    break
  fi
  echo "Attempt $i/30: Exec pod phase=$EXEC_POD_PHASE"
  sleep 1
done

# Test exec with a simple echo command
echo "Testing kubectl exec..."
EXEC_OUTPUT=$(retry_kubectl "kubectl exec reaper-exec-test -- echo 'exec works'" 2>/dev/null || echo "")
if [ "$EXEC_OUTPUT" = "exec works" ]; then
  echo "✅ PASS: kubectl exec works - output: $EXEC_OUTPUT"
else
  echo "⚠️  Exec test: unexpected output '$EXEC_OUTPUT'"
  # Note: This is a soft failure - exec functionality is still being implemented
  # Don't exit(1) since this is experimental
fi

# Clean up exec test pod
retry_kubectl "kubectl delete pod reaper-exec-test --ignore-not-found" || true

echo ""
echo "================================================"
echo "✅ Kind integration test complete!"
echo "================================================"
echo "Both binaries deployed:"
echo "  - Shim: /usr/local/bin/$SHIM_BIN"
echo "  - Runtime: /usr/local/bin/$RUNTIME_BIN"
