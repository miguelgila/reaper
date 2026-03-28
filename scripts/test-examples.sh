#!/usr/bin/env bash
# test-examples.sh — Validate all Reaper example manifests and optionally run cluster tests.
#
# Usage:
#   ./scripts/test-examples.sh                # Full validation (YAML + cluster tests)
#   ./scripts/test-examples.sh --skip-cluster # YAML validation only
#   ./scripts/test-examples.sh --cleanup      # Delete the test cluster and exit
#
# Flags:
#   --skip-cluster   Skip Phase 2 cluster tests (YAML validation only)
#   --cleanup        Delete the test cluster and exit

set -euo pipefail

CLUSTER_NAME="reaper-examples-test"
KUBECONFIG_FILE="/tmp/reaper-examples-test-kubeconfig"

SKIP_CLUSTER=false
CLEANUP=false

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# Colors (respects NO_COLOR)
# ---------------------------------------------------------------------------
if [[ -n "${NO_COLOR:-}" ]]; then
  GREEN="" YELLOW="" RED="" CYAN="" BOLD="" RESET=""
elif [[ -t 1 ]] || [[ -n "${CI:-}" ]]; then
  GREEN=$'\033[1;32m'
  YELLOW=$'\033[1;33m'
  RED=$'\033[1;31m'
  CYAN=$'\033[1;36m'
  BOLD=$'\033[1m'
  RESET=$'\033[0m'
else
  GREEN="" YELLOW="" RED="" CYAN="" BOLD="" RESET=""
fi

pass() { echo "${GREEN}[PASS]${RESET} $*"; }
fail() { echo "${RED}[FAIL]${RESET} $*"; }
skip() { echo "${YELLOW}[SKIP]${RESET} $*"; }
info() { echo "${CYAN}==>${RESET} ${BOLD}$*${RESET}"; }

# ---------------------------------------------------------------------------
# Counters
# ---------------------------------------------------------------------------
P1_PASS=0
P1_FAIL=0
P2_PASS=0
P2_FAIL=0
P2_SKIP=0

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case $1 in
    --skip-cluster)
      SKIP_CLUSTER=true
      shift
      ;;
    --cleanup)
      CLEANUP=true
      shift
      ;;
    -h|--help)
      echo "Usage: $0 [--skip-cluster] [--cleanup]"
      echo ""
      echo "  --skip-cluster   Run YAML validation only (no kind cluster needed)"
      echo "  --cleanup        Delete the test cluster and exit"
      exit 0
      ;;
    *)
      echo "Unknown option: $1 (use -h for help)" >&2
      exit 1
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Cleanup mode
# ---------------------------------------------------------------------------
if $CLEANUP; then
  info "Deleting Kind cluster '$CLUSTER_NAME'..."
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && echo "Cluster deleted." || echo "Cluster not found."
  rm -f "$KUBECONFIG_FILE"
  exit 0
fi

# ---------------------------------------------------------------------------
# Phase 1: YAML Validation (no cluster needed)
# ---------------------------------------------------------------------------
info "Phase 1: YAML Validation"

# Validate that python3 is available for YAML parsing
HAVE_PYTHON=false
if command -v python3 >/dev/null 2>&1; then
  HAVE_PYTHON=true
fi

validate_yaml() {
  local yaml_file="$1"

  # Check YAML parses cleanly
  if $HAVE_PYTHON; then
    if ! python3 -c "
import sys, yaml
try:
    docs = list(yaml.safe_load_all(open(sys.argv[1])))
    for doc in docs:
        if doc is None:
            continue
        if not isinstance(doc, dict):
            print('Not a mapping', file=sys.stderr)
            sys.exit(1)
        missing = [f for f in ('apiVersion', 'kind', 'metadata') if f not in doc]
        if missing:
            print(f'Missing fields: {missing}', file=sys.stderr)
            sys.exit(1)
    sys.exit(0)
except Exception as e:
    print(e, file=sys.stderr)
    sys.exit(1)
" "$yaml_file" 2>/dev/null; then
      return 1
    fi
  else
    # Fallback: check that file is non-empty and contains apiVersion/kind/metadata
    if ! grep -q "apiVersion:" "$yaml_file" 2>/dev/null; then
      return 1
    fi
    if ! grep -q "kind:" "$yaml_file" 2>/dev/null; then
      return 1
    fi
    if ! grep -q "metadata:" "$yaml_file" 2>/dev/null; then
      return 1
    fi
  fi
  return 0
}

EXAMPLES_DIR="$REPO_ROOT/examples"
for dir in "$EXAMPLES_DIR"/*/; do
  dir_name="$(basename "$dir")"

  # Find all YAML files in this directory (non-recursive, examples are flat)
  while IFS= read -r -d '' yaml_file; do
    rel_path="examples/${dir_name}/$(basename "$yaml_file")"
    if validate_yaml "$yaml_file"; then
      pass "YAML valid: $rel_path"
      (( P1_PASS++ )) || true
    else
      fail "YAML invalid: $rel_path"
      (( P1_FAIL++ )) || true
    fi
  done < <(find "$dir" -maxdepth 1 -name "*.yaml" -print0 2>/dev/null | sort -z)
done

# ---------------------------------------------------------------------------
# Phase 2: Cluster Tests
# ---------------------------------------------------------------------------
info "Phase 2: Cluster Tests"

if $SKIP_CLUSTER; then
  skip "Phase 2 skipped (--skip-cluster)"
  P2_SKIP=1
else
  # Check prerequisites for cluster tests
  HAVE_KIND=false
  HAVE_KUBECTL=false
  HAVE_HELM=false
  HAVE_DOCKER=false

  command -v kind    >/dev/null 2>&1 && HAVE_KIND=true
  command -v kubectl >/dev/null 2>&1 && HAVE_KUBECTL=true
  command -v helm    >/dev/null 2>&1 && HAVE_HELM=true
  command -v docker  >/dev/null 2>&1 && HAVE_DOCKER=true

  if ! $HAVE_KIND || ! $HAVE_KUBECTL || ! $HAVE_HELM || ! $HAVE_DOCKER; then
    skip "Phase 2 skipped (missing prerequisites: kind=$HAVE_KIND kubectl=$HAVE_KUBECTL helm=$HAVE_HELM docker=$HAVE_DOCKER)"
    P2_SKIP=1
  elif ! docker info >/dev/null 2>&1; then
    skip "Phase 2 skipped (Docker daemon not running)"
    P2_SKIP=1
  else
    # Set up or reuse cluster
    CLUSTER_EXISTS=false
    if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
      CLUSTER_EXISTS=true
      info "Reusing existing cluster '$CLUSTER_NAME'"
    fi

    if ! $CLUSTER_EXISTS; then
      info "Setting up playground cluster '$CLUSTER_NAME' (this may take a few minutes)..."
      "$SCRIPT_DIR/setup-playground.sh" --cluster-name "$CLUSTER_NAME" --quiet || {
        skip "Phase 2 skipped (cluster setup failed)"
        P2_SKIP=1
        SKIP_CLUSTER=true
      }
    fi

    if ! $SKIP_CLUSTER; then
      # Export KUBECONFIG
      kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG_FILE"
      export KUBECONFIG="$KUBECONFIG_FILE"

      # -----------------------------------------------------------------------
      # Helper: wait for ReaperPod to reach Succeeded phase
      # -----------------------------------------------------------------------
      wait_reaperpod_succeeded() {
        local name="$1"
        local timeout_secs="${2:-60}"
        local elapsed=0
        while [[ $elapsed -lt $timeout_secs ]]; do
          local phase
          phase=$(kubectl get reaperpod "$name" -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
          if [[ "$phase" == "Succeeded" ]]; then
            return 0
          elif [[ "$phase" == "Failed" ]]; then
            echo "  ReaperPod '$name' phase: Failed" >&2
            return 1
          fi
          sleep 2
          (( elapsed += 2 )) || true
        done
        echo "  Timed out waiting for ReaperPod '$name' (last phase: $phase)" >&2
        return 1
      }

      # -----------------------------------------------------------------------
      # Test example 09: ReaperPod CRDs
      # -----------------------------------------------------------------------
      info "Testing example 09-reaperpod..."

      test_reaperpod() {
        local yaml_file="$1"
        local name="$2"
        local timeout_secs="${3:-60}"
        local label="examples/09-reaperpod/$(basename "$yaml_file")"

        kubectl apply -f "$yaml_file" >/dev/null 2>&1 || {
          fail "Apply failed: $label"
          (( P2_FAIL++ )) || true
          return
        }

        if wait_reaperpod_succeeded "$name" "$timeout_secs"; then
          pass "ReaperPod Succeeded: $label"
          (( P2_PASS++ )) || true
        else
          fail "ReaperPod did not Succeed: $label"
          (( P2_FAIL++ )) || true
        fi

        kubectl delete reaperpod "$name" --ignore-not-found >/dev/null 2>&1 || true
      }

      REAPERPOD_DIR="$EXAMPLES_DIR/09-reaperpod"

      # Clean stale ReaperPods from previous runs
      kubectl delete reaperpod --all --ignore-not-found >/dev/null 2>&1 || true
      kubectl delete pod -l reaper.giar.dev/owner --ignore-not-found >/dev/null 2>&1 || true

      # Set up prerequisites for example 09
      WORKER_NODE=$(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep -v control-plane | head -1)
      kubectl label node "$WORKER_NODE" workload-type=compute --overwrite >/dev/null 2>&1 || true
      kubectl create configmap app-config --from-literal=greeting="Hello from ConfigMap" --dry-run=client -o yaml | kubectl apply -f - >/dev/null 2>&1

      # Names must match metadata.name in each YAML file
      test_reaperpod "$REAPERPOD_DIR/simple-task.yaml"        "hello-world"    60
      test_reaperpod "$REAPERPOD_DIR/with-node-selector.yaml" "node-info"      60
      test_reaperpod "$REAPERPOD_DIR/with-volumes.yaml"       "config-reader"  60

      # Clean up prerequisites
      kubectl label node "$WORKER_NODE" workload-type- >/dev/null 2>&1 || true
      kubectl delete configmap app-config --ignore-not-found >/dev/null 2>&1 || true

      # -----------------------------------------------------------------------
      # Test example 01: DaemonSet scheduling
      # -----------------------------------------------------------------------
      info "Testing example 01-scheduling (DaemonSet apply + pod scheduling)..."

      DAEMONSET_YAML="$EXAMPLES_DIR/01-scheduling/all-nodes-daemonset.yaml"
      if kubectl apply -f "$DAEMONSET_YAML" >/dev/null 2>&1; then
        # Wait up to 30s for at least one pod to be Running
        elapsed=0
        DS_OK=false
        while [[ $elapsed -lt 30 ]]; do
          ready=$(kubectl get daemonset node-monitor -o jsonpath='{.status.numberReady}' 2>/dev/null || echo "0")
          if [[ "${ready:-0}" -gt 0 ]]; then
            DS_OK=true
            break
          fi
          sleep 2
          (( elapsed += 2 )) || true
        done
        if $DS_OK; then
          pass "DaemonSet pods scheduled: examples/01-scheduling/all-nodes-daemonset.yaml"
          (( P2_PASS++ )) || true
        else
          fail "DaemonSet pods did not schedule: examples/01-scheduling/all-nodes-daemonset.yaml"
          (( P2_FAIL++ )) || true
        fi
        kubectl delete daemonset node-monitor --ignore-not-found >/dev/null 2>&1 || true
      else
        fail "Apply failed: examples/01-scheduling/all-nodes-daemonset.yaml"
        (( P2_FAIL++ )) || true
      fi

      # -----------------------------------------------------------------------
      # Test example 04: Volumes
      # -----------------------------------------------------------------------
      info "Testing example 04-volumes (apply manifests, verify pods run)..."

      VOLUMES_DIR="$EXAMPLES_DIR/04-volumes"
      VOLUMES_OK=true

      # Set up prerequisites for example 04
      NODE_ID=$(docker ps --filter "name=reaper-examples-test-control-plane" --format '{{.ID}}')
      DEMO_NODE=$(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep -v control-plane | head -1)
      kubectl label node "$DEMO_NODE" role=demo --overwrite >/dev/null 2>&1 || true
      kubectl create configmap nginx-config --from-literal=demo.conf='server { listen 8080; location / { return 200 "hello"; } }' --dry-run=client -o yaml | kubectl apply -f - >/dev/null 2>&1
      kubectl create secret generic app-credentials --from-literal=username=demo-user --from-literal=password=s3cret --dry-run=client -o yaml | kubectl apply -f - >/dev/null 2>&1
      DEMO_NODE_ID=$(docker ps --filter "name=${DEMO_NODE}" --format '{{.ID}}')
      if [[ -n "$DEMO_NODE_ID" ]]; then
        docker exec "$DEMO_NODE_ID" mkdir -p /opt/reaper-demo/html >/dev/null 2>&1 || true
        docker exec "$DEMO_NODE_ID" sh -c 'echo "<h1>Hello</h1>" > /opt/reaper-demo/html/index.html' >/dev/null 2>&1 || true
      fi

      for yaml_file in "$VOLUMES_DIR"/*.yaml; do
        [[ -f "$yaml_file" ]] || continue
        if ! kubectl apply -f "$yaml_file" >/dev/null 2>&1; then
          fail "Apply failed: examples/04-volumes/$(basename "$yaml_file")"
          VOLUMES_OK=false
          (( P2_FAIL++ )) || true
        fi
      done

      if $VOLUMES_OK; then
        # Wait up to 30s for pods to reach Running or Succeeded
        elapsed=0
        VOL_OK=false
        while [[ $elapsed -lt 30 ]]; do
          running=$(kubectl get pods -l "app in (configmap-nginx,emptydir-worker,hostpath-reader,secret-reader)" \
            --field-selector='status.phase=Running' --no-headers 2>/dev/null | wc -l | tr -d ' ')
          succeeded=$(kubectl get pods -l "app in (configmap-nginx,emptydir-worker,hostpath-reader,secret-reader)" \
            --field-selector='status.phase=Succeeded' --no-headers 2>/dev/null | wc -l | tr -d ' ')
          total=$(( running + succeeded ))
          if [[ "$total" -gt 0 ]]; then
            VOL_OK=true
            break
          fi
          sleep 2
          (( elapsed += 2 )) || true
        done

        if $VOL_OK; then
          pass "Volume pods running: examples/04-volumes/"
          (( P2_PASS++ )) || true
        else
          fail "Volume pods did not start: examples/04-volumes/"
          (( P2_FAIL++ )) || true
        fi

        # Clean up
        for yaml_file in "$VOLUMES_DIR"/*.yaml; do
          [[ -f "$yaml_file" ]] || continue
          kubectl delete -f "$yaml_file" --ignore-not-found >/dev/null 2>&1 || true
        done
      fi

      # Clean up volume prerequisites
      kubectl label node "$DEMO_NODE" role- >/dev/null 2>&1 || true
      kubectl delete configmap nginx-config --ignore-not-found >/dev/null 2>&1 || true
      kubectl delete secret app-credentials --ignore-not-found >/dev/null 2>&1 || true
    fi
  fi
fi

# ---------------------------------------------------------------------------
# Phase 3: Summary
# ---------------------------------------------------------------------------
echo ""
echo "${BOLD}=== Reaper Example Test Results ===${RESET}"
echo "Phase 1 (YAML validation): ${GREEN}${P1_PASS} passed${RESET}, ${RED}${P1_FAIL} failed${RESET}"
echo "Phase 2 (Cluster tests):   ${GREEN}${P2_PASS} passed${RESET}, ${RED}${P2_FAIL} failed${RESET}, ${YELLOW}${P2_SKIP} skipped${RESET}"
TOTAL_PASS=$(( P1_PASS + P2_PASS ))
TOTAL_FAIL=$(( P1_FAIL + P2_FAIL ))
TOTAL_SKIP=$P2_SKIP
echo "Total: ${GREEN}${TOTAL_PASS} passed${RESET}, ${RED}${TOTAL_FAIL} failed${RESET}, ${YELLOW}${TOTAL_SKIP} skipped${RESET}"
echo ""

if [[ $TOTAL_FAIL -gt 0 ]]; then
  exit 1
fi
exit 0
