#!/bin/bash
# Kubernetes Integration Test Script
# This script performs end-to-end testing of the Reaper runtime in Kubernetes

set -e

echo "🚀 Starting Reaper Kubernetes Integration Test"

# Check prerequisites
command -v kubectl >/dev/null 2>&1 || { echo "❌ kubectl not found"; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "❌ docker not found"; exit 1; }

# Check if we have a Kubernetes cluster
kubectl cluster-info >/dev/null 2>&1 || { echo "❌ No Kubernetes cluster available"; exit 1; }

echo "✅ Prerequisites check passed"

# Build the shim binary
echo "🔨 Building Reaper shim..."
cargo build --release --bin containerd-shim-reaper-v2

# Create a temporary directory for our test
TEST_DIR=$(mktemp -d)
echo "📁 Test directory: $TEST_DIR"

# Copy the shim binary to test directory
cp target/release/containerd-shim-reaper-v2 "$TEST_DIR/"

# Create a kind cluster with containerd (if not already running)
if ! kubectl get nodes >/dev/null 2>&1; then
    echo "🔧 Creating kind cluster..."
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
fi

echo "🎯 Testing Reaper RuntimeClass creation..."
# Apply the RuntimeClass
kubectl apply -f kubernetes/runtimeclass.yaml

# Wait for it to be ready
kubectl wait --for=condition=established --timeout=60s runtimeclass/reaper-v2

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
kubectl wait --for=condition=Ready --timeout=30s pod/reaper-test-pod || true
kubectl wait --for=jsonpath='{.status.phase}'=Succeeded --timeout=60s pod/reaper-test-pod

echo "📝 Checking pod logs..."
LOGS=$(kubectl logs reaper-test-pod)
echo "Pod logs: $LOGS"

if [[ "$LOGS" == *"Hello from Reaper runtime!"* ]]; then
    echo "✅ Integration test PASSED!"
    echo "🎉 Reaper runtime successfully executed command in Kubernetes"
else
    echo "❌ Integration test FAILED!"
    echo "Expected logs not found"
    kubectl describe pod reaper-test-pod
    exit 1
fi

# Cleanup
echo "🧹 Cleaning up..."
kubectl delete pod reaper-test-pod
kubectl delete runtimeclass reaper-v2

if [[ "${KIND_CLEANUP:-true}" == "true" ]]; then
    kind delete cluster --name reaper-test
fi

rm -rf "$TEST_DIR"

echo "✨ Integration test completed successfully!"