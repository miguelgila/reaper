#!/usr/bin/env bash
# setup.sh — Create a 10-node Kind cluster for the Ansible complex demo.
#
# Topology:
#   control-plane    (no workloads)
#   worker1-2        role=login    ← login/bastion nodes
#   worker3-9        role=compute  ← compute/batch nodes
#
# Creates:
#   - ConfigMap 'nginx-playbook' containing an Ansible playbook for login nodes
#
# Usage:
#   ./examples/07-ansible-complex/setup.sh              # Create cluster (latest release)
#   ./examples/07-ansible-complex/setup.sh v0.2.5       # Create cluster (specific release)
#   ./examples/07-ansible-complex/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - Ansible installed (pip install ansible)
#   - curl (for downloading release binaries)
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-ansible-complex-demo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KIND_CONFIG="/tmp/reaper-ansible-complex-kind-config.yaml"
LOG_FILE="/tmp/reaper-ansible-complex-setup.log"

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
  kubectl delete configmap nginx-login-playbook --ignore-not-found 2>/dev/null || true
  kubectl delete configmap htop-compute-playbook --ignore-not-found 2>/dev/null || true
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
  fail "Run this script from the repository root: ./examples/07-ansible-complex/setup.sh"
fi

ok "All prerequisites found."

# ---------------------------------------------------------------------------
# Resolve release version
# ---------------------------------------------------------------------------
# shellcheck source=../../scripts/lib/release-utils.sh
source "$REPO_ROOT/scripts/lib/release-utils.sh"

# Accept optional version argument (first non-flag arg)
RELEASE_VERSION="${1:-latest}"

if [[ "$RELEASE_VERSION" == "latest" ]]; then
  info "Resolving latest release..."
  RELEASE_VERSION=$(resolve_latest_release) || \
    fail "Could not determine latest release. Specify a version: ./examples/07-ansible-complex/setup.sh v0.2.5"
  ok "Latest release: $RELEASE_VERSION"
else
  ok "Using release: $RELEASE_VERSION"
fi

# ---------------------------------------------------------------------------
# Create Kind config for 10 nodes (1 control-plane + 9 workers)
# ---------------------------------------------------------------------------
info "Writing Kind cluster config"

cat > "$KIND_CONFIG" <<'EOF'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
  - role: worker
  - role: worker
  - role: worker
  - role: worker
  - role: worker
  - role: worker
  - role: worker
  - role: worker
  - role: worker
EOF

ok "Config written to $KIND_CONFIG"

# ---------------------------------------------------------------------------
# Create or reuse cluster
# ---------------------------------------------------------------------------
info "Creating Kind cluster '$CLUSTER_NAME' (10 nodes)"

if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
  warn "Cluster '$CLUSTER_NAME' already exists, reusing."
else
  kind create cluster --name "$CLUSTER_NAME" --config "$KIND_CONFIG" 2>&1 | tee "$LOG_FILE"
  ok "Cluster created."
fi

# ---------------------------------------------------------------------------
# Install Reaper on all nodes via Ansible (pre-built release)
# ---------------------------------------------------------------------------
info "Installing Reaper $RELEASE_VERSION on all nodes (pre-built release)"

cd "$REPO_ROOT"

./scripts/install-reaper.sh --kind "$CLUSTER_NAME" --release "$RELEASE_VERSION" >> "$LOG_FILE" 2>&1 || {
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
# Apply node labels — 2 login + 7 compute
# ---------------------------------------------------------------------------
info "Applying node labels"

WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker | sort))

if [[ ${#WORKERS[@]} -lt 9 ]]; then
  fail "Expected at least 9 worker nodes, found ${#WORKERS[@]}"
fi

# Workers 0-1: login nodes
for i in 0 1; do
  kubectl label node "${WORKERS[$i]}" role=login --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled role=login"
done

# Workers 2-8: compute nodes
for i in 2 3 4 5 6 7 8; do
  kubectl label node "${WORKERS[$i]}" role=compute --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled role=compute"
done

# ---------------------------------------------------------------------------
# Create ConfigMap from Ansible playbook file
# ---------------------------------------------------------------------------
info "Creating ConfigMap 'nginx-login-playbook'"

kubectl delete configmap nginx-login-playbook --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap nginx-login-playbook \
  --from-file=playbook.yml="$SCRIPT_DIR/nginx-login-playbook.ansible" >> "$LOG_FILE" 2>&1

ok "ConfigMap nginx-login-playbook created"

info "Creating ConfigMap 'htop-compute-playbook'"

kubectl delete configmap htop-compute-playbook --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap htop-compute-playbook \
  --from-file=playbook.yml="$SCRIPT_DIR/htop-compute-playbook.ansible" >> "$LOG_FILE" 2>&1

ok "ConfigMap htop-compute-playbook created"

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
echo "${B}Reaper release: $RELEASE_VERSION${R}"
echo "${C}========================================${R}"
echo ""

echo "${B}Nodes:${R}"
kubectl get nodes -o custom-columns=\
'NAME:.metadata.name,STATUS:.status.conditions[-1].type,ROLE:.metadata.labels.role' \
  --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done

echo ""
echo "${B}Node groups:${R}"
echo "  ${G}login${R}   (workers 1-2)  ←  bastion/login nodes"
echo "  ${G}compute${R} (workers 3-9)  ←  compute/batch nodes"

echo ""
echo "${B}ConfigMaps:${R}"
echo "  nginx-login-playbook   (Ansible playbook for login nodes)"
echo "  htop-compute-playbook  (Ansible playbook for compute nodes)"

echo ""
echo "${B}RuntimeClass:${R}"
echo "  $(kubectl get runtimeclass reaper-v2 -o custom-columns='NAME:.metadata.name,HANDLER:.handler' --no-headers 2>/dev/null)"

echo ""
echo "${C}────────────────────────────────────────${R}"
echo ""
echo "Run the demo (single apply — init containers handle ordering):"
echo ""
echo "  ${B}# Deploy everything at once${R}"
echo "  kubectl apply -f examples/07-ansible-complex/"
echo "  kubectl rollout status daemonset/ansible-bootstrap --timeout=300s"
echo "  kubectl rollout status daemonset/nginx-login --timeout=300s"
echo "  kubectl rollout status daemonset/htop-compute --timeout=300s"
echo ""
echo "  ${B}# Check output${R}"
echo "  kubectl logs -l app=ansible-bootstrap --all-containers --prefix"
echo "  kubectl logs -l app=nginx-login --all-containers --prefix"
echo "  kubectl logs -l app=htop-compute --all-containers --prefix"
echo ""
echo "  ${B}# Clean up${R}"
echo "  kubectl delete -f examples/07-ansible-complex/"
echo "  ./examples/07-ansible-complex/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
