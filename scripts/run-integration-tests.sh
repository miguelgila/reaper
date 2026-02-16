#!/usr/bin/env bash
# run-integration-tests.sh — Structured integration test harness for Reaper runtime
# Orchestrator: parses args, sources library modules, runs phases.
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
  phase_integration_tests
  phase_summary
}

main
