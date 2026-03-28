#!/usr/bin/env bash
# run-integration-tests.sh — Structured integration test harness for Reaper runtime
# Orchestrator: parses args, sources library modules, runs phases.
#
# Usage:
#   ./scripts/run-integration-tests.sh                    # Full run (cargo + kind + tests)
#   ./scripts/run-integration-tests.sh --skip-cargo       # Skip Rust cargo tests
#   ./scripts/run-integration-tests.sh --no-cleanup       # Keep kind cluster after run
#   ./scripts/run-integration-tests.sh --verbose          # Print verbose output to stdout too
#   ./scripts/run-integration-tests.sh --agent-only       # Only run agent tests (fast iteration)
#   ./scripts/run-integration-tests.sh --crd-only        # Only run CRD controller tests (fast iteration)

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
AGENT_ONLY=false
CRD_ONLY=false
TEST_FILTER=""

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case $1 in
    --skip-cargo)  SKIP_CARGO=true; shift ;;
    --no-cleanup)  NO_CLEANUP=true; shift ;;
    --verbose)     VERBOSE=true; shift ;;
    --agent-only)  AGENT_ONLY=true; SKIP_CARGO=true; shift ;;
    --crd-only)    CRD_ONLY=true; SKIP_CARGO=true; shift ;;
    --test)
      TEST_FILTER="${2:-}"
      [[ -z "$TEST_FILTER" ]] && { echo "--test requires a pattern" >&2; exit 1; }
      SKIP_CARGO=true
      shift 2
      ;;
    -h|--help)
      echo "Usage: $0 [OPTIONS]"
      echo ""
      echo "Options:"
      echo "  --skip-cargo       Skip Rust cargo tests (for quick K8s-only reruns)"
      echo "  --no-cleanup       Keep kind cluster after run"
      echo "  --verbose          Also print verbose output to stdout"
      echo "  --agent-only       Only run agent tests (skip cargo + integration tests)"
      echo "  --crd-only         Only run CRD controller tests (skip cargo + other tests)"
      echo "  --test <pattern>   Run only tests matching pattern (implies --skip-cargo)"
      echo "                     Pattern matches against function name, e.g.:"
      echo "                       --test overlay_name    # test_overlay_name_isolation"
      echo "                       --test dns             # test_dns_resolution + test_kubernetes_dns_resolution"
      echo "                       --test agent_job       # all agent job API tests"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Usage: $0 [--skip-cargo] [--no-cleanup] [--verbose] [--test <pattern>]" >&2
      exit 1
      ;;
  esac
done

export TEST_FILTER

# ---------------------------------------------------------------------------
# Resolve script directory and source libraries
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

source "$SCRIPT_DIR/lib/test-common.sh"
source "$SCRIPT_DIR/lib/test-phases.sh"
source "$SCRIPT_DIR/lib/test-integration-suite.sh"

# ---------------------------------------------------------------------------
# Initialize
# ---------------------------------------------------------------------------
setup_colors

COMMIT_ID=$(get_commit_id)
TEST_TIMESTAMP=$(date '+%Y-%m-%d %H:%M:%S')

mkdir -p "$LOG_DIR"
: > "$LOG_FILE"  # truncate

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
  # Clean up dedicated kubeconfig and parallel results
  rm -f "/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
  rm -rf "${PARALLEL_RESULTS_DIR:-/tmp/nonexistent}" 2>/dev/null || true
  exit "$exit_code"
}
trap cleanup EXIT

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

  if ! $AGENT_ONLY && ! $CRD_ONLY; then
    phase_integration_tests
  else
    log_status "Skipping integration tests (--agent-only or --crd-only)."
  fi

  # Run remaining phases in parallel when all are enabled (no --test filter,
  # not --agent-only or --crd-only). Agent tests are independent of CRD phases
  # (controller, overlay, daemon-job) — they use different resources and namespaces.
  local run_parallel=false
  if ! $AGENT_ONLY && ! $CRD_ONLY && [[ -z "${TEST_FILTER:-}" ]]; then
    run_parallel=true
  fi

  if $run_parallel; then
    log_status ""
    log_status "${CLR_PHASE}Running agent tests and CRD phases in parallel...${CLR_RESET}"

    # Set up per-phase result files for cross-process collection
    PARALLEL_RESULTS_DIR=$(mktemp -d /tmp/reaper-parallel-results-XXXXXX)
    export PARALLEL_RESULTS_DIR

    # Agent tests in background subshell
    (
      export RESULTS_FILE="$PARALLEL_RESULTS_DIR/agent.results"
      : > "$RESULTS_FILE"
      phase_agent_tests
    ) > >(tee -a "$LOG_FILE") 2>&1 &
    local agent_pid=$!

    # CRD phases in foreground (sequential — they share controller state)
    export RESULTS_FILE="$PARALLEL_RESULTS_DIR/crd.results"
    : > "$RESULTS_FILE"
    phase_controller_tests
    phase_overlay_tests
    phase_daemon_job_tests
    unset RESULTS_FILE

    # Wait for agent tests to complete
    log_status "Waiting for agent tests to finish..."
    wait "$agent_pid" || true
  else
    if ! $CRD_ONLY; then
      phase_agent_tests
    else
      log_status "Skipping agent tests (--crd-only)."
    fi

    if ! $AGENT_ONLY; then
      phase_controller_tests
    else
      log_status "Skipping controller tests (--agent-only)."
    fi

    if ! $AGENT_ONLY; then
      phase_overlay_tests
    else
      log_status "Skipping overlay tests (--agent-only)."
    fi

    if ! $AGENT_ONLY; then
      phase_daemon_job_tests
    else
      log_status "Skipping daemon job tests (--agent-only)."
    fi
  fi

  phase_summary
}

main
