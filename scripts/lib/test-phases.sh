#!/usr/bin/env bash
# test-phases.sh — Phase 1 (cargo), Phase 2 (setup), Phase 3 (readiness), Phase 5 (summary)
# Sourced by run-integration-tests.sh; do not execute directly.

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
  cargo test --test integration_io 2>&1 | tee -a "$LOG_FILE"
  cargo test --test integration_exec 2>&1 | tee -a "$LOG_FILE"

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

  # Delegate to the shared playground setup script.
  # It handles: cluster creation, binary build, Ansible install, readiness, smoke test.
  local setup_args=(
    --cluster-name "$CLUSTER_NAME"
    --quiet
  )

  # Use the single-node Kind config for CI (playground default is 3-node)
  if [[ -f "scripts/kind-config.yaml" ]]; then
    setup_args+=(--kind-config "scripts/kind-config.yaml")
  fi

  log_status "Running setup-playground.sh for cluster '$CLUSTER_NAME'..."
  ./scripts/setup-playground.sh "${setup_args[@]}" 2>&1 | tee -a "$LOG_FILE" || {
    log_error "Cluster setup failed"
    tail -100 "$LOG_FILE" >&2
    exit 1
  }

  # Capture NODE_ID for diagnostics (used by cleanup trap and test functions)
  NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')

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

  # Node readiness and RuntimeClass are already verified by setup-playground.sh.
  # Here we handle test-specific readiness: stability buffer, ServiceAccount, stale pods.

  sleep 30  # stability buffer for CI test reliability

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

  # Clean stale pods from previous runs (--no-cleanup reuse)
  log_verbose "Cleaning stale pods from previous runs..."
  kubectl delete pod reaper-example reaper-integration-test reaper-dns-check \
    reaper-overlay-writer reaper-overlay-reader reaper-exec-test \
    reaper-uid-gid-test reaper-privdrop-test \
    reaper-configmap-vol reaper-secret-vol reaper-emptydir-vol reaper-hostpath-vol \
    reaper-exit-code-test reaper-cmd-not-found reaper-env-test \
    reaper-stderr-test reaper-pgkill-test reaper-large-output \
    reaper-cwd-test reaper-sigterm-test reaper-ro-vol-test \
    reaper-concurrent-a reaper-concurrent-b reaper-concurrent-c \
    reaper-stress-1 reaper-stress-2 reaper-stress-3 \
    reaper-stress-4 reaper-stress-5 \
    --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete configmap reaper-test-scripts --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete secret reaper-test-secret --ignore-not-found >> "$LOG_FILE" 2>&1 || true

  log_status "Kubernetes cluster ready."
  ci_group_end
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
