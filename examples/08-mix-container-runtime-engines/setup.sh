#!/usr/bin/env bash
# setup.sh — Create a 4-node Kind cluster for the mixed runtime demo.
#
# Topology:
#   control-plane    (no workloads)
#   worker-0         role=login    ← OpenLDAP (default runtime) + Reaper workloads
#   worker-1         role=compute  ← Reaper workloads only
#   worker-2         role=compute  ← Reaper workloads only
#
# Creates:
#   - ConfigMap 'base-config-playbook' containing an Ansible playbook for SSSD
#
# Usage:
#   ./examples/08-mix-container-runtime-engines/setup.sh           # Create cluster and install via Helm
#   ./examples/08-mix-container-runtime-engines/setup.sh --cleanup # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - helm installed (https://helm.sh/)
#   - kubectl
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-mixed-runtime-demo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KIND_CONFIG="/tmp/reaper-mixed-runtime-kind-config.yaml"
LOG_FILE="/tmp/reaper-mixed-runtime-setup.log"

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
  echo "Create a 4-node Kind cluster for the mixed runtime demo."
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
  kubectl delete configmap base-config-playbook --ignore-not-found 2>/dev/null || true
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
# Create Kind config for 4 nodes (1 control-plane + 3 workers)
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
# Apply node labels — 1 login + 2 compute
# ---------------------------------------------------------------------------
info "Applying node labels"

WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker | sort))

if [[ ${#WORKERS[@]} -lt 3 ]]; then
  fail "Expected at least 3 worker nodes, found ${#WORKERS[@]}"
fi

# Worker 0: login node (also runs OpenLDAP via default runtime)
kubectl label node "${WORKERS[0]}" role=login --overwrite >> "$LOG_FILE" 2>&1
ok "${WORKERS[0]} labeled role=login"

# Workers 1-2: compute nodes
for i in 1 2; do
  kubectl label node "${WORKERS[$i]}" role=compute --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled role=compute"
done

# ---------------------------------------------------------------------------
# Create ConfigMap from Ansible playbook file
# ---------------------------------------------------------------------------
info "Creating ConfigMap 'base-config-playbook'"

kubectl delete configmap base-config-playbook --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap base-config-playbook \
  --from-file=playbook.yml="$SCRIPT_DIR/base-config-playbook.ansible" >> "$LOG_FILE" 2>&1

ok "ConfigMap base-config-playbook created"

# ---------------------------------------------------------------------------
# Configure DNS mode on all nodes
# ---------------------------------------------------------------------------
info "Configuring DNS mode (kubernetes) on all nodes"

for node in $(kind get nodes --name "$CLUSTER_NAME" 2>/dev/null); do
  docker exec "$node" bash -c \
    'mkdir -p /etc/reaper && echo "REAPER_DNS_MODE=kubernetes" >> /etc/reaper/reaper.conf' \
    >> "$LOG_FILE" 2>&1
  ok "$node: REAPER_DNS_MODE=kubernetes"
done

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
echo "${B}Reaper: installed via Helm${R}"
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
echo "  ${G}login${R}   (worker-0)    ←  OpenLDAP + SSSD (mixed runtimes)"
echo "  ${G}compute${R} (workers 1-2) ←  SSSD only (Reaper)"

echo ""
echo "${B}ConfigMaps:${R}"
echo "  base-config-playbook   (Ansible playbook for SSSD configuration)"

echo ""
echo "${B}DNS Mode:${R}"
echo "  REAPER_DNS_MODE=kubernetes (configured in /etc/reaper/reaper.conf on each node)"

echo ""
echo "${B}RuntimeClass:${R}"
echo "  $(kubectl get runtimeclass reaper-v2 -o custom-columns='NAME:.metadata.name,HANDLER:.handler' --no-headers 2>/dev/null)"

echo ""
echo "${B}Connect:${R}"
echo "  export KUBECONFIG=/tmp/reaper-${CLUSTER_NAME}-kubeconfig"

echo ""
echo "${C}────────────────────────────────────────${R}"
echo ""
echo "Run the demo (single apply — init containers handle ordering):"
echo ""
echo "  ${B}# Deploy everything at once${R}"
echo "  kubectl apply -f examples/08-mix-container-runtime-engines/"
echo "  kubectl rollout status deployment/openldap --timeout=120s"
echo "  kubectl rollout status daemonset/base-deps --timeout=300s"
echo "  kubectl rollout status daemonset/base-config --timeout=300s"
echo ""
echo "  ${B}# Check output${R}"
echo "  kubectl logs -l app=base-deps --all-containers --prefix"
echo "  kubectl logs -l app=base-config --all-containers --prefix"
echo ""
echo "  ${B}# Verify LDAP users are visible from any worker${R}"
echo "  kubectl run ldap-test --image=busybox --restart=Never --rm -it \\"
echo "    --overrides='{\"spec\":{\"runtimeClassName\":\"reaper-v2\"}}' \\"
echo "    -- sh -c 'sleep 1 && getent passwd user1'"
echo ""
echo "  ${B}# Clean up${R}"
echo "  kubectl delete -f examples/08-mix-container-runtime-engines/"
echo "  ./examples/08-mix-container-runtime-engines/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
