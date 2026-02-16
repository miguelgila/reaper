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

  # Build static musl binaries for Kind (Linux containers)
  # We must use static musl binaries to avoid glibc version mismatches
  log_status "Detecting Kind node architecture..."
  NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
  NODE_ARCH=$(docker exec "$NODE_ID" uname -m 2>&1) || {
    log_error "Failed to detect node architecture"
    exit 1
  }
  log_status "Node architecture: $NODE_ARCH"

  log_status "Building static musl Linux binaries via Docker..."
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
      log_error "Unsupported node architecture: $NODE_ARCH"
      exit 1
      ;;
  esac

  docker run --rm \
    -v "$(pwd)":/work \
    -w /work \
    "$MUSL_IMAGE" \
    cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime --target "$TARGET_TRIPLE" \
    >> "$LOG_FILE" 2>&1 || {
      log_error "Failed to build binaries"
      tail -50 "$LOG_FILE" >&2
      exit 1
    }

  # Set binary directory for Ansible installer
  # In CI, target/release may be owned by a different user (from cache), so we use the
  # target-specific directory directly without copying
  if [[ -n "${CI:-}" ]]; then
    # CI mode: Use binaries directly from target/<triple>/release to avoid permission issues
    export REAPER_BINARY_DIR="$(pwd)/target/$TARGET_TRIPLE/release"
    log_status "Using binaries from $REAPER_BINARY_DIR (CI mode)..."
  else
    # Local mode: Copy to target/release for convenience
    log_status "Copying binaries to target/release/ for installer..."
    mkdir -p target/release
    cp "target/$TARGET_TRIPLE/release/containerd-shim-reaper-v2" target/release/ 2>&1 | tee -a "$LOG_FILE"
    cp "target/$TARGET_TRIPLE/release/reaper-runtime" target/release/ 2>&1 | tee -a "$LOG_FILE"
    # REAPER_BINARY_DIR not needed in local mode (uses default)
  fi

  # Install Reaper using the unified Ansible installer
  log_status "Installing Reaper runtime to Kind cluster (via Ansible)..."
  if $VERBOSE; then
    ./scripts/install-reaper.sh --kind "$CLUSTER_NAME" --verbose 2>&1 | tee -a "$LOG_FILE"
  else
    # Show errors on stderr even in non-verbose mode
    ./scripts/install-reaper.sh --kind "$CLUSTER_NAME" 2>&1 | tee -a "$LOG_FILE" || {
      log_error "Ansible installer failed"
      tail -100 "$LOG_FILE" >&2
      exit 1
    }
  fi

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

  # Verify RuntimeClass was created by install script
  log_status "Verifying RuntimeClass..."
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
    reaper-uid-gid-test reaper-privdrop-test \
    reaper-configmap-vol reaper-secret-vol reaper-emptydir-vol reaper-hostpath-vol \
    reaper-exit-code-test reaper-cmd-not-found reaper-env-test \
    reaper-stderr-test reaper-pgkill-test reaper-large-output \
    reaper-cwd-test reaper-sigterm-test reaper-ro-vol-test \
    reaper-concurrent-a reaper-concurrent-b reaper-concurrent-c \
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
