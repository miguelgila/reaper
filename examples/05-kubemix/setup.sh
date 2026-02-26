#!/usr/bin/env bash
# setup.sh — Create a 10-node Kind cluster for the kubemix demo.
#
# Topology:
#   control-plane    (no workloads)
#   worker1-3        workload-type=batch    ← Jobs
#   worker4-6        workload-type=daemon   ← DaemonSets
#   worker7-9        workload-type=service  ← Deployments
#
# Creates:
#   - ConfigMap 'batch-config'   (parameters for the batch report job)
#   - ConfigMap 'monitor-config' (thresholds for the node health DaemonSet)
#   - ConfigMap 'greeter-config' (settings for the greeter Deployment)
#
# Usage:
#   ./examples/05-kubemix/setup.sh              # Create cluster (latest release)
#   ./examples/05-kubemix/setup.sh v0.2.5       # Create cluster (specific release)
#   ./examples/05-kubemix/setup.sh --build      # Build binaries from source
#   ./examples/05-kubemix/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - Ansible installed (pip install ansible)
#   - curl (for downloading release binaries)
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-kubemix-demo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KIND_CONFIG="/tmp/reaper-kubemix-kind-config.yaml"
LOG_FILE="/tmp/reaper-kubemix-setup.log"

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
  echo "Usage: $0 [VERSION] [OPTIONS]"
  echo ""
  echo "Create a 10-node Kind cluster for the kubemix demo."
  echo ""
  echo "Arguments:"
  echo "  VERSION      Release version to install (e.g., v0.2.5). Default: latest"
  echo ""
  echo "Options:"
  echo "  --build      Build binaries from source instead of downloading a release"
  echo "  --cleanup    Delete the Kind cluster"
  echo "  -h, --help   Show this help message"
  exit 0
fi

# ---------------------------------------------------------------------------
# Cleanup mode
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--cleanup" ]]; then
  info "Deleting Kind cluster '$CLUSTER_NAME'..."
  kubectl delete configmap batch-config --ignore-not-found 2>/dev/null || true
  kubectl delete configmap monitor-config --ignore-not-found 2>/dev/null || true
  kubectl delete configmap greeter-config --ignore-not-found 2>/dev/null || true
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && ok "Cluster deleted." || warn "Cluster not found."
  exit 0
fi

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------
BUILD_MODE=false

for arg in "$@"; do
  case "$arg" in
    --build) BUILD_MODE=true ;;
  esac
done

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
  fail "Run this script from the repository root: ./examples/05-kubemix/setup.sh"
fi

ok "All prerequisites found."

# ---------------------------------------------------------------------------
# Resolve release version (skip when building from source)
# ---------------------------------------------------------------------------
if ! $BUILD_MODE; then
  # shellcheck source=../../scripts/lib/release-utils.sh
  source "$REPO_ROOT/scripts/lib/release-utils.sh"

  # Accept optional version argument (first non-flag arg)
  RELEASE_VERSION="latest"
  for arg in "$@"; do
    case "$arg" in
      --build|--cleanup|--help|-h) ;;
      *) RELEASE_VERSION="$arg" ;;
    esac
  done

  if [[ "$RELEASE_VERSION" == "latest" ]]; then
    info "Resolving latest release..."
    RELEASE_VERSION=$(resolve_latest_release) || \
      fail "Could not determine latest release. Specify a version: ./examples/05-kubemix/setup.sh v0.2.5"
    ok "Latest release: $RELEASE_VERSION"
  else
    ok "Using release: $RELEASE_VERSION"
  fi
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
# Build from source (--build mode)
# ---------------------------------------------------------------------------
if $BUILD_MODE; then
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
fi

# ---------------------------------------------------------------------------
# Install Reaper on all nodes via Ansible
# ---------------------------------------------------------------------------
if $BUILD_MODE; then
  info "Installing Reaper on all nodes (built from source)"
else
  info "Installing Reaper $RELEASE_VERSION on all nodes (pre-built release)"
fi

cd "$REPO_ROOT"

INSTALL_ARGS=(--kind "$CLUSTER_NAME")
if ! $BUILD_MODE; then
  INSTALL_ARGS+=(--release "$RELEASE_VERSION")
fi

./scripts/install-reaper.sh "${INSTALL_ARGS[@]}" >> "$LOG_FILE" 2>&1 || {
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
# Apply node labels — partition workers into three groups
# ---------------------------------------------------------------------------
info "Applying node labels"

WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker | sort))

if [[ ${#WORKERS[@]} -lt 9 ]]; then
  fail "Expected at least 9 worker nodes, found ${#WORKERS[@]}"
fi

# Workers 0-2: batch nodes (for Jobs)
for i in 0 1 2; do
  kubectl label node "${WORKERS[$i]}" workload-type=batch --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled workload-type=batch"
done

# Workers 3-5: daemon nodes (for DaemonSets)
for i in 3 4 5; do
  kubectl label node "${WORKERS[$i]}" workload-type=daemon --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled workload-type=daemon"
done

# Workers 6-8: service nodes (for Deployments)
for i in 6 7 8; do
  kubectl label node "${WORKERS[$i]}" workload-type=service --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled workload-type=service"
done

# ---------------------------------------------------------------------------
# Create ConfigMaps
# ---------------------------------------------------------------------------
info "Creating ConfigMap 'batch-config'"

kubectl delete configmap batch-config --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap batch-config \
  --from-literal=report.conf='# Batch Report Configuration
REPORT_TITLE="Reaper Cluster Node Report"
COLLECT_LOAD=true
COLLECT_MEMORY=true
COLLECT_DISK=true
COLLECT_NETWORK=true
OUTPUT_FORMAT=text' >> "$LOG_FILE" 2>&1

ok "ConfigMap batch-config created"

info "Creating ConfigMap 'monitor-config'"

kubectl delete configmap monitor-config --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap monitor-config \
  --from-literal=monitor.conf='# Node Health Monitor Configuration
CHECK_INTERVAL_SECONDS=30
LOAD_WARN_THRESHOLD=2.0
LOAD_CRIT_THRESHOLD=5.0
MEM_WARN_PERCENT=80
MEM_CRIT_PERCENT=95
REPORT_HOSTNAME=true' >> "$LOG_FILE" 2>&1

ok "ConfigMap monitor-config created"

info "Creating ConfigMap 'greeter-config'"

kubectl delete configmap greeter-config --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap greeter-config \
  --from-literal=greeter.conf='# Greeter Service Configuration
SERVICE_NAME="reaper-greeter"
SERVICE_VERSION="1.0.0"
GREETING_MESSAGE="Hello from Reaper!"
LOG_REQUESTS=true
HEALTH_CHECK_PATH=/healthz' >> "$LOG_FILE" 2>&1

ok "ConfigMap greeter-config created"

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
if $BUILD_MODE; then
  echo "${B}Reaper: built from source${R}"
else
  echo "${B}Reaper release: $RELEASE_VERSION${R}"
fi
echo "${C}========================================${R}"
echo ""

echo "${B}Nodes:${R}"
kubectl get nodes -o custom-columns=\
'NAME:.metadata.name,STATUS:.status.conditions[-1].type,WORKLOAD-TYPE:.metadata.labels.workload-type' \
  --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done

echo ""
echo "${B}Node groups:${R}"
echo "  ${G}batch${R}   (workers 1-3)  →  Jobs"
echo "  ${G}daemon${R}  (workers 4-6)  →  DaemonSets"
echo "  ${G}service${R} (workers 7-9)  →  Deployments"

echo ""
echo "${B}ConfigMaps:${R}"
echo "  batch-config    (report parameters for batch Job)"
echo "  monitor-config  (thresholds for health DaemonSet)"
echo "  greeter-config  (settings for greeter Deployment)"

echo ""
echo "${B}RuntimeClass:${R}"
echo "  $(kubectl get runtimeclass reaper-v2 -o custom-columns='NAME:.metadata.name,HANDLER:.handler' --no-headers 2>/dev/null)"

echo ""
echo "${C}────────────────────────────────────────${R}"
echo ""
echo "Run the demos (all three can run simultaneously):"
echo ""
echo "  ${B}# 1. Job — batch report on 'batch' nodes${R}"
echo "  kubectl apply -f examples/05-kubemix/batch-report-job.yaml"
echo "  kubectl logs -l job-name=batch-report --all-containers --prefix -f"
echo ""
echo "  ${B}# 2. DaemonSet — health monitor on 'daemon' nodes${R}"
echo "  kubectl apply -f examples/05-kubemix/node-health-daemonset.yaml"
echo "  kubectl logs -l app=node-health --all-containers --prefix -f"
echo ""
echo "  ${B}# 3. Deployment — greeter service on 'service' nodes${R}"
echo "  kubectl apply -f examples/05-kubemix/web-greeter-deployment.yaml"
echo "  kubectl logs -l app=web-greeter --all-containers --prefix -f"
echo ""
echo "  ${B}# Clean up${R}"
echo "  kubectl delete -f examples/05-kubemix/"
echo "  ./examples/05-kubemix/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
