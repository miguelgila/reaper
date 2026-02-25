#!/usr/bin/env bash
# setup.sh — Create a 4-node Kind cluster for the client-server runAs demo.
#
# Same topology as client-server, but all workloads run as a shared
# non-root user (demo-svc, UID 1500 / GID 1500) created on every node,
# mimicking an LDAP/directory-service environment where UIDs are
# consistent across the cluster.
#
# Topology:
#   control-plane  (no workloads)
#   worker         role=server   ← TCP server listens here
#   worker2        role=client   ← TCP client connects from here
#   worker3        role=client   ← TCP client connects from here
#
# Usage:
#   ./examples/client-server-runas/setup.sh              # Create cluster
#   ./examples/client-server-runas/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - Ansible installed (pip install ansible)
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-runas-demo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KIND_CONFIG="/tmp/reaper-runas-kind-config.yaml"
LOG_FILE="/tmp/reaper-runas-setup.log"

# Shared user (same on every node, like LDAP)
DEMO_USER="demo-svc"
DEMO_UID=1500
DEMO_GID=1500

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
  echo "Create a 3-node Kind cluster for the client-server runAs demo."
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
  kubectl delete configmap server-config --ignore-not-found 2>/dev/null || true
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && ok "Cluster deleted." || warn "Cluster not found."
  exit 0
fi

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------
BUILD_MODE=false
RELEASE_VERSION="latest"

for arg in "$@"; do
  case "$arg" in
    --build)   BUILD_MODE=true ;;
    --cleanup|--help|-h) ;;  # already handled above
    *)         RELEASE_VERSION="$arg" ;;
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
  fail "Run this script from the repository root: ./examples/client-server-runas/setup.sh"
fi

ok "All prerequisites found."

# ---------------------------------------------------------------------------
# Resolve release version
# ---------------------------------------------------------------------------
if ! $BUILD_MODE; then
  # shellcheck source=../../scripts/lib/release-utils.sh
  source "$REPO_ROOT/scripts/lib/release-utils.sh"

  if [[ "$RELEASE_VERSION" == "latest" ]]; then
    info "Resolving latest release..."
    RELEASE_VERSION=$(resolve_latest_release) || \
      fail "Could not determine latest release. Specify a version or use --build."
    ok "Latest release: $RELEASE_VERSION"
  else
    ok "Using release: $RELEASE_VERSION"
  fi
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
# Install Reaper on all nodes
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

info "Installing Reaper on all nodes"

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
# Create shared user on all worker nodes (mimics LDAP)
# ---------------------------------------------------------------------------
info "Creating shared user $DEMO_USER (UID=$DEMO_UID, GID=$DEMO_GID) on all worker nodes"

WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker | sort))

if [[ ${#WORKERS[@]} -lt 3 ]]; then
  fail "Expected at least 3 worker nodes, found ${#WORKERS[@]}"
fi

for worker in "${WORKERS[@]}"; do
  # Create group and user idempotently
  docker exec "$worker" sh -c "
    if getent group $DEMO_GID >/dev/null 2>&1; then
      echo 'Group $DEMO_GID already exists'
    else
      groupadd --gid $DEMO_GID $DEMO_USER
    fi
    if id -u $DEMO_UID >/dev/null 2>&1; then
      echo 'User $DEMO_UID already exists'
    else
      useradd --uid $DEMO_UID --gid $DEMO_GID --no-create-home --shell /bin/sh $DEMO_USER
    fi
  " >> "$LOG_FILE" 2>&1 || {
    fail "Failed to create user on $worker. See $LOG_FILE"
  }

  # Verify
  actual_uid=$(docker exec "$worker" id -u "$DEMO_USER" 2>&1)
  actual_gid=$(docker exec "$worker" id -g "$DEMO_USER" 2>&1)
  if [[ "$actual_uid" == "$DEMO_UID" && "$actual_gid" == "$DEMO_GID" ]]; then
    ok "$worker: $DEMO_USER (uid=$actual_uid gid=$actual_gid)"
  else
    fail "$worker: expected uid=$DEMO_UID gid=$DEMO_GID, got uid=$actual_uid gid=$actual_gid"
  fi
done

# ---------------------------------------------------------------------------
# Apply node labels
# ---------------------------------------------------------------------------
info "Applying node labels"

# First worker is the server, rest are clients
kubectl label node "${WORKERS[0]}" role=server --overwrite >> "$LOG_FILE" 2>&1
ok "${WORKERS[0]} labeled role=server"

for i in 1 2; do
  kubectl label node "${WORKERS[$i]}" role=client --overwrite >> "$LOG_FILE" 2>&1
  ok "${WORKERS[$i]} labeled role=client"
done

# ---------------------------------------------------------------------------
# Create ConfigMap with server node IP
# ---------------------------------------------------------------------------
info "Discovering server node IP"

SERVER_NODE="${WORKERS[0]}"
SERVER_IP=$(kubectl get node "$SERVER_NODE" -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}')

if [[ -z "$SERVER_IP" ]]; then
  fail "Could not determine internal IP of server node $SERVER_NODE"
fi

ok "Server node IP: $SERVER_IP"

kubectl delete configmap server-config --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap server-config --from-literal=SERVER_IP="$SERVER_IP" >> "$LOG_FILE" 2>&1
ok "ConfigMap server-config created (SERVER_IP=$SERVER_IP)"

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
# Ensure socat is available on worker nodes
# ---------------------------------------------------------------------------
info "Ensuring socat is available on worker nodes"

for worker in "${WORKERS[@]}"; do
  if docker exec "$worker" which socat >/dev/null 2>&1; then
    ok "$worker already has socat"
  else
    echo "  Installing socat on $worker..."
    docker exec "$worker" sh -c "apt-get update -qq && apt-get install -y -qq socat" >> "$LOG_FILE" 2>&1 || {
      fail "Failed to install socat on $worker. See $LOG_FILE"
    }
    ok "$worker socat installed"
  fi
done

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "${C}========================================${R}"
echo "${B}Cluster ready: $CLUSTER_NAME${R}"
echo "${C}========================================${R}"
echo ""

if ! $BUILD_MODE; then
  echo "${B}Reaper release: $RELEASE_VERSION${R}"
else
  echo "${B}Reaper: built from source${R}"
fi

echo "${B}Nodes:${R}"
kubectl get nodes -o custom-columns=\
'NAME:.metadata.name,STATUS:.status.conditions[-1].type,ROLE:.metadata.labels.role,IP:.status.addresses[0].address' \
  --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done

echo ""
echo "${B}Shared user (all worker nodes):${R}"
echo "  User:  $DEMO_USER"
echo "  UID:   $DEMO_UID"
echo "  GID:   $DEMO_GID"

echo ""
echo "${B}Server:${R}"
echo "  Node: $SERVER_NODE"
echo "  IP:   $SERVER_IP"
echo "  Port: 9090"

echo ""
echo "${B}ConfigMap:${R}"
echo "  $(kubectl get configmap server-config -o jsonpath='{.data}' 2>/dev/null)"

echo ""
echo "${C}────────────────────────────────────────${R}"
echo ""
echo "Run the demo:"
echo ""
echo "  ${B}# 1. Start the server (runs as $DEMO_USER)${R}"
echo "  kubectl apply -f examples/client-server-runas/server-daemonset.yaml"
echo ""
echo "  ${B}# 2. Start the clients (run as $DEMO_USER)${R}"
echo "  kubectl apply -f examples/client-server-runas/client-daemonset.yaml"
echo ""
echo "  ${B}# 3. Watch the clients — note the uid/gid in the output${R}"
echo "  kubectl logs -l app=demo-client-runas --all-containers --prefix -f"
echo ""
echo "  ${B}# 4. Check server logs${R}"
echo "  kubectl logs -l app=demo-server-runas -f"
echo ""
echo "  ${B}# Clean up${R}"
echo "  kubectl delete -f examples/client-server-runas/"
echo "  ./examples/client-server-runas/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
