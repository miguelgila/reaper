#!/usr/bin/env bash
# setup.sh — Create a 3-node Kind cluster with Reaper installed and node labels applied.
#
# Usage:
#   ./examples/01-scheduling/setup.sh              # Create cluster
#   ./examples/01-scheduling/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - helm installed (https://helm.sh/)
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
# Help
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  echo "Usage: $0 [OPTIONS]"
  echo ""
  echo "Create a 3-node Kind cluster with Reaper installed and node labels."
  echo ""
  echo "Options:"
  echo "  --cleanup    Delete the Kind cluster"
  echo "  -h, --help   Show this help message"
  exit 0
fi

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
command -v helm >/dev/null 2>&1   || fail "helm not found. Install from https://helm.sh/"

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
# Create cluster and install Reaper via Helm
# ---------------------------------------------------------------------------
info "Setting up cluster '$CLUSTER_NAME' with Reaper (Helm)..."

"$REPO_ROOT/scripts/setup-playground.sh" \
  --cluster-name "$CLUSTER_NAME" \
  --kind-config "$KIND_CONFIG" \
  --quiet \
  || fail "Cluster setup failed. See /tmp/reaper-playground-setup.log"

ok "Cluster created and Reaper installed via Helm."

# Set KUBECONFIG for subsequent kubectl commands
export KUBECONFIG="/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG"

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

echo "${B}Reaper: installed via Helm${R}"

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
echo "${B}Connect:${R}"
echo "  export KUBECONFIG=/tmp/reaper-${CLUSTER_NAME}-kubeconfig"

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
