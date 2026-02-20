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
#   ./examples/08-mix-container-runtime-engines/setup.sh              # Create cluster (latest release)
#   ./examples/08-mix-container-runtime-engines/setup.sh v0.2.5       # Create cluster (specific release)
#   ./examples/08-mix-container-runtime-engines/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - Ansible installed (pip install ansible)
#   - curl (for downloading release binaries)
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
command -v ansible-playbook >/dev/null 2>&1 || fail "ansible-playbook not found. Install with: pip install ansible"

if [[ ! -f "$REPO_ROOT/scripts/install-reaper.sh" ]]; then
  fail "Run this script from the repository root: ./examples/08-mix-container-runtime-engines/setup.sh"
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
    fail "Could not determine latest release. Specify a version: ./examples/08-mix-container-runtime-engines/setup.sh v0.2.5"
  ok "Latest release: $RELEASE_VERSION"
else
  ok "Using release: $RELEASE_VERSION"
fi

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
# Create or reuse cluster
# ---------------------------------------------------------------------------
info "Creating Kind cluster '$CLUSTER_NAME' (4 nodes)"

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
echo "  ${G}login${R}   (worker-0)    ←  OpenLDAP + SSSD (mixed runtimes)"
echo "  ${G}compute${R} (workers 1-2) ←  SSSD only (Reaper)"

echo ""
echo "${B}ConfigMaps:${R}"
echo "  base-config-playbook   (Ansible playbook for SSSD configuration)"

echo ""
echo "${B}RuntimeClass:${R}"
echo "  $(kubectl get runtimeclass reaper-v2 -o custom-columns='NAME:.metadata.name,HANDLER:.handler' --no-headers 2>/dev/null)"

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
echo "    -- getent passwd user1"
echo ""
echo "  ${B}# Clean up${R}"
echo "  kubectl delete -f examples/08-mix-container-runtime-engines/"
echo "  ./examples/08-mix-container-runtime-engines/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
