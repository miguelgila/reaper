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
#   ./examples/06-ansible-jobs/setup.sh              # Create cluster (latest release)
#   ./examples/06-ansible-jobs/setup.sh v0.2.5       # Create cluster (specific release)
#   ./examples/06-ansible-jobs/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - Ansible installed (pip install ansible)
#   - curl (for downloading release binaries)
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
command -v ansible-playbook >/dev/null 2>&1 || fail "ansible-playbook not found. Install with: pip install ansible"

if [[ ! -f "$REPO_ROOT/scripts/install-reaper.sh" ]]; then
  fail "Run this script from the repository root: ./examples/06-ansible-jobs/setup.sh"
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
    fail "Could not determine latest release. Specify a version: ./examples/06-ansible-jobs/setup.sh v0.2.5"
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
# Create ConfigMap from Ansible playbook file
# ---------------------------------------------------------------------------
info "Creating ConfigMap 'nginx-playbook'"

kubectl delete configmap nginx-playbook --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap nginx-playbook \
  --from-file=playbook.yml="$SCRIPT_DIR/nginx-playbook.ansible.yml" >> "$LOG_FILE" 2>&1

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
echo "${B}Reaper release: $RELEASE_VERSION${R}"
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
