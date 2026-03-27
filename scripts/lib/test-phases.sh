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
  # It handles: cluster creation, image builds (node + controller + agent),
  # Helm install (CRD, RuntimeClass, DaemonSet, controller, agent), readiness, smoke test.
  local setup_args=(
    --cluster-name "$CLUSTER_NAME"
    --quiet
  )

  # Use the single-node Kind config for CI (playground default is 3-node)
  if [[ -f "scripts/kind-config.yaml" ]]; then
    setup_args+=(--kind-config "scripts/kind-config.yaml")
  fi

  # In CI with --skip-cargo, binaries are pre-built artifacts — skip compilation
  if [[ -n "${CI:-}" ]] && $SKIP_CARGO; then
    setup_args+=(--skip-build)
  fi

  # Always start with a fresh cluster for integration tests.
  # Reusing a cluster from a previous --no-cleanup run leaves stale containerd
  # sandbox state that causes "fork/exec shim: no such file or directory" errors.
  if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
    log_status "Deleting existing cluster '$CLUSTER_NAME' for clean test run..."
    kind delete cluster --name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1 || true
  fi

  log_status "Running setup-playground.sh for cluster '$CLUSTER_NAME'..."
  ./scripts/setup-playground.sh "${setup_args[@]}" 2>&1 | tee -a "$LOG_FILE" || {
    log_error "Cluster setup failed; review why and fix it"
    echo "--- setup-playground.sh log (/tmp/reaper-playground-setup.log) ---" >&2
    cat /tmp/reaper-playground-setup.log >&2 2>/dev/null || echo "(log file not found)" >&2
    echo "--- integration test log (last 100 lines) ---" >&2
    tail -100 "$LOG_FILE" >&2
    exit 1
  }

  # Set dedicated KUBECONFIG so all kubectl commands target the right cluster,
  # even when the user has other Kind clusters or contexts active.
  KUBECONFIG_FILE="/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
  kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG_FILE"
  export KUBECONFIG="$KUBECONFIG_FILE"
  log_status "Using KUBECONFIG=$KUBECONFIG_FILE"

  # Capture NODE_ID for diagnostics (used by cleanup trap and test functions)
  NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')

  # Note: reaper-agent image is now built by setup-playground.sh alongside
  # reaper-node and reaper-controller (all 3 images loaded into Kind).

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
  # Here we handle test-specific readiness: functional probe, ServiceAccount, stale pods.

  # Functional readiness check: run a trivial Reaper pod instead of a blind sleep
  log_status "Verifying Reaper runtime is functional..."
  kubectl run reaper-readiness-probe \
    --image=busybox --restart=Never \
    --overrides='{"spec":{"runtimeClassName":"reaper-v2"}}' \
    -- echo "ready" >> "$LOG_FILE" 2>&1
  for i in $(seq 1 60); do
    phase=$(kubectl get pod reaper-readiness-probe -o jsonpath='{.status.phase}' 2>/dev/null || echo "Pending")
    if [[ "$phase" == "Succeeded" ]]; then
      log_status "Reaper readiness probe passed."
      break
    elif [[ "$phase" == "Failed" ]]; then
      log_error "Reaper readiness probe failed"
      kubectl logs reaper-readiness-probe >> "$LOG_FILE" 2>&1 || true
      break
    fi
    sleep 2
  done
  kubectl delete pod reaper-readiness-probe --ignore-not-found >> "$LOG_FILE" 2>&1 || true

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
  # Use --grace-period=0 --force to avoid pods stuck in Terminating state
  # with dead shim processes blocking containerd.
  log_verbose "Cleaning stale pods from previous runs..."
  kubectl delete pod reaper-example reaper-integration-test reaper-dns-check \
    reaper-overlay-writer reaper-overlay-reader reaper-exec-test \
    reaper-ovname-writer reaper-ovname-reader reaper-ovname-same \
    reaper-dns-annot-default reaper-dns-annot-host \
    reaper-readiness-probe \
    reaper-annot-combined reaper-annot-invalid reaper-annot-unknown \
    reaper-uid-gid-test reaper-privdrop-test \
    reaper-configmap-vol reaper-secret-vol reaper-emptydir-vol reaper-hostpath-vol \
    reaper-exit-code-test reaper-cmd-not-found reaper-env-test \
    reaper-stderr-test reaper-pgkill-test reaper-large-output \
    reaper-cwd-test reaper-sigterm-test reaper-ro-vol-test \
    reaper-concurrent-a reaper-concurrent-b reaper-concurrent-c \
    reaper-stress-1 reaper-stress-2 reaper-stress-3 \
    reaper-stress-4 reaper-stress-5 \
    --ignore-not-found --grace-period=0 --force >> "$LOG_FILE" 2>&1 || true
  kubectl delete configmap reaper-test-scripts --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete secret reaper-test-secret --ignore-not-found >> "$LOG_FILE" 2>&1 || true

  log_status "Kubernetes cluster ready."
  ci_group_end
}

# ---------------------------------------------------------------------------
# Import results from file-based collection (parallel phases)
# ---------------------------------------------------------------------------
import_results_from_files() {
  local results_dir="${1:-}"
  [[ -d "$results_dir" ]] || return 0

  for rfile in "$results_dir"/*.results; do
    [[ -f "$rfile" ]] || continue
    while IFS='|' read -r status name duration; do
      [[ -z "$status" ]] && continue
      TEST_NAMES+=("$name")
      TEST_RESULTS+=("$status")
      TEST_DURATIONS+=("$duration")
      case "$status" in
        PASS) TESTS_PASSED=$((TESTS_PASSED + 1)) ;;
        FAIL) TESTS_FAILED=$((TESTS_FAILED + 1)) ;;
        WARN) TESTS_WARNED=$((TESTS_WARNED + 1)) ;;
      esac
    done < "$rfile"
  done
}

# ---------------------------------------------------------------------------
# Phase 5: Summary
# ---------------------------------------------------------------------------
phase_summary() {
  # Import any results from parallel phase execution
  if [[ -n "${PARALLEL_RESULTS_DIR:-}" ]]; then
    import_results_from_files "$PARALLEL_RESULTS_DIR"
  fi

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
    log_status ""
    log_status "${CLR_FAIL}Failed tests:${CLR_RESET}"
    for i in "${!TEST_NAMES[@]}"; do
      if [[ "${TEST_RESULTS[$i]}" == "FAIL" ]]; then
        log_status "  ${CLR_FAIL}x${CLR_RESET} ${TEST_NAMES[$i]}  (${TEST_DURATIONS[$i]})"
      fi
    done
    log_status ""
    log_error "Integration tests FAILED ($TESTS_FAILED failure(s))."
    log_status "Download logs artifact or check failure details above for root cause."
    return 1
  fi
  log_status "${CLR_PASS}All integration tests passed.${CLR_RESET}"
}
