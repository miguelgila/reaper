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
      echo "  --skip-build            Skip binary cross-compilation (use existing images)"
      echo "  --quiet                 Suppress output (for scripted use)"
      echo "  -h, --help              Show this help"
      echo ""
      echo "Prerequisites: Docker, kind, kubectl, helm"
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
command -v helm >/dev/null 2>&1            || fail "helm not found. Install from https://helm.sh/docs/intro/install/"

if [[ ! -f "$REPO_ROOT/deploy/helm/reaper/Chart.yaml" ]]; then
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
# Build and load images
# ---------------------------------------------------------------------------
cd "$REPO_ROOT"

# Extract version from Cargo.toml so images are tagged with the version under test
# (matches what the Helm chart defaults to via appVersion).
REAPER_VERSION=$(sed -n '/^\[package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)
info "Reaper version: $REAPER_VERSION" | if_log

# Build reaper-node image (contains shim + runtime + install script)
info "Building reaper-node image" | if_log
local_build_args=(--cluster-name "$CLUSTER_NAME")
if $SKIP_BUILD; then
  local_build_args+=(--skip-build)
fi
if $QUIET; then
  local_build_args+=(--quiet)
fi
"$SCRIPT_DIR/build-node-image.sh" "${local_build_args[@]}" \
  --image "ghcr.io/miguelgila/reaper-node:${REAPER_VERSION}" \
  2>&1 | tee -a "$LOG_FILE" || {
  fail "reaper-node image build failed. See $LOG_FILE"
}
ok "reaper-node image loaded into Kind." | if_log

# Build reaper-controller image
info "Building reaper-controller image" | if_log
"$SCRIPT_DIR/build-controller-image.sh" "${local_build_args[@]}" \
  --image "ghcr.io/miguelgila/reaper-controller:${REAPER_VERSION}" \
  2>&1 | tee -a "$LOG_FILE" || {
  fail "reaper-controller image build failed. See $LOG_FILE"
}
ok "reaper-controller image loaded into Kind." | if_log

# Build reaper-agent image
info "Building reaper-agent image" | if_log
"$SCRIPT_DIR/build-agent-image.sh" "${local_build_args[@]}" \
  --image "ghcr.io/miguelgila/reaper-agent:${REAPER_VERSION}" \
  2>&1 | tee -a "$LOG_FILE" || {
  fail "reaper-agent image build failed. See $LOG_FILE"
}
ok "reaper-agent image loaded into Kind." | if_log

# ---------------------------------------------------------------------------
# Install Reaper via Helm
# ---------------------------------------------------------------------------
info "Installing Reaper via Helm" | if_log

# Pre-create namespace idempotently.  helm upgrade --install --create-namespace
# fails with "namespace already exists" when retrying after a partial install,
# so we create it up front and omit --create-namespace.
kubectl create namespace reaper-system --dry-run=client -o yaml | kubectl apply -f - >> "$LOG_FILE" 2>&1

# Retry Helm install up to 3 times. On freshly-created Kind clusters the API
# server may not be fully stabilized when we reach this point, causing the
# first attempt to fail (CRD establishment race, transient API errors, etc.).
HELM_INSTALLED=false
for attempt in 1 2 3; do
  if $QUIET; then
    if helm upgrade --install reaper deploy/helm/reaper/ \
      --namespace reaper-system \
      --set node.image.pullPolicy=IfNotPresent \
      --set controller.image.pullPolicy=IfNotPresent \
      --set agent.image.pullPolicy=IfNotPresent \
      --wait --timeout 120s \
      >> "$LOG_FILE" 2>&1; then
      HELM_INSTALLED=true
      break
    fi
  else
    if helm upgrade --install reaper deploy/helm/reaper/ \
      --namespace reaper-system \
      --set node.image.pullPolicy=IfNotPresent \
      --set controller.image.pullPolicy=IfNotPresent \
      --set agent.image.pullPolicy=IfNotPresent \
      --wait --timeout 120s \
      2>&1 | tee -a "$LOG_FILE"; then
      HELM_INSTALLED=true
      break
    fi
  fi
  warn "Helm install attempt $attempt failed, retrying in 5s..." | if_log
  sleep 5
done

if ! $HELM_INSTALLED; then
  echo "--- Last 30 lines of $LOG_FILE ---" >&2
  tail -30 "$LOG_FILE" >&2
  fail "Helm install failed after 3 attempts. See $LOG_FILE"
fi

ok "Reaper installed via Helm." | if_log

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

# Wait for reaper-node DaemonSet to be fully rolled out (init container copies
# shim + runtime binaries to host).  Helm --wait considers a DaemonSet ready
# when pods are Running, but containerd may not have picked up the new shim yet.
info "Waiting for reaper-node DaemonSet rollout" | if_log

kubectl rollout status daemonset/reaper-node -n reaper-system --timeout=120s >> "$LOG_FILE" 2>&1 || {
  fail "reaper-node DaemonSet did not become ready. Binaries may not be installed. See $LOG_FILE"
}

ok "reaper-node DaemonSet rolled out." | if_log

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
