#!/bin/bash
# Kubernetes Integration Test Script for Reaper Shim v2
# This script performs end-to-end testing of the Reaper runtime in Kubernetes
# Supports both minikube and kind clusters

set -e

# Functions for cluster setup
setup_kind_cluster() {
    echo "🔧 Setting up kind cluster..."

    # Create kind config
    cat > "$TEST_DIR/kind-config.yaml" << EOF
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
- role: control-plane
  kubeadmConfigPatches:
  - |
    kind: InitConfiguration
    nodeRegistration:
      kubeletExtraArgs:
        node-labels: "reaper-runtime=true"
  extraMounts:
  - hostPath: $TEST_DIR/containerd-shim-reaper-v2
    containerPath: /usr/local/bin/containerd-shim-reaper-v2
EOF

    kind create cluster --config "$TEST_DIR/kind-config.yaml" --name reaper-test
    kubectl config use-context kind-reaper-test

    # Configure containerd in the kind node
    NODE_ID=$(docker ps --filter "name=reaper-test-control-plane" --format '{{.ID}}')
    docker exec "$NODE_ID" chmod +x /usr/local/bin/containerd-shim-reaper-v2
    ./scripts/configure-containerd.sh kind "$NODE_ID"
}

setup_minikube_cluster() {
    echo "🔧 Setting up minikube cluster..."

    # Start minikube with containerd
    minikube start --container-runtime=containerd --driver=docker

    # Detect architecture and build appropriate binary
    echo "Detecting minikube node architecture..."
    NODE_ARCH="$(minikube ssh -- uname -m | tr -d '\r')"
    echo "Node arch: $NODE_ARCH"

    # Build musl binary for the detected architecture
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

    echo "Building static (musl) shim binary..."
    docker run --rm \
        -v "$(pwd)":/work \
        -w /work \
        "$MUSL_IMAGE" \
        cargo build --release --bin containerd-shim-reaper-v2 --target "$TARGET_TRIPLE"

    SHIM_PATH="$(pwd)/target/$TARGET_TRIPLE/release/containerd-shim-reaper-v2"

    echo "Copy shim binary into minikube node..."
    minikube cp "$SHIM_PATH" "/usr/local/bin/containerd-shim-reaper-v2"
    minikube ssh -- "sudo chmod +x /usr/local/bin/containerd-shim-reaper-v2"

    echo "Configuring containerd in minikube..."
    ./scripts/configure-containerd.sh minikube
}

echo "🚀 Starting Reaper Shim v2 Kubernetes Integration Test"

# Parse command line arguments
CLUSTER_TYPE="${CLUSTER_TYPE:-minikube}"  # Default to minikube
KIND_CLEANUP="${KIND_CLEANUP:-true}"

while [[ $# -gt 0 ]]; do
  case $1 in
    --kind)
      CLUSTER_TYPE="kind"
      shift
      ;;
    --minikube)
      CLUSTER_TYPE="minikube"
      shift
      ;;
    --no-cleanup)
      KIND_CLEANUP="false"
      shift
      ;;
    *)
      echo "Unknown option: $1"
      echo "Usage: $0 [--kind|--minikube] [--no-cleanup]"
      exit 1
      ;;
  esac
done

echo "📋 Using cluster type: $CLUSTER_TYPE"

# Check prerequisites
command -v kubectl >/dev/null 2>&1 || { echo "❌ kubectl not found"; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "❌ docker not found"; exit 1; }

if [[ "$CLUSTER_TYPE" == "kind" ]]; then
    command -v kind >/dev/null 2>&1 || { echo "❌ kind not found"; exit 1; }
elif [[ "$CLUSTER_TYPE" == "minikube" ]]; then
    command -v minikube >/dev/null 2>&1 || { echo "❌ minikube not found"; exit 1; }
fi

echo "✅ Prerequisites check passed"

# Build the shim binary
echo "🔨 Building Reaper shim v2..."
cargo build --release --bin containerd-shim-reaper-v2

# Create a temporary directory for our test
TEST_DIR=$(mktemp -d)
echo "📁 Test directory: $TEST_DIR"

# Copy the shim binary to test directory
cp target/release/containerd-shim-reaper-v2 "$TEST_DIR/"

# Setup cluster based on type
if [[ "$CLUSTER_TYPE" == "kind" ]]; then
    setup_kind_cluster
elif [[ "$CLUSTER_TYPE" == "minikube" ]]; then
    setup_minikube_cluster
fi

echo "🎯 Testing Reaper RuntimeClass creation..."
# Apply the RuntimeClass
kubectl apply -f kubernetes/runtimeclass.yaml

# Wait for it to be ready
kubectl wait --for=condition=established --timeout=60s runtimeclass/reaper-v2 2>/dev/null || echo "⚠️  RuntimeClass wait may not be supported in all k8s versions"

echo "✅ RuntimeClass created successfully"

echo "🏃 Testing pod creation with Reaper runtime..."
# Create a test pod
cat > "$TEST_DIR/test-pod.yaml" << EOF
apiVersion: v1
kind: Pod
metadata:
  name: reaper-test-pod
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
  - name: test
    image: busybox
    command: ["/bin/echo", "Hello from Reaper runtime!"]
EOF

kubectl apply -f "$TEST_DIR/test-pod.yaml"

echo "⏳ Waiting for pod to complete..."
# Wait for pod to succeed
kubectl wait --for=condition=Ready --timeout=30s pod/reaper-test-pod 2>/dev/null || true
kubectl wait --for=jsonpath='{.status.phase}'=Succeeded --timeout=60s pod/reaper-test-pod

echo "📝 Checking pod logs..."
LOGS=$(kubectl logs reaper-test-pod 2>/dev/null || echo "No logs available")
echo "Pod logs: $LOGS"

if [[ "$LOGS" == *"Hello from Reaper runtime!"* ]]; then
    echo "✅ Integration test PASSED!"
    echo "🎉 Reaper shim v2 runtime successfully executed command in Kubernetes"
else
    echo "❌ Integration test FAILED!"
    echo "Expected logs not found. Pod status:"
    kubectl describe pod reaper-test-pod
    kubectl get pods
    exit 1
fi

# Cleanup
echo "🧹 Cleaning up..."
kubectl delete pod reaper-test-pod 2>/dev/null || true
kubectl delete runtimeclass reaper-v2 2>/dev/null || true

if [[ "$CLUSTER_TYPE" == "kind" && "$KIND_CLEANUP" == "true" ]]; then
    echo "🗑️  Deleting kind cluster..."
    kind delete cluster --name reaper-test
elif [[ "$CLUSTER_TYPE" == "minikube" ]]; then
    echo "🗑️  Deleting minikube cluster..."
    minikube delete
fi

rm -rf "$TEST_DIR"

echo "✨ Integration test completed successfully!"