#!/usr/bin/env bash
# setup-playground.sh — Create a Reaper-enabled Kind cluster for manual testing.
#
# Usage:
#   ./scripts/setup-playground.sh                          # Build from source + create cluster
#   ./scripts/setup-playground.sh --release                # Use latest GitHub release (no build)
#   ./scripts/setup-playground.sh --release v0.2.4         # Use specific GitHub release
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
#   - curl (for --release mode)
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
RELEASE_VERSION=""      # empty = build from source; "latest" or "vX.Y.Z" = download

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
    --release)
      # Accept optional version argument; default to "latest"
      if [[ -n "${2:-}" ]] && [[ "$2" != --* ]]; then
        RELEASE_VERSION="$2"
        shift 2
      else
        RELEASE_VERSION="latest"
        shift
      fi
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
      echo "  --release [version]     Use pre-built binaries from GitHub Releases (default: latest)"
      echo "  --cleanup               Delete the playground cluster"
      echo "  --cluster-name <name>   Cluster name (default: reaper-playground)"
      echo "  --kind-config <path>    Custom Kind config file (default: 3-node cluster)"
      echo "  --skip-build            Skip binary cross-compilation (when building from source)"
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
      pod_annotations = ["reaper.runtime/*"]
EOF
  KIND_CONFIG="$GENERATED_CONFIG"
  info "Using default 3-node Kind config" | if_log
else
  info "Using provided Kind config: $KIND_CONFIG" | if_log
fi

# Clean up generated config on exit
cleanup_temp() {
  if [[ -n "$GENERATED_CONFIG" ]]; then
    rm -f "$GENERATED_CONFIG"
  fi
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

# Export a dedicated KUBECONFIG so all kubectl commands in this session
# (and child processes) target the right cluster, even if the user has
# other Kind clusters or contexts active.
KUBECONFIG_FILE="/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
info "Using KUBECONFIG=$KUBECONFIG_FILE" | if_log

# ---------------------------------------------------------------------------
# Resolve "latest" release version
# ---------------------------------------------------------------------------
# shellcheck source=lib/release-utils.sh
source "$SCRIPT_DIR/lib/release-utils.sh"

if [[ -n "$RELEASE_VERSION" ]]; then
  if [[ "$RELEASE_VERSION" == "latest" ]]; then
    info "Resolving latest release..." | if_log
    RELEASE_VERSION=$(resolve_latest_release) || \
      fail "Could not determine latest release. Check https://github.com/${GITHUB_REPO}/releases or specify a version: --release v0.2.4"
    ok "Latest release: $RELEASE_VERSION" | if_log
  fi
fi

# ---------------------------------------------------------------------------
# Obtain binaries (release download OR build from source)
# ---------------------------------------------------------------------------
cd "$REPO_ROOT"

if [[ -n "$RELEASE_VERSION" ]]; then
  # --release mode: delegate to install-reaper.sh which handles download
  info "Using pre-built release $RELEASE_VERSION (skipping build)" | if_log
  INSTALL_RELEASE_ARGS="--release $RELEASE_VERSION"

elif ! $SKIP_BUILD; then
  info "Building Reaper binaries for Kind nodes" | if_log

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
  INSTALL_RELEASE_ARGS=""
else
  info "Skipping build (--skip-build)" | if_log
  if [[ -n "${CI:-}" ]]; then
    # CI: binaries were downloaded as artifacts to the musl target dir
    for triple in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
      if [[ -f "target/$triple/release/containerd-shim-reaper-v2" ]]; then
        export REAPER_BINARY_DIR="$(pwd)/target/$triple/release"
        info "Using pre-built binaries from $REAPER_BINARY_DIR" | if_log
        break
      fi
    done
  fi
  INSTALL_RELEASE_ARGS=""
fi

# ---------------------------------------------------------------------------
# Install Reaper on all nodes via Ansible
# ---------------------------------------------------------------------------
info "Installing Reaper runtime on all nodes" | if_log

# shellcheck disable=SC2086
if $QUIET; then
  ./scripts/install-reaper.sh --kind "$CLUSTER_NAME" $INSTALL_RELEASE_ARGS >> "$LOG_FILE" 2>&1 || {
    fail "Ansible install failed. See $LOG_FILE for details."
  }
else
  ./scripts/install-reaper.sh --kind "$CLUSTER_NAME" $INSTALL_RELEASE_ARGS 2>&1 | tee -a "$LOG_FILE" || {
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
# Optional: ReaperPod CRD + controller
# ---------------------------------------------------------------------------
info "Installing ReaperPod CRD and controller" | if_log

# Install CRD
kubectl apply -f deploy/kubernetes/crds/reaperpods.reaper.io.yaml >> "$LOG_FILE" 2>&1

# Wait for CRD to be established
for i in $(seq 1 15); do
  established=$(kubectl get crd reaperpods.reaper.io -o jsonpath='{.status.conditions[?(@.type=="Established")].status}' 2>/dev/null || true)
  if [[ "$established" == "True" ]]; then
    break
  fi
  sleep 1
done
[[ "$established" == "True" ]] || fail "CRD reaperpods.reaper.io not established after 15s"
ok "ReaperPod CRD installed." | if_log

# Create namespace and deploy controller
kubectl create namespace reaper-system --dry-run=client -o yaml | kubectl apply -f - >> "$LOG_FILE" 2>&1

# Build and load controller image
if [[ -x "$REPO_ROOT/scripts/build-controller-image.sh" ]]; then
  if $QUIET; then
    "$REPO_ROOT/scripts/build-controller-image.sh" --cluster-name "$CLUSTER_NAME" --quiet >> "$LOG_FILE" 2>&1 || \
      fail "Controller image build failed. See $LOG_FILE"
  else
    "$REPO_ROOT/scripts/build-controller-image.sh" --cluster-name "$CLUSTER_NAME" 2>&1 | tee -a "$LOG_FILE" || \
      fail "Controller image build failed. See $LOG_FILE"
  fi
else
  warn "build-controller-image.sh not found, assuming image is pre-loaded" | if_log
fi

kubectl apply -f deploy/kubernetes/reaper-controller.yaml >> "$LOG_FILE" 2>&1

# Wait for controller pod to be ready
for i in $(seq 1 60); do
  ready=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-controller \
    -o jsonpath='{.items[0].status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || true)
  if [[ "$ready" == "True" ]]; then
    break
  fi
  sleep 2
done
[[ "$ready" == "True" ]] || fail "Controller pod not ready after 120s. See $LOG_FILE"
ok "reaper-controller deployed and ready." | if_log

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

  CTX="kind-${CLUSTER_NAME}"

  echo ""
  echo "${C}────────────────────────────────────────${R}"
  echo ""
  echo "Try it out:"
  echo ""
  echo "  ${B}# Run a command on the host${R}"
  echo "  kubectl --context=${CTX} run hello --rm -it --image=busybox --restart=Never \\"
  echo "    --overrides='{\"spec\":{\"runtimeClassName\":\"reaper-v2\"}}' \\"
  echo "    -- /bin/sh -c 'echo Hello from \$(hostname) && uname -a'"
  echo ""
  echo "  ${B}# Interactive shell${R}"
  echo "  kubectl --context=${CTX} run debug --rm -it --image=busybox --restart=Never \\"
  echo "    --overrides='{\"spec\":{\"runtimeClassName\":\"reaper-v2\"}}' \\"
  echo "    -- /bin/bash"
  echo ""
  echo "  ${B}# Create a ReaperPod (CRD)${R}"
  echo "  kubectl --context=${CTX} apply -f examples/09-reaperpod/simple-task.yaml"
  echo "  kubectl --context=${CTX} get reaperpods"
  echo ""
  echo "  ${B}# Quick inline ReaperPod${R}"
  echo "  kubectl --context=${CTX} apply -f - <<'YAML'"
  echo "apiVersion: reaper.io/v1alpha1"
  echo "kind: ReaperPod"
  echo "metadata:"
  echo "  name: quick-test"
  echo "spec:"
  echo "  command: [\"/bin/sh\", \"-c\", \"echo Hello from \\\$(hostname) at \\\$(date)\"]"
  echo "YAML"
  echo ""

  echo "  ${B}# See the examples${R}"
  echo "  ls examples/"
  echo ""
  echo "  ${B}# Clean up${R}"
  echo "  ./scripts/setup-playground.sh --cleanup"
  echo ""
  echo "Log file: $LOG_FILE"
fi
