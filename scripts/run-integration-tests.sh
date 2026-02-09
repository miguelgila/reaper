#!/usr/bin/env bash
# run-integration-tests.sh — Structured integration test harness for Reaper runtime
# Replaces kind-integration.sh with proper test reporting, logging, and CI integration.
#
# Usage:
#   ./scripts/run-integration-tests.sh                    # Full run (cargo + kind + tests)
#   ./scripts/run-integration-tests.sh --skip-cargo       # Skip Rust cargo tests
#   ./scripts/run-integration-tests.sh --no-cleanup       # Keep kind cluster after run
#   ./scripts/run-integration-tests.sh --verbose          # Print verbose output to stdout too

set -euo pipefail

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
CLUSTER_NAME="reaper-ci"
SHIM_BIN="containerd-shim-reaper-v2"
RUNTIME_BIN="reaper-runtime"
LOG_DIR="/tmp/reaper-integration-logs"
LOG_FILE="$LOG_DIR/integration-test.log"
NODE_ID=""

# Test bookkeeping
declare -a TEST_NAMES=()
declare -a TEST_RESULTS=()
declare -a TEST_DURATIONS=()
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_WARNED=0

# Flags
SKIP_CARGO=false
NO_CLEANUP=false
VERBOSE=false

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case $1 in
    --skip-cargo)  SKIP_CARGO=true; shift ;;
    --no-cleanup)  NO_CLEANUP=true; shift ;;
    --verbose)     VERBOSE=true; shift ;;
    -h|--help)
      echo "Usage: $0 [--skip-cargo] [--no-cleanup] [--verbose]"
      echo "  --skip-cargo  Skip Rust cargo tests (for quick K8s-only reruns)"
      echo "  --no-cleanup  Keep kind cluster after run"
      echo "  --verbose     Also print verbose output to stdout"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Usage: $0 [--skip-cargo] [--no-cleanup] [--verbose]" >&2
      exit 1
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Color setup (respects NO_COLOR, non-TTY; force on in CI)
# ---------------------------------------------------------------------------
setup_colors() {
  if [[ -n "${NO_COLOR:-}" ]]; then
    CLR_PASS="" CLR_FAIL="" CLR_WARN="" CLR_PHASE="" CLR_RESET="" CLR_DIM=""
  elif [[ -n "${CI:-}" ]] || [[ -t 1 ]]; then
    CLR_PASS=$'\033[1;32m'   # bold green
    CLR_FAIL=$'\033[1;31m'   # bold red
    CLR_WARN=$'\033[1;33m'   # bold yellow
    CLR_PHASE=$'\033[1;36m'  # bold cyan
    CLR_DIM=$'\033[0;37m'    # dim white
    CLR_RESET=$'\033[0m'
  else
    CLR_PASS="" CLR_FAIL="" CLR_WARN="" CLR_PHASE="" CLR_RESET="" CLR_DIM=""
  fi
}
setup_colors

# ---------------------------------------------------------------------------
# Git commit tracking
# ---------------------------------------------------------------------------
get_commit_id() {
  if command -v git >/dev/null 2>&1 && [[ -d .git ]]; then
    git rev-parse --short HEAD 2>/dev/null || echo "unknown"
  else
    echo "unknown"
  fi
}

COMMIT_ID=$(get_commit_id)
TEST_TIMESTAMP=$(date '+%Y-%m-%d %H:%M:%S')

# ---------------------------------------------------------------------------
# Logging helpers
# ---------------------------------------------------------------------------
mkdir -p "$LOG_DIR"
: > "$LOG_FILE"  # truncate

log_verbose() {
  echo "[$(date +%H:%M:%S)] $*" >> "$LOG_FILE"
  if $VERBOSE; then
    echo "${CLR_DIM}$*${CLR_RESET}"
  fi
}

log_status() {
  echo "$*"
  echo "[$(date +%H:%M:%S)] $*" >> "$LOG_FILE"
}

log_error() {
  echo "${CLR_FAIL}$*${CLR_RESET}" >&2
  echo "[$(date +%H:%M:%S)] ERROR: $*" >> "$LOG_FILE"
}

# GitHub Actions grouping
ci_group_start() {
  if [[ -n "${CI:-}" ]]; then
    echo "::group::$1"
  fi
}

ci_group_end() {
  if [[ -n "${CI:-}" ]]; then
    echo "::endgroup::"
  fi
}

ci_error() {
  if [[ -n "${CI:-}" ]]; then
    echo "::error::$1"
  fi
}

# ---------------------------------------------------------------------------
# Core helpers
# ---------------------------------------------------------------------------
retry_kubectl() {
  local max_retries=5
  local retry_count=0
  local backoff=1
  local output
  local exit_code

  while [[ $retry_count -lt $max_retries ]]; do
    output=$("$@" 2>&1) && {
      echo "$output"
      return 0
    }
    exit_code=$?

    log_verbose "kubectl attempt $((retry_count + 1))/$max_retries failed (exit $exit_code): $output"
    retry_count=$((retry_count + 1))
    if [[ $retry_count -lt $max_retries ]]; then
      sleep $backoff
      backoff=$((backoff * 2))
    fi
  done

  log_error "kubectl command failed after $max_retries attempts: $*"
  return 1
}

wait_for_pod_phase() {
  local pod_name="$1"
  local target_phase="$2"
  local timeout="${3:-60}"
  local interval="${4:-2}"
  local elapsed=0
  local phase

  while [[ $elapsed -lt $timeout ]]; do
    phase=$(kubectl get pod "$pod_name" -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    log_verbose "Pod $pod_name phase=$phase (${elapsed}s/${timeout}s)"
    if [[ "$phase" == "$target_phase" ]]; then
      return 0
    fi
    # Also catch terminal failure quickly
    if [[ "$target_phase" != "Failed" && "$phase" == "Failed" ]]; then
      log_verbose "Pod $pod_name entered Failed phase unexpectedly"
      return 1
    fi
    sleep "$interval"
    elapsed=$((elapsed + interval))
  done

  log_error "Timed out waiting for pod $pod_name to reach phase $target_phase (last: $phase)"
  return 1
}

collect_diagnostics() {
  log_verbose "--- Collecting diagnostics ---"
  {
    echo "=== Pod descriptions ==="
    kubectl describe pods --all-namespaces 2>/dev/null || true
    echo ""
    echo "=== Containerd journal (last 200 lines) ==="
    docker exec "$NODE_ID" journalctl -u containerd -n 200 --no-pager 2>/dev/null || true
    echo ""
    echo "=== Kubelet journal (last 200 lines) ==="
    docker exec "$NODE_ID" journalctl -u kubelet -n 200 --no-pager 2>/dev/null || true
    echo ""
    echo "=== Reaper state files ==="
    docker exec "$NODE_ID" find /run/reaper -type f -exec sh -c 'echo "--- {} ---"; cat {}' \; 2>/dev/null || true
    echo ""
    echo "=== Docker containerd log ==="
    docker exec "$NODE_ID" tail -200 /var/log/containerd.log 2>/dev/null || true
  } >> "$LOG_FILE" 2>&1
  log_verbose "--- Diagnostics collected ---"
}

# ---------------------------------------------------------------------------
# Cleanup trap
# ---------------------------------------------------------------------------
cleanup() {
  local exit_code=$?
  if [[ $exit_code -ne 0 && -n "$NODE_ID" ]]; then
    log_status "Collecting diagnostics before exit..."
    collect_diagnostics
  fi
  if ! $NO_CLEANUP && [[ -n "$NODE_ID" ]]; then
    log_status "Deleting kind cluster $CLUSTER_NAME..."
    kind delete cluster --name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1 || true
  fi
  exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Test runner
# ---------------------------------------------------------------------------
run_test() {
  local func="$1"
  local name="$2"
  local fail_mode="${3:---hard-fail}"  # --hard-fail (default) or --soft-fail

  ci_group_start "Test: $name"

  local start_time
  start_time=$(date +%s%N 2>/dev/null || date +%s)

  local result=0
  "$func" || result=$?

  local end_time
  end_time=$(date +%s%N 2>/dev/null || date +%s)

  # Compute duration in seconds (with fractional if nanoseconds available)
  local duration
  if [[ ${#start_time} -gt 10 ]]; then
    duration=$(( (end_time - start_time) / 1000000 ))  # ms
    duration="$(( duration / 1000 )).$(( duration % 1000 / 100 ))s"
  else
    duration="$(( end_time - start_time ))s"
  fi

  TEST_NAMES+=("$name")
  TEST_DURATIONS+=("$duration")

  if [[ $result -eq 0 ]]; then
    log_status "${CLR_PASS}[PASS]${CLR_RESET}  $name  ${CLR_DIM}($duration)${CLR_RESET}"
    TEST_RESULTS+=("PASS")
    TESTS_PASSED=$((TESTS_PASSED + 1))
  elif [[ "$fail_mode" == "--soft-fail" ]]; then
    log_status "${CLR_WARN}[WARN]${CLR_RESET}  $name  ${CLR_DIM}($duration)${CLR_RESET}"
    TEST_RESULTS+=("WARN")
    TESTS_WARNED=$((TESTS_WARNED + 1))
  else
    log_status "${CLR_FAIL}[FAIL]${CLR_RESET}  $name  ${CLR_DIM}($duration)${CLR_RESET}"
    ci_error "Test failed: $name"
    TEST_RESULTS+=("FAIL")
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi

  ci_group_end
  return 0  # never abort mid-suite; summary handles exit code
}

# ---------------------------------------------------------------------------
# Phase 1: Cargo tests
# ---------------------------------------------------------------------------
phase_cargo_tests() {
  log_status ""
  log_status "${CLR_PHASE}Phase 1: Rust cargo tests${CLR_RESET}"
  log_status "========================================"
  ci_group_start "Phase 1: Rust cargo tests"

  cargo test --test integration_basic_binary 2>&1 | tee -a "$LOG_FILE"
  cargo test --test integration_user_management 2>&1 | tee -a "$LOG_FILE"
  cargo test --test integration_shim 2>&1 | tee -a "$LOG_FILE"

  log_status "All Rust integration tests passed."
  ci_group_end
}

# ---------------------------------------------------------------------------
# Phase 2: Infrastructure setup
# ---------------------------------------------------------------------------
phase_setup() {
  log_status ""
  log_status "${CLR_PHASE}Phase 2: Infrastructure setup${CLR_RESET}"
  log_status "========================================"
  ci_group_start "Phase 2: Infrastructure setup"

  # Ensure kind is installed
  if ! command -v kind >/dev/null 2>&1; then
    log_status "Installing kind..."
    curl -Lo ./kind "https://kind.sigs.k8s.io/dl/v0.23.0/kind-$(uname | tr '[:upper:]' '[:lower:]')-amd64" >> "$LOG_FILE" 2>&1
    chmod +x ./kind
    sudo mv ./kind /usr/local/bin/kind
  fi

  # Create or reuse kind cluster
  if kind get clusters 2>/dev/null | grep -q "^$CLUSTER_NAME\$"; then
    log_status "Kind cluster '$CLUSTER_NAME' already exists, reusing."
  else
    log_status "Creating kind cluster '$CLUSTER_NAME'..."
    if [[ -f "kind-config.yaml" ]]; then
      kind create cluster --name "$CLUSTER_NAME" --config kind-config.yaml >> "$LOG_FILE" 2>&1
    else
      kind create cluster --name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1
    fi
  fi

  # Detect node
  NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
  local node_arch
  node_arch=$(docker exec "$NODE_ID" uname -m)
  log_status "Kind node: $NODE_ID (arch: $node_arch)"

  # Build static musl binaries
  log_status "Building static musl Linux binaries..."
  local target_triple musl_image
  case "$node_arch" in
    aarch64)
      target_triple="aarch64-unknown-linux-musl"
      musl_image="messense/rust-musl-cross:aarch64-musl"
      ;;
    x86_64)
      target_triple="x86_64-unknown-linux-musl"
      musl_image="messense/rust-musl-cross:x86_64-musl"
      ;;
    *)
      log_error "Unsupported node arch: $node_arch"
      return 1
      ;;
  esac

  docker run --rm \
    -v "$(pwd)":/work \
    -w /work \
    "$musl_image" \
    cargo build --release --bin "$SHIM_BIN" --bin "$RUNTIME_BIN" --target "$target_triple" >> "$LOG_FILE" 2>&1

  # Deploy binaries to kind node
  local shim_path
  shim_path="$(pwd)/target/$target_triple/release/$SHIM_BIN"
  local runtime_path
  runtime_path="$(pwd)/target/$target_triple/release/$RUNTIME_BIN"

  log_status "Deploying binaries to kind node..."
  {
    docker cp "$shim_path" "$NODE_ID":/usr/local/bin/$SHIM_BIN
    docker exec "$NODE_ID" chmod +x /usr/local/bin/$SHIM_BIN
    docker cp "$runtime_path" "$NODE_ID":/usr/local/bin/$RUNTIME_BIN
    docker exec "$NODE_ID" chmod +x /usr/local/bin/$RUNTIME_BIN
  } >> "$LOG_FILE" 2>&1

  # Create overlay directories
  docker exec "$NODE_ID" mkdir -p /run/reaper/overlay/upper /run/reaper/overlay/work >> "$LOG_FILE" 2>&1

  # Configure containerd
  log_status "Configuring containerd..."
  ./scripts/configure-containerd.sh kind "$NODE_ID" >> "$LOG_FILE" 2>&1

  log_status "Infrastructure setup complete."
  ci_group_end
}

# ---------------------------------------------------------------------------
# Phase 3: K8s readiness
# ---------------------------------------------------------------------------
phase_readiness() {
  log_status ""
  log_status "${CLR_PHASE}Phase 3: Kubernetes readiness${CLR_RESET}"
  log_status "========================================"
  ci_group_start "Phase 3: Kubernetes readiness"

  # Wait for API server
  log_status "Waiting for Kubernetes API server..."
  retry_kubectl kubectl wait --for=condition=Ready node --all --timeout=300s >> "$LOG_FILE" 2>&1 || {
    log_verbose "Initial node wait failed, giving API server more time..."
    sleep 10
  }
  sleep 30  # stability buffer

  # Create RuntimeClass
  log_status "Creating RuntimeClass..."
  cat <<'YAML' | retry_kubectl kubectl apply -f - --validate=false >> "$LOG_FILE" 2>&1
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper-v2
handler: reaper-v2
YAML

  for i in $(seq 1 30); do
    if kubectl get runtimeclass reaper-v2 &>/dev/null; then
      log_status "RuntimeClass reaper-v2 ready."
      break
    fi
    log_verbose "Waiting for RuntimeClass... ($i/30)"
    sleep 1
  done

  # Wait for default service account
  log_status "Waiting for default ServiceAccount..."
  retry_kubectl kubectl wait --for=jsonpath='{.metadata.name}'=default serviceaccount/default -n default --timeout=60s >> "$LOG_FILE" 2>&1 || {
    for i in $(seq 1 30); do
      if kubectl get serviceaccount default -n default &>/dev/null; then
        log_status "Default service account ready."
        break
      fi
      log_verbose "Waiting for service account... ($i/30)"
      sleep 2
    done
  }

  # Clean stale pods
  log_verbose "Cleaning stale pods from previous runs..."
  kubectl delete pod reaper-example reaper-integration-test reaper-dns-check \
    reaper-overlay-writer reaper-overlay-reader reaper-exec-test \
    --ignore-not-found >> "$LOG_FILE" 2>&1 || true

  log_status "Kubernetes cluster ready."
  ci_group_end
}

# ---------------------------------------------------------------------------
# Phase 4: Integration tests
# ---------------------------------------------------------------------------

test_dns_resolution() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-dns-check
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: check
      image: busybox
      command:
        - /bin/sh
        - -c
        - |
          set -e
          echo "=== DNS Check ==="
          if [ ! -f /etc/resolv.conf ]; then
            echo "FAIL: /etc/resolv.conf does not exist"; exit 1
          fi
          echo "resolv.conf exists"
          if [ ! -s /etc/resolv.conf ]; then
            echo "FAIL: /etc/resolv.conf is empty"; exit 1
          fi
          echo "resolv.conf size: $(wc -c < /etc/resolv.conf) bytes"
          if ! grep -q '^nameserver ' /etc/resolv.conf; then
            echo "FAIL: No nameserver entries"; cat /etc/resolv.conf; exit 1
          fi
          echo "Valid nameserver entries found"
          grep '^nameserver ' /etc/resolv.conf
          echo "=== DNS Check PASSED ==="
YAML

  # BUG FIX: use phase polling instead of condition=Succeeded (pods have phases, not conditions)
  wait_for_pod_phase reaper-dns-check Succeeded 60 2 || return 1
  local logs
  logs=$(kubectl logs reaper-dns-check 2>/dev/null || echo "")
  log_verbose "DNS check logs: $logs"
  [[ "$logs" == *"DNS Check PASSED"* ]]
}

test_echo_command() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-integration-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
  - name: test
    image: busybox
    command: ["/bin/echo", "Hello from Reaper!"]
YAML

  wait_for_pod_phase reaper-integration-test Succeeded 120 5 || {
    log_verbose "Echo pod did not succeed. Collecting pod info..."
    kubectl describe pod reaper-integration-test >> "$LOG_FILE" 2>&1 || true
    kubectl get pod reaper-integration-test -o yaml >> "$LOG_FILE" 2>&1 || true
    return 1
  }
  local logs
  logs=$(kubectl logs reaper-integration-test 2>/dev/null || echo "")
  log_verbose "Echo test logs: $logs"
  [[ "$logs" == *"Hello from Reaper!"* ]]
}

test_overlay_sharing() {
  # Writer pod
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-overlay-writer
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: writer
      image: busybox
      command: ["/bin/sh", "-c", "echo overlay-works > /tmp/overlay-test.txt"]
YAML

  wait_for_pod_phase reaper-overlay-writer Succeeded 60 2 || return 1

  # Reader pod
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-overlay-reader
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: reader
      image: busybox
      command: ["/bin/sh", "-c", "cat /tmp/overlay-test.txt"]
YAML

  wait_for_pod_phase reaper-overlay-reader Succeeded 60 2 || return 1

  local reader_output
  reader_output=$(kubectl logs reaper-overlay-reader 2>/dev/null || echo "")
  log_verbose "Overlay reader output: '$reader_output'"
  [[ "$reader_output" == "overlay-works" ]]
}

test_host_protection() {
  # The overlay writer from the previous test wrote /tmp/overlay-test.txt.
  # It must NOT appear on the host filesystem.
  local host_file_exists
  host_file_exists=$(docker exec "$NODE_ID" test -f /tmp/overlay-test.txt && echo "yes" || echo "no")
  log_verbose "Host file check: $host_file_exists"
  if [[ "$host_file_exists" != "no" ]]; then
    log_error "Host protection FAILED: /tmp/overlay-test.txt leaked to host filesystem"
    log_error "Overlay isolation is mandatory. Workloads must not modify the host."
    return 1
  fi
  return 0
}

test_no_defunct_processes() {
  # Wait briefly to let any remaining shim processes settle after pod completions
  sleep 5

  local defunct_output
  defunct_output=$(docker exec "$NODE_ID" ps aux 2>/dev/null | grep -E '\<defunct\>' | grep -v grep || true)

  if [[ -n "$defunct_output" ]]; then
    log_error "Defunct (zombie) processes found on node:"
    log_error "$defunct_output"

    # Also grab the process tree for diagnostics
    local pstree_output
    pstree_output=$(docker exec "$NODE_ID" ps auxf 2>/dev/null || true)
    log_verbose "Full process tree: $pstree_output"

    return 1
  fi

  log_verbose "No defunct processes found on node."
  return 0
}

test_shim_cleanup_after_delete() {
  # After all test pods have been deleted, there should be no lingering
  # containerd-shim-reaper-v2 processes for k8s.io containers.
  # Each shim's shutdown() must signal ExitSignal so the process exits.
  sleep 5

  # Count reaper shim processes still running
  local shim_pids
  shim_pids=$(docker exec "$NODE_ID" ps aux 2>/dev/null \
    | grep '[c]ontainerd-shim-reaper-v2' \
    | grep -v grep || true)

  local shim_count
  shim_count=$(echo "$shim_pids" | grep -c . 2>/dev/null || echo 0)

  # Count how many reaper pods are still actually running
  local running_pods
  running_pods=$(kubectl get pods --no-headers 2>/dev/null \
    | grep -c '^reaper-' || echo 0)

  log_verbose "Shim processes: $shim_count, Running reaper pods: $running_pods"

  if [[ "$shim_count" -gt 0 && "$running_pods" -eq 0 ]]; then
    log_error "Found $shim_count orphaned containerd-shim-reaper-v2 processes with no reaper pods running:"
    log_error "$shim_pids"

    # Grab container IDs from the shim command lines for diagnostics
    local shim_ids
    shim_ids=$(docker exec "$NODE_ID" ps aux 2>/dev/null \
      | grep '[c]ontainerd-shim-reaper-v2' \
      | grep -oP '(?<=-id )[0-9a-f]+' || true)
    log_verbose "Orphaned shim container IDs: $shim_ids"

    return 1
  fi

  log_verbose "Shim cleanup OK: $shim_count shims for $running_pods pods."
  return 0
}

test_exec_support() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-exec-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["sleep", "60"]
YAML

  wait_for_pod_phase reaper-exec-test Running 60 1 || return 1

  local exec_output
  exec_output=$(kubectl exec reaper-exec-test -- echo 'exec works' 2>/dev/null || echo "")
  log_verbose "Exec output: '$exec_output'"
  [[ "$exec_output" == "exec works" ]]
}

phase_integration_tests() {
  log_status ""
  log_status "${CLR_PHASE}Phase 4: Integration tests${CLR_RESET}"
  log_status "========================================"

  run_test test_dns_resolution   "DNS resolution check"          --hard-fail
  run_test test_echo_command     "Echo command execution"        --hard-fail
  run_test test_overlay_sharing  "Overlay filesystem sharing"    --hard-fail
  run_test test_host_protection  "Host filesystem protection"    --hard-fail
  run_test test_exec_support     "kubectl exec support"          --soft-fail

  # Cleanup test pods (before defunct check so pods are terminated)
  kubectl delete pod reaper-dns-check reaper-integration-test \
    reaper-overlay-writer reaper-overlay-reader reaper-exec-test \
    --ignore-not-found >> "$LOG_FILE" 2>&1 || true

  # Wait for all pods to fully terminate before checking for zombies
  log_verbose "Waiting for test pods to terminate..."
  for i in $(seq 1 30); do
    local remaining
    remaining=$(kubectl get pods --no-headers 2>/dev/null | grep -c '^reaper-' || true)
    if [[ "$remaining" -eq 0 ]]; then
      break
    fi
    log_verbose "Still $remaining reaper pods remaining ($i/30)..."
    sleep 2
  done

  # Run defunct check last, after all pods are gone
  run_test test_no_defunct_processes "No defunct (zombie) processes" --hard-fail
  run_test test_shim_cleanup_after_delete "Shim processes exit after pod delete" --hard-fail
}

# ---------------------------------------------------------------------------
# Phase 5: Summary
# ---------------------------------------------------------------------------
phase_summary() {
  log_status ""
  log_status "${CLR_PHASE}Summary${CLR_RESET}"
  log_status "========================================"

  local total=$((TESTS_PASSED + TESTS_FAILED + TESTS_WARNED))
  for i in "${!TEST_NAMES[@]}"; do
    local badge
    case "${TEST_RESULTS[$i]}" in
      PASS) badge="${CLR_PASS}PASS${CLR_RESET}" ;;
      FAIL) badge="${CLR_FAIL}FAIL${CLR_RESET}" ;;
      WARN) badge="${CLR_WARN}WARN${CLR_RESET}" ;;
    esac
    log_status "  [$badge]  ${TEST_NAMES[$i]}  (${TEST_DURATIONS[$i]})"
  done

  log_status ""
  log_status "Total: $total  Passed: $TESTS_PASSED  Failed: $TESTS_FAILED  Warned: $TESTS_WARNED"
  log_status "Logs: $LOG_FILE"

  # GitHub Actions step summary
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "## Integration Test Results"
      echo ""
      echo "| Test | Result | Duration |"
      echo "|------|--------|----------|"
      for i in "${!TEST_NAMES[@]}"; do
        local emoji
        case "${TEST_RESULTS[$i]}" in
          PASS) emoji="+" ;;
          FAIL) emoji="x" ;;
          WARN) emoji="!" ;;
        esac
        echo "| ${TEST_NAMES[$i]} | $emoji ${TEST_RESULTS[$i]} | ${TEST_DURATIONS[$i]} |"
      done
      echo ""
      echo "**Total:** $total | **Passed:** $TESTS_PASSED | **Failed:** $TESTS_FAILED | **Warned:** $TESTS_WARNED"
    } >> "$GITHUB_STEP_SUMMARY"
  fi

  if [[ $TESTS_FAILED -gt 0 ]]; then
    log_error "Integration tests FAILED ($TESTS_FAILED failure(s))."
    return 1
  fi
  log_status "${CLR_PASS}All integration tests passed.${CLR_RESET}"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
  log_status "${CLR_PHASE}Reaper Integration Test Suite${CLR_RESET}"
  log_status "========================================"
  log_status "Timestamp: $TEST_TIMESTAMP"
  log_status "Commit: $COMMIT_ID"
  log_status "Log file: $LOG_FILE"

  if ! $SKIP_CARGO; then
    phase_cargo_tests
  else
    log_status "Skipping cargo tests (--skip-cargo)."
  fi

  phase_setup
  phase_readiness
  phase_integration_tests
  phase_summary
}

main
