#!/usr/bin/env bash
# test-common.sh — Shared utilities for Reaper integration tests
# Sourced by run-integration-tests.sh; do not execute directly.

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

# ---------------------------------------------------------------------------
# Logging helpers
# ---------------------------------------------------------------------------
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

dump_pod_diagnostics() {
  local pod_name="$1"
  local diag_header="--- Diagnostics for pod $pod_name ---"
  log_status "$diag_header"

  # Pod status overview
  local pod_json
  pod_json=$(kubectl get pod "$pod_name" -o json 2>/dev/null || echo "")
  if [[ -n "$pod_json" ]]; then
    local phase status_msg
    phase=$(echo "$pod_json" | jq -r '.status.phase // "unknown"' 2>/dev/null || echo "unknown")
    log_status "  Pod phase: $phase"

    # Container statuses — show waiting/terminated reasons
    local container_statuses
    container_statuses=$(echo "$pod_json" | jq -r '
      .status.containerStatuses // [] | .[] |
      . as $cs | .state | to_entries[] |
      "    Container \($cs.name): state=\(.key)" +
      (if .value.reason then " reason=\(.value.reason)" else "" end) +
      (if .value.message then " message=\(.value.message)" else "" end) +
      (if .value.exitCode != null then " exitCode=\(.value.exitCode)" else "" end)
    ' 2>/dev/null || echo "    (container status unavailable)")
    log_status "$container_statuses"
  else
    log_status "  (pod $pod_name not found or kubectl failed)"
  fi

  # Pod events (often reveals scheduling/pull/runtime errors)
  log_status "  Events:"
  local events
  events=$(kubectl get events --field-selector "involvedObject.name=$pod_name" \
    --sort-by='.lastTimestamp' -o custom-columns=TIME:.lastTimestamp,TYPE:.type,REASON:.reason,MESSAGE:.message \
    --no-headers 2>/dev/null || echo "    (no events found)")
  # Indent each line for readability
  echo "$events" | while IFS= read -r line; do
    log_status "    $line"
  done

  # Container logs (may not exist if container never started)
  local logs
  logs=$(kubectl logs "$pod_name" --all-containers=true 2>&1 || echo "(no logs available)")
  log_status "  Container logs:"
  echo "$logs" | while IFS= read -r line; do
    log_status "    $line"
  done

  # Reaper runtime log (shows daemon errors that go to /dev/null on stdout)
  local runtime_log
  runtime_log=$(docker exec "$NODE_ID" tail -50 /run/reaper/runtime.log 2>/dev/null || echo "(no runtime log)")
  log_status "  Reaper runtime log (last 50 lines):"
  echo "$runtime_log" | while IFS= read -r line; do
    log_status "    $line"
  done

  # kubectl describe (full detail, to log file only to avoid overwhelming stdout)
  {
    echo "=== kubectl describe pod $pod_name ==="
    kubectl describe pod "$pod_name" 2>/dev/null || true
    echo "=== kubectl get pod $pod_name -o yaml ==="
    kubectl get pod "$pod_name" -o yaml 2>/dev/null || true
  } >> "$LOG_FILE" 2>&1

  log_status "--- End diagnostics for $pod_name ---"
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
    echo "=== Reaper runtime log ==="
    docker exec "$NODE_ID" cat /run/reaper/runtime.log 2>/dev/null || echo "(no runtime log)"
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
    # Dump node-level runtime logs on hard failures for context
    if [[ -n "$NODE_ID" ]]; then
      log_status "  Containerd logs (last 50 lines):"
      docker exec "$NODE_ID" journalctl -u containerd -n 50 --no-pager 2>/dev/null \
        | while IFS= read -r line; do log_status "    $line"; done || true
      log_status "  Kubelet logs (last 30 lines):"
      docker exec "$NODE_ID" journalctl -u kubelet -n 30 --no-pager 2>/dev/null \
        | while IFS= read -r line; do log_status "    $line"; done || true
    fi
    log_status "  Full diagnostic log: $LOG_FILE"
  fi

  ci_group_end
  return 0  # never abort mid-suite; summary handles exit code
}
