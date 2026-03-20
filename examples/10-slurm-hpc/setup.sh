#!/usr/bin/env bash
# setup.sh — Create a 4-node Kind cluster for the Slurm HPC demo.
#
# Topology:
#   control-plane    (no workloads)
#   worker-0         role=slurmctld  ← Slurm controller (default runtime)
#   worker-1         role=compute    ← slurmd via Reaper
#   worker-2         role=compute    ← slurmd via Reaper
#
# Usage:
#   ./examples/10-slurm-hpc/setup.sh                       # Build from source
#   ./examples/10-slurm-hpc/setup.sh --release              # Use latest release
#   ./examples/10-slurm-hpc/setup.sh --release v0.2.14      # Use specific release
#   ./examples/10-slurm-hpc/setup.sh --cleanup              # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind, kubectl, helm
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-slurm-hpc"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG_FILE="/tmp/reaper-slurm-hpc-setup.log"

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
RELEASE_ARGS=()

while [[ $# -gt 0 ]]; do
  case $1 in
    --help|-h)
      echo "Usage: $0 [OPTIONS]"
      echo ""
      echo "Create a 4-node Kind cluster for the Slurm HPC mixed-runtime demo."
      echo ""
      echo "Options:"
      echo "  --release [VERSION]  Use pre-built images from GHCR (default: latest)"
      echo "  --cleanup            Delete the Kind cluster"
      echo "  -h, --help           Show this help"
      exit 0
      ;;
    --cleanup)
      info "Deleting Kind cluster '$CLUSTER_NAME'..."
      kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && ok "Cluster deleted." || warn "Cluster not found."
      exit 0
      ;;
    --release)
      RELEASE_ARGS=(--release)
      if [[ -n "${2:-}" && "${2:-}" == v* ]]; then
        RELEASE_ARGS=(--release "$2")
        shift
      fi
      shift
      ;;
    *)
      fail "Unknown option: $1 (use -h for help)"
      ;;
  esac
done

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
# Create Kind cluster via setup-playground.sh
# ---------------------------------------------------------------------------
KIND_CONFIG=$(mktemp /tmp/reaper-slurm-kind-XXXXXX.yaml)
cat > "$KIND_CONFIG" <<'EOF'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
  - role: worker
  - role: worker
  - role: worker
containerdConfigPatches:
  - |
    [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
      runtime_type = "io.containerd.reaper.v2"
      sandbox_mode = "podsandbox"
      pod_annotations = ["reaper.runtime/*"]
EOF

info "Setting up cluster via setup-playground.sh"
"$REPO_ROOT/scripts/setup-playground.sh" \
  --cluster-name "$CLUSTER_NAME" \
  --kind-config "$KIND_CONFIG" \
  "${RELEASE_ARGS[@]}" \
  2>&1 | tee "$LOG_FILE"

rm -f "$KIND_CONFIG"

# Export KUBECONFIG
KUBECONFIG_FILE="/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"

# Ensure ReaperOverlay CRD is installed (idempotent — may already exist via Helm)
info "Ensuring ReaperOverlay CRD is installed"
kubectl apply -f "$REPO_ROOT/deploy/kubernetes/crds/reaperoverlays.reaper.io.yaml" >> "$LOG_FILE" 2>&1
ok "ReaperOverlay CRD installed"

# ---------------------------------------------------------------------------
# Label nodes
# ---------------------------------------------------------------------------
info "Labeling nodes"
WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker | sort))

if [[ ${#WORKERS[@]} -lt 3 ]]; then
  fail "Expected at least 3 workers, found ${#WORKERS[@]}"
fi

kubectl label node "${WORKERS[0]}" role=slurmctld --overwrite >> "$LOG_FILE" 2>&1
ok "${WORKERS[0]} labeled role=slurmctld"

COMPUTE_NODES=()
for i in 1 2; do
  kubectl label node "${WORKERS[$i]}" role=compute --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled role=compute"
  COMPUTE_NODES+=("${WORKERS[$i]}")
done

# ---------------------------------------------------------------------------
# Generate Slurm config with actual node names
# ---------------------------------------------------------------------------
info "Generating slurm-config ConfigMap with actual node names"

NODE_LIST=$(IFS=,; echo "${COMPUTE_NODES[*]}")

# Detect CPU count from a worker node
NODE_CPUS=$(docker exec "${COMPUTE_NODES[0]}" nproc 2>/dev/null || echo "2")

kubectl delete configmap slurm-config --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap slurm-config \
  --from-literal=slurm.conf="$(cat <<SLURMEOF
# Auto-generated Slurm configuration for Kind demo
ClusterName=reaper-demo
SlurmctldHost=slurmctld

# Run as root (Kind demo — overlay filters /etc/gshadow, see issue #41)
SlurmUser=root
SlurmdUser=root

# Scheduling
SchedulerType=sched/backfill
SelectType=select/cons_tres

# Logging
SlurmctldLogFile=/var/log/slurmctld.log
SlurmdLogFile=/var/log/slurmd.log

# Process tracking (no cgroup — Kind nodes lack dbus/systemd)
ProctrackType=proctrack/linuxproc
TaskPlugin=task/none

# Timeouts
SlurmctldTimeout=60
SlurmdTimeout=60
WaitTime=30

# Authentication
AuthType=auth/munge

# Compute nodes (auto-populated by setup.sh)
NodeName=${NODE_LIST} CPUs=${NODE_CPUS} RealMemory=1024 State=UNKNOWN
PartitionName=batch Nodes=ALL Default=YES MaxTime=INFINITE State=UP
SLURMEOF
)" \
  --from-literal=cgroup.conf="CgroupPlugin=cgroup/v1" \
  >> "$LOG_FILE" 2>&1

ok "slurm-config created with nodes: $NODE_LIST"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "${C}========================================${R}"
echo "${B}Cluster ready: $CLUSTER_NAME${R}"
echo "${C}========================================${R}"
echo ""
echo "${B}Nodes:${R}"
kubectl get nodes -o custom-columns='NAME:.metadata.name,STATUS:.status.conditions[-1].type,ROLE:.metadata.labels.role' --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done
echo ""
echo "${B}Connect:${R}"
echo "  export KUBECONFIG=$KUBECONFIG_FILE"
echo ""
echo "Deploy Slurm:"
echo "  kubectl apply -f examples/10-slurm-hpc/slurm-overlay.yaml"
echo "  kubectl apply -f examples/10-slurm-hpc/munge-secret.yaml"
echo "  kubectl apply -f examples/10-slurm-hpc/slurmctld-deployment.yaml"
echo "  kubectl apply -f examples/10-slurm-hpc/slurmd-daemonset.yaml"
echo ""
echo "Clean up:"
echo "  ./examples/10-slurm-hpc/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
