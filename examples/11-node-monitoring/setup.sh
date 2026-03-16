#!/usr/bin/env bash
# setup.sh — Create a Kind cluster for the node monitoring demo.
#
# Topology:
#   control-plane    (no workloads)
#   worker-0         ← node_exporter (Reaper) + Prometheus (default runtime)
#   worker-1         ← node_exporter (Reaper)
#
# Usage:
#   ./examples/11-node-monitoring/setup.sh              # Create cluster
#   ./examples/11-node-monitoring/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind, kubectl, helm
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-node-monitoring"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG_FILE="/tmp/reaper-node-monitoring-setup.log"

# ---------------------------------------------------------------------------
# Colors (respects NO_COLOR)
# ---------------------------------------------------------------------------
if [[ -n "${NO_COLOR:-}" ]]; then
  B="" G="" Y="" C="" R=""
elif [[ -t 1 ]]; then
  B=$'\033[1m' G=$'\033[1;32m' Y=$'\033[1;33m' C=$'\033[1;36m' R=$'\033[0m'
else
  B="" G="" Y="" C="" R=""
fi

info()  { echo "${C}==> ${R}${B}$*${R}"; }
ok()    { echo " ${G}OK${R}  $*"; }
warn()  { echo " ${Y}!!${R}  $*"; }
fail()  { echo " ${Y}ERR${R} $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Help / Cleanup
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  echo "Usage: $0 [--cleanup]"
  echo ""
  echo "Create a Kind cluster for the Prometheus + Reaper node monitoring demo."
  echo ""
  echo "Options:"
  echo "  --cleanup    Delete the Kind cluster"
  echo "  -h, --help   Show this help"
  exit 0
fi

if [[ "${1:-}" == "--cleanup" ]]; then
  info "Deleting Kind cluster '$CLUSTER_NAME'..."
  kubectl delete clusterrolebinding prometheus --ignore-not-found 2>/dev/null || true
  kubectl delete clusterrole prometheus --ignore-not-found 2>/dev/null || true
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && ok "Cluster deleted." || warn "Cluster not found."
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
info "Preflight checks"
command -v docker >/dev/null 2>&1 || fail "docker not found."
docker info >/dev/null 2>&1       || fail "Docker daemon not running."
command -v kind >/dev/null 2>&1   || fail "kind not found."
command -v kubectl >/dev/null 2>&1 || fail "kubectl not found."
command -v helm >/dev/null 2>&1    || fail "helm not found."

if [[ ! -f "$REPO_ROOT/deploy/helm/reaper/Chart.yaml" ]]; then
  fail "Run this script from the repository root."
fi
ok "All prerequisites found."

# ---------------------------------------------------------------------------
# Create Kind cluster via setup-playground.sh (default 3-node config is fine)
# ---------------------------------------------------------------------------
info "Setting up cluster via setup-playground.sh"
"$REPO_ROOT/scripts/setup-playground.sh" \
  --cluster-name "$CLUSTER_NAME" \
  2>&1 | tee "$LOG_FILE"

# Export KUBECONFIG
KUBECONFIG_FILE="/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "${C}========================================${R}"
echo "${B}Cluster ready: $CLUSTER_NAME${R}"
echo "${C}========================================${R}"
echo ""
echo "${B}Nodes:${R}"
kubectl get nodes -o wide --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done
echo ""
echo "Deploy monitoring:"
echo "  kubectl apply -f examples/11-node-monitoring/"
echo ""
echo "Access Prometheus UI:"
echo "  kubectl port-forward svc/prometheus 9090:9090"
echo "  open http://localhost:9090"
echo ""
echo "Clean up:"
echo "  kubectl delete -f examples/11-node-monitoring/"
echo "  ./examples/11-node-monitoring/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
