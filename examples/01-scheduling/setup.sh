#!/usr/bin/env bash
# setup.sh — Create a 3-node Kind cluster with Reaper installed and node labels applied.
#
# Usage:
#   ./examples/scheduling/setup.sh              # Create cluster
#   ./examples/scheduling/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - Ansible installed (pip install ansible)
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-scheduling-demo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KIND_CONFIG="/tmp/reaper-scheduling-kind-config.yaml"
LOG_FILE="/tmp/reaper-scheduling-setup.log"

# ---------------------------------------------------------------------------
# Colors (respects NO_COLOR)
# ---------------------------------------------------------------------------
if [[ -n "${NO_COLOR:-}" ]]; then
  B="" G="" Y="" C="" D="" R=""
elif [[ -t 1 ]]; then
  B=$'\033[1m'       # bold
  G=$'\033[1;32m'    # green
  Y=$'\033[1;33m'    # yellow
  C=$'\033[1;36m'    # cyan
  D=$'\033[0;37m'    # dim
  R=$'\033[0m'       # reset
else
  B="" G="" Y="" C="" D="" R=""
fi

info()  { echo "${C}==> ${R}${B}$*${R}"; }
ok()    { echo " ${G}OK${R}  $*"; }
warn()  { echo " ${Y}!!${R}  $*"; }
fail()  { echo " ${Y}ERR${R} $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Cleanup mode
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--cleanup" ]]; then
  info "Deleting Kind cluster '$CLUSTER_NAME'..."
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && ok "Cluster deleted." || warn "Cluster not found."
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------
info "Preflight checks"

command -v docker >/dev/null 2>&1 || fail "docker not found. Install Docker first."
docker info >/dev/null 2>&1       || fail "Docker daemon not running."
command -v kind >/dev/null 2>&1   || fail "kind not found. Install from https://kind.sigs.k8s.io/"
command -v kubectl >/dev/null 2>&1 || fail "kubectl not found."
command -v ansible-playbook >/dev/null 2>&1 || fail "ansible-playbook not found. Install with: pip install ansible"

if [[ ! -f "$REPO_ROOT/scripts/install-reaper.sh" ]]; then
  fail "Run this script from the repository root: ./examples/scheduling/setup.sh"
fi

ok "All prerequisites found."

# ---------------------------------------------------------------------------
# Create Kind config for 3 nodes (1 control-plane + 2 workers)
# ---------------------------------------------------------------------------
info "Writing Kind cluster config"

cat > "$KIND_CONFIG" <<'EOF'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
  - role: worker
  - role: worker
EOF

ok "Config written to $KIND_CONFIG"

# ---------------------------------------------------------------------------
# Create or reuse cluster
# ---------------------------------------------------------------------------
info "Creating Kind cluster '$CLUSTER_NAME' (3 nodes)"

if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
  warn "Cluster '$CLUSTER_NAME' already exists, reusing."
else
  kind create cluster --name "$CLUSTER_NAME" --config "$KIND_CONFIG" 2>&1 | tee "$LOG_FILE"
  ok "Cluster created."
fi

# ---------------------------------------------------------------------------
# Build static musl binaries for Kind nodes
# ---------------------------------------------------------------------------
info "Building Reaper binaries for Kind nodes"

cd "$REPO_ROOT"

NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
NODE_ARCH=$(docker exec "$NODE_ID" uname -m 2>&1) || fail "Cannot detect node architecture"

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
    fail "Unsupported architecture: $NODE_ARCH"
    ;;
esac

echo "  Architecture: $NODE_ARCH ($TARGET_TRIPLE)"

docker run --rm \
  -v "$(pwd)":/work \
  -w /work \
  "$MUSL_IMAGE" \
  cargo build --release \
    --bin containerd-shim-reaper-v2 \
    --bin reaper-runtime \
    --target "$TARGET_TRIPLE" \
  >> "$LOG_FILE" 2>&1 || {
    fail "Build failed. See $LOG_FILE for details."
  }

mkdir -p target/release
cp "target/$TARGET_TRIPLE/release/containerd-shim-reaper-v2" target/release/
cp "target/$TARGET_TRIPLE/release/reaper-runtime" target/release/

ok "Binaries built."

# ---------------------------------------------------------------------------
# Install Reaper on all nodes via Ansible
# ---------------------------------------------------------------------------
info "Installing Reaper runtime on all nodes"

./scripts/install-reaper.sh --kind "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1 || {
  fail "Ansible install failed. See $LOG_FILE for details."
}

ok "Reaper installed on all nodes."

# ---------------------------------------------------------------------------
# Wait for nodes to be ready
# ---------------------------------------------------------------------------
info "Waiting for all nodes to be Ready"

kubectl wait --for=condition=Ready node --all --timeout=120s >> "$LOG_FILE" 2>&1 || {
  fail "Nodes did not become Ready. See $LOG_FILE"
}

ok "All nodes Ready."

# ---------------------------------------------------------------------------
# Apply node labels for the subset example
# ---------------------------------------------------------------------------
info "Applying node labels"

WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker))

if [[ ${#WORKERS[@]} -lt 2 ]]; then
  fail "Expected at least 2 worker nodes, found ${#WORKERS[@]}"
fi

# Label the first worker as a "login" node (used by subset-nodes-daemonset.yaml)
kubectl label node "${WORKERS[0]}" node-role=login --overwrite >> "$LOG_FILE" 2>&1
ok "${WORKERS[0]} labeled node-role=login"

# Second worker stays unlabeled (demonstrates that the subset DaemonSet skips it)

# ---------------------------------------------------------------------------
# Verify RuntimeClass
# ---------------------------------------------------------------------------
info "Verifying RuntimeClass"

for i in $(seq 1 15); do
  if kubectl get runtimeclass reaper-v2 &>/dev/null; then
    ok "RuntimeClass reaper-v2 available."
    break
  fi
  sleep 1
done

kubectl get runtimeclass reaper-v2 &>/dev/null || fail "RuntimeClass reaper-v2 not found"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "${C}========================================${R}"
echo "${B}Cluster ready: $CLUSTER_NAME${R}"
echo "${C}========================================${R}"
echo ""

echo "${B}Nodes:${R}"
kubectl get nodes -o custom-columns=\
'NAME:.metadata.name,STATUS:.status.conditions[-1].type,ROLES:.metadata.labels.node\.kubernetes\.io/role,NODE-ROLE:.metadata.labels.node-role' \
  --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done

echo ""
echo "${B}RuntimeClass:${R}"
echo "  $(kubectl get runtimeclass reaper-v2 -o custom-columns='NAME:.metadata.name,HANDLER:.handler' --no-headers 2>/dev/null)"

echo ""
echo "${B}Node labels:${R}"
for node in $(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name); do
  role_label=$(kubectl get node "$node" -o jsonpath='{.metadata.labels.node-role}' 2>/dev/null)
  if [[ -n "$role_label" ]]; then
    echo "  $node  →  node-role=$role_label"
  else
    echo "  $node  →  ${D}(no node-role label)${R}"
  fi
done

echo ""
echo "${C}────────────────────────────────────────${R}"
echo ""
echo "Try the examples:"
echo ""
echo "  ${B}# Run on ALL nodes${R}"
echo "  kubectl apply -f examples/scheduling/all-nodes-daemonset.yaml"
echo "  kubectl get pods -l app=node-monitor -o wide"
echo "  kubectl logs -l app=node-monitor --all-containers --prefix"
echo ""
echo "  ${B}# Run only on 'login' nodes${R}"
echo "  kubectl apply -f examples/scheduling/subset-nodes-daemonset.yaml"
echo "  kubectl get pods -l app=login-monitor -o wide"
echo "  kubectl logs -l app=login-monitor --all-containers --prefix"
echo ""
echo "  ${B}# Clean up${R}"
echo "  kubectl delete -f examples/scheduling/"
echo "  ./examples/scheduling/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
