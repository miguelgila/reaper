#!/usr/bin/env bash
# setup-playground.sh — Create a Reaper-enabled Kind cluster for manual testing.
#
# Usage:
#   ./scripts/setup-playground.sh                          # Create 3-node playground
#   ./scripts/setup-playground.sh --cleanup                # Delete playground cluster
#   ./scripts/setup-playground.sh --cluster-name my-test   # Custom cluster name
#   ./scripts/setup-playground.sh --kind-config <path>     # Custom Kind config
#   ./scripts/setup-playground.sh --skip-build             # Skip binary compilation
#
# Prerequisites:
#   - Docker running
#   - kind (https://kind.sigs.k8s.io/)
#   - kubectl
#   - ansible-playbook (pip install ansible)
#   - Run from the repository root

set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
CLUSTER_NAME="reaper-playground"
KIND_CONFIG=""          # empty = generate default 3-node config
SKIP_BUILD=false
QUIET=false
CLEANUP=false

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_FILE="/tmp/reaper-playground-setup.log"

# ---------------------------------------------------------------------------
# Colors (respects NO_COLOR)
# ---------------------------------------------------------------------------
setup_colors() {
  if [[ -n "${NO_COLOR:-}" ]]; then
    B="" G="" Y="" C="" D="" R=""
  elif [[ -t 1 ]] || [[ -n "${CI:-}" ]]; then
    B=$'\033[1m'       # bold
    G=$'\033[1;32m'    # green
    Y=$'\033[1;33m'    # yellow
    C=$'\033[1;36m'    # cyan
    D=$'\033[0;37m'    # dim
    R=$'\033[0m'       # reset
  else
    B="" G="" Y="" C="" D="" R=""
  fi
}
setup_colors

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
info()  { echo "${C}==> ${R}${B}$*${R}"; }
ok()    { echo " ${G}OK${R}  $*"; }
warn()  { echo " ${Y}!!${R}  $*"; }
fail()  { echo " ${Y}ERR${R} $*" >&2; exit 1; }

# In quiet mode, redirect info/ok to log file only
if_log() {
  if $QUIET; then
    cat >> "$LOG_FILE"
  else
    cat
  fi
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case $1 in
    --cleanup)
      CLEANUP=true
      shift
      ;;
    --cluster-name)
      CLUSTER_NAME="${2:-}"
      [[ -z "$CLUSTER_NAME" ]] && fail "--cluster-name requires a value"
      shift 2
      ;;
    --kind-config)
      KIND_CONFIG="${2:-}"
      [[ -z "$KIND_CONFIG" ]] && fail "--kind-config requires a path"
      [[ ! -f "$KIND_CONFIG" ]] && fail "Kind config not found: $KIND_CONFIG"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=true
      shift
      ;;
    --quiet)
      QUIET=true
      shift
      ;;
    -h|--help)
      echo "Usage: $0 [OPTIONS]"
      echo ""
      echo "Create a Reaper-enabled Kind cluster for testing."
      echo ""
      echo "Options:"
      echo "  --cleanup               Delete the playground cluster"
      echo "  --cluster-name <name>   Cluster name (default: reaper-playground)"
      echo "  --kind-config <path>    Custom Kind config file (default: 3-node cluster)"
      echo "  --skip-build            Skip binary cross-compilation"
      echo "  --quiet                 Suppress output (for scripted use)"
      echo "  -h, --help              Show this help"
      echo ""
      echo "Prerequisites: Docker, kind, kubectl, ansible-playbook"
      echo ""
      echo "Environment:"
      echo "  CI                      Set in CI; uses target-specific binary dir"
      echo "  REAPER_BINARY_DIR       Override binary directory for Ansible installer"
      exit 0
      ;;
    *)
      fail "Unknown option: $1 (use -h for help)"
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Cleanup mode
# ---------------------------------------------------------------------------
if $CLEANUP; then
  info "Deleting Kind cluster '$CLUSTER_NAME'..." | if_log
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && ok "Cluster deleted." | if_log || warn "Cluster not found." | if_log
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------
info "Preflight checks" | if_log

command -v docker >/dev/null 2>&1         || fail "docker not found. Install Docker first."
docker info >/dev/null 2>&1               || fail "Docker daemon not running."
command -v kind >/dev/null 2>&1           || fail "kind not found. Install from https://kind.sigs.k8s.io/"
command -v kubectl >/dev/null 2>&1        || fail "kubectl not found. Install from https://kubernetes.io/docs/tasks/tools/"
command -v ansible-playbook >/dev/null 2>&1 || fail "ansible-playbook not found. Install with: pip install ansible"

if [[ ! -f "$REPO_ROOT/scripts/install-reaper.sh" ]]; then
  fail "Run this script from the repository root: ./scripts/setup-playground.sh"
fi

ok "All prerequisites found." | if_log

# ---------------------------------------------------------------------------
# Prepare Kind config
# ---------------------------------------------------------------------------
GENERATED_CONFIG=""

if [[ -z "$KIND_CONFIG" ]]; then
  # Generate default 3-node config (1 control-plane + 2 workers)
  GENERATED_CONFIG=$(mktemp /tmp/reaper-playground-kind-XXXXXX.yaml)
  cat > "$GENERATED_CONFIG" <<'EOF'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
  - role: worker
  - role: worker
containerdConfigPatches:
  - |
    [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
      runtime_type = "io.containerd.reaper.v2"
      sandbox_mode = "podsandbox"
EOF
  KIND_CONFIG="$GENERATED_CONFIG"
  info "Using default 3-node Kind config" | if_log
else
  info "Using provided Kind config: $KIND_CONFIG" | if_log
fi

# Clean up generated config on exit
cleanup_temp() {
  [[ -n "$GENERATED_CONFIG" ]] && rm -f "$GENERATED_CONFIG"
}
trap cleanup_temp EXIT

# ---------------------------------------------------------------------------
# Create or reuse Kind cluster
# ---------------------------------------------------------------------------
info "Creating Kind cluster '$CLUSTER_NAME'" | if_log

if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
  warn "Cluster '$CLUSTER_NAME' already exists, reusing." | if_log
else
  if $QUIET; then
    kind create cluster --name "$CLUSTER_NAME" --config "$KIND_CONFIG" >> "$LOG_FILE" 2>&1
  else
    kind create cluster --name "$CLUSTER_NAME" --config "$KIND_CONFIG" 2>&1 | tee -a "$LOG_FILE"
  fi
  ok "Cluster created." | if_log
fi

# ---------------------------------------------------------------------------
# Build static musl binaries
# ---------------------------------------------------------------------------
if ! $SKIP_BUILD; then
  info "Building Reaper binaries for Kind nodes" | if_log

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

  {
    echo "  Architecture: $NODE_ARCH ($TARGET_TRIPLE)"
  } | if_log

  if $QUIET; then
    docker run --rm \
      -v "$(pwd)":/work \
      -w /work \
      "$MUSL_IMAGE" \
      cargo build --release \
        --bin containerd-shim-reaper-v2 \
        --bin reaper-runtime \
        --target "$TARGET_TRIPLE" \
      >> "$LOG_FILE" 2>&1 || fail "Build failed. See $LOG_FILE for details."
  else
    docker run --rm \
      -v "$(pwd)":/work \
      -w /work \
      "$MUSL_IMAGE" \
      cargo build --release \
        --bin containerd-shim-reaper-v2 \
        --bin reaper-runtime \
        --target "$TARGET_TRIPLE" \
      2>&1 | tee -a "$LOG_FILE" || fail "Build failed. See $LOG_FILE for details."
  fi

  # Set binary directory for Ansible installer
  if [[ -n "${CI:-}" ]]; then
    export REAPER_BINARY_DIR="$(pwd)/target/$TARGET_TRIPLE/release"
    info "Using binaries from $REAPER_BINARY_DIR (CI mode)" | if_log
  else
    mkdir -p target/release
    cp "target/$TARGET_TRIPLE/release/containerd-shim-reaper-v2" target/release/
    cp "target/$TARGET_TRIPLE/release/reaper-runtime" target/release/
  fi

  ok "Binaries built." | if_log
else
  info "Skipping build (--skip-build)" | if_log
  cd "$REPO_ROOT"
fi

# ---------------------------------------------------------------------------
# Install Reaper on all nodes via Ansible
# ---------------------------------------------------------------------------
info "Installing Reaper runtime on all nodes" | if_log

if $QUIET; then
  ./scripts/install-reaper.sh --kind "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1 || {
    fail "Ansible install failed. See $LOG_FILE for details."
  }
else
  ./scripts/install-reaper.sh --kind "$CLUSTER_NAME" 2>&1 | tee -a "$LOG_FILE" || {
    fail "Ansible install failed. See $LOG_FILE for details."
  }
fi

ok "Reaper installed on all nodes." | if_log

# ---------------------------------------------------------------------------
# Wait for readiness
# ---------------------------------------------------------------------------
info "Waiting for nodes to be Ready" | if_log

kubectl wait --for=condition=Ready node --all --timeout=120s >> "$LOG_FILE" 2>&1 || {
  fail "Nodes did not become Ready. See $LOG_FILE"
}

ok "All nodes Ready." | if_log

# Verify RuntimeClass
info "Verifying RuntimeClass" | if_log

for i in $(seq 1 15); do
  if kubectl get runtimeclass reaper-v2 &>/dev/null; then
    ok "RuntimeClass reaper-v2 available." | if_log
    break
  fi
  sleep 1
done

kubectl get runtimeclass reaper-v2 &>/dev/null || fail "RuntimeClass reaper-v2 not found"

# ---------------------------------------------------------------------------
# Smoke test
# ---------------------------------------------------------------------------
info "Running smoke test" | if_log

kubectl apply -f - <<'EOF' >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-smoke-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/echo", "Hello from Reaper playground!"]
EOF

# Wait for the smoke test pod to complete
for i in $(seq 1 30); do
  phase=$(kubectl get pod reaper-smoke-test -o jsonpath='{.status.phase}' 2>/dev/null || echo "Pending")
  if [[ "$phase" == "Succeeded" ]]; then
    break
  elif [[ "$phase" == "Failed" ]]; then
    warn "Smoke test pod failed" | if_log
    kubectl logs reaper-smoke-test 2>/dev/null | if_log
    kubectl delete pod reaper-smoke-test --ignore-not-found >> "$LOG_FILE" 2>&1
    fail "Smoke test failed. Check $LOG_FILE"
  fi
  sleep 1
done

SMOKE_OUTPUT=$(kubectl logs reaper-smoke-test 2>/dev/null || echo "(no output)")
kubectl delete pod reaper-smoke-test --ignore-not-found >> "$LOG_FILE" 2>&1

if [[ "$SMOKE_OUTPUT" == *"Hello from Reaper playground!"* ]]; then
  ok "Smoke test passed: $SMOKE_OUTPUT" | if_log
else
  warn "Smoke test output unexpected: $SMOKE_OUTPUT" | if_log
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
if ! $QUIET; then
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
  echo "${B}RuntimeClass:${R}"
  echo "  $(kubectl get runtimeclass reaper-v2 -o custom-columns='NAME:.metadata.name,HANDLER:.handler' --no-headers 2>/dev/null)"

  echo ""
  echo "${C}────────────────────────────────────────${R}"
  echo ""
  echo "Try it out:"
  echo ""
  echo "  ${B}# Run a command on the host${R}"
  echo "  kubectl run hello --rm -it --image=busybox --restart=Never \\"
  echo "    --overrides='{\"spec\":{\"runtimeClassName\":\"reaper-v2\"}}' \\"
  echo "    -- /bin/sh -c 'echo Hello from \$(hostname) && uname -a'"
  echo ""
  echo "  ${B}# Interactive shell${R}"
  echo "  kubectl run debug --rm -it --image=busybox --restart=Never \\"
  echo "    --overrides='{\"spec\":{\"runtimeClassName\":\"reaper-v2\"}}' \\"
  echo "    -- /bin/bash"
  echo ""
  echo "  ${B}# Run the examples${R}"
  echo "  kubectl apply -f deploy/kubernetes/runtimeclass.yaml"
  echo "  kubectl logs reaper-example"
  echo ""
  echo "  ${B}# Clean up${R}"
  echo "  ./scripts/setup-playground.sh --cleanup"
  echo ""
  echo "Log file: $LOG_FILE"
fi
