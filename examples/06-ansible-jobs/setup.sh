#!/usr/bin/env bash
# setup.sh — Create a 10-node Kind cluster for the Ansible Jobs demo.
#
# Topology:
#   control-plane    (no workloads)
#   worker1-9        all equal — Jobs run across all workers
#
# Creates:
#   - ConfigMap 'nginx-playbook' containing an Ansible playbook
#
# Usage:
#   ./examples/06-ansible-jobs/setup.sh           # Create cluster and install via Helm
#   ./examples/06-ansible-jobs/setup.sh --cleanup # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - helm installed (https://helm.sh/)
#   - kubectl
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-ansible-jobs-demo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KIND_CONFIG="/tmp/reaper-ansible-jobs-kind-config.yaml"
LOG_FILE="/tmp/reaper-ansible-jobs-setup.log"

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
  echo "Create a 10-node Kind cluster for the Ansible jobs demo."
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
  kubectl delete configmap nginx-playbook --ignore-not-found 2>/dev/null || true
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
# Create ConfigMap from Ansible playbook file
# ---------------------------------------------------------------------------
info "Creating ConfigMap 'nginx-playbook'"

kubectl delete configmap nginx-playbook --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap nginx-playbook \
  --from-file=playbook.yml="$SCRIPT_DIR/nginx-playbook.ansible" >> "$LOG_FILE" 2>&1

ok "ConfigMap nginx-playbook created"

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
WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker | sort))

echo ""
echo "${C}========================================${R}"
echo "${B}Cluster ready: $CLUSTER_NAME${R}"
echo "${B}Reaper: installed via Helm${R}"
echo "${C}========================================${R}"
echo ""

echo "${B}Nodes (${#WORKERS[@]} workers):${R}"
kubectl get nodes -o custom-columns=\
'NAME:.metadata.name,STATUS:.status.conditions[-1].type,ROLES:.metadata.labels.node\.kubernetes\.io/role' \
  --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done

echo ""
echo "${B}ConfigMaps:${R}"
echo "  nginx-playbook  (Ansible playbook that installs and verifies nginx)"

echo ""
echo "${B}RuntimeClass:${R}"
echo "  $(kubectl get runtimeclass reaper-v2 -o custom-columns='NAME:.metadata.name,HANDLER:.handler' --no-headers 2>/dev/null)"

echo ""
echo "${B}Connect:${R}"
echo "  export KUBECONFIG=/tmp/reaper-${CLUSTER_NAME}-kubeconfig"

echo ""
echo "${C}────────────────────────────────────────${R}"
echo ""
echo "${Y}NOTE:${R} Jobs must be run in order. Job 1 installs Ansible into the"
echo "      shared overlay; Job 2 uses it to run a playbook. The overlay"
echo "      persists state between workloads on the same node."
echo ""
echo "Run the demo (in order):"
echo ""
echo "  ${B}# Step 1: Install Ansible on all workers${R}"
echo "  kubectl apply -f examples/06-ansible-jobs/install-ansible-job.yaml"
echo "  kubectl wait --for=condition=Complete job/install-ansible --timeout=300s"
echo "  kubectl logs -l job-name=install-ansible --all-containers --prefix"
echo ""
echo "  ${B}# Step 2: Run Ansible playbook to install nginx${R}"
echo "  kubectl apply -f examples/06-ansible-jobs/nginx-playbook-job.yaml"
echo "  kubectl wait --for=condition=Complete job/nginx-playbook --timeout=300s"
echo "  kubectl logs -l job-name=nginx-playbook --all-containers --prefix"
echo ""
echo "  ${B}# Clean up${R}"
echo "  kubectl delete -f examples/06-ansible-jobs/"
echo "  ./examples/06-ansible-jobs/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
