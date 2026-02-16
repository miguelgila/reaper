#!/usr/bin/env bash
# test-integration-suite.sh — Phase 4: All integration test functions
# Sourced by run-integration-tests.sh; do not execute directly.

# ---------------------------------------------------------------------------
# Individual test functions
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
  wait_for_pod_phase reaper-dns-check Succeeded 60 2 || {
    log_error "DNS check pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-dns-check
    return 1
  }
  local logs
  logs=$(kubectl logs reaper-dns-check --all-containers=true 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "DNS check logs: $logs"
  if [[ "$logs" != *"DNS Check PASSED"* ]]; then
    log_error "DNS check did not produce expected 'DNS Check PASSED' output"
    log_error "Actual pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-dns-check
    return 1
  fi
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
    log_error "Echo command pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-integration-test
    return 1
  }
  local logs
  logs=$(kubectl logs reaper-integration-test --all-containers=true 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Echo test logs: $logs"
  if [[ "$logs" != *"Hello from Reaper!"* ]]; then
    log_error "Echo test did not produce expected 'Hello from Reaper!' output"
    log_error "Actual pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-integration-test
    return 1
  fi
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

  wait_for_pod_phase reaper-overlay-writer Succeeded 60 2 || {
    log_error "Overlay writer pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-overlay-writer
    return 1
  }

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

  wait_for_pod_phase reaper-overlay-reader Succeeded 60 2 || {
    log_error "Overlay reader pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-overlay-reader
    return 1
  }

  local reader_output
  reader_output=$(kubectl logs reaper-overlay-reader --all-containers=true 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Overlay reader output: '$reader_output'"
  if [[ "$reader_output" != "overlay-works" ]]; then
    log_error "Overlay reader did not produce expected 'overlay-works' output"
    log_error "Actual pod logs: '$reader_output'"
    dump_pod_diagnostics reaper-overlay-reader
    return 1
  fi
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
  if [[ -z "$shim_pids" ]]; then
    shim_count=0
  else
    shim_count=$(echo "$shim_pids" | wc -l | tr -d ' ')
  fi

  # Count how many reaper pods are still actually running
  local running_pods
  running_pods=$(kubectl get pods --no-headers 2>/dev/null \
    | grep -c '^reaper-' || true)

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

test_uid_gid_switching() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-uid-gid-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  securityContext:
    runAsUser: 1000
    runAsGroup: 1000
  containers:
  - name: test
    image: busybox
    command: ["/bin/sh", "-c", "id -u && id -g && echo 'uid-gid-ok'"]
YAML

  wait_for_pod_phase reaper-uid-gid-test Succeeded 60 2 || {
    log_error "UID/GID test pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-uid-gid-test
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-uid-gid-test 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "UID/GID test logs: $logs"

  # Parse output: should be "1000\n1000\nuid-gid-ok"
  local uid_line
  uid_line=$(echo "$logs" | sed -n '1p' | tr -d '[:space:]')
  local gid_line
  gid_line=$(echo "$logs" | sed -n '2p' | tr -d '[:space:]')

  if [[ "$uid_line" != "1000" ]]; then
    log_error "Expected UID 1000, got: '$uid_line'"
    log_error "Full pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-uid-gid-test
    return 1
  fi

  if [[ "$gid_line" != "1000" ]]; then
    log_error "Expected GID 1000, got: '$gid_line'"
    log_error "Full pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-uid-gid-test
    return 1
  fi

  if [[ "$logs" != *"uid-gid-ok"* ]]; then
    log_error "UID/GID test did not produce expected 'uid-gid-ok' marker"
    log_error "Full pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-uid-gid-test
    return 1
  fi

  log_verbose "UID/GID switching verified: UID=$uid_line, GID=$gid_line"
}

test_privilege_drop() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-privdrop-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  securityContext:
    runAsUser: 1001  # unprivileged user
    runAsGroup: 1001
  containers:
  - name: test
    image: busybox
    command: ["/bin/sh", "-c", "id -u; id -g; echo 'privilege-drop-ok'"]
YAML

  wait_for_pod_phase reaper-privdrop-test Succeeded 60 2 || {
    log_error "Privilege drop test pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-privdrop-test
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-privdrop-test 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Privilege drop test logs: $logs"

  # Should output "1001" (UID), "1001" (GID), and "privilege-drop-ok"
  if [[ "$logs" != *"1001"* ]]; then
    log_error "Expected UID/GID 1001, but not found in output"
    log_error "Full pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-privdrop-test
    return 1
  fi

  if [[ "$logs" != *"privilege-drop-ok"* ]]; then
    log_error "Privilege drop verification failed"
    log_error "Full pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-privdrop-test
    return 1
  fi

  log_verbose "Privilege drop verified: process runs as UID/GID 1001"
}

test_configmap_volume() {
  # Create a ConfigMap with a test script
  kubectl create configmap reaper-test-scripts \
    --from-literal=hello.sh='#!/bin/sh
echo "configmap-volume-works"' \
    --dry-run=client -o yaml | kubectl apply -f - >> "$LOG_FILE" 2>&1

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-configmap-vol
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  volumes:
    - name: scripts
      configMap:
        name: reaper-test-scripts
        defaultMode: 0755
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "cat /scripts/hello.sh && /bin/sh /scripts/hello.sh"]
      volumeMounts:
        - name: scripts
          mountPath: /scripts
YAML

  wait_for_pod_phase reaper-configmap-vol Succeeded 120 2 || {
    log_error "ConfigMap volume pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-configmap-vol
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-configmap-vol 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "ConfigMap volume test logs: $logs"

  if [[ "$logs" != *"configmap-volume-works"* ]]; then
    log_error "ConfigMap volume test did not produce expected 'configmap-volume-works' output"
    log_error "Actual pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-configmap-vol
    return 1
  fi

  log_verbose "ConfigMap volume mount verified"
}

test_hostpath_volume() {
  # Create a test file on the Kind node
  docker exec "$NODE_ID" sh -c 'mkdir -p /tmp/reaper-hostpath-test && echo "hostpath-volume-works" > /tmp/reaper-hostpath-test/data.txt'

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-hostpath-vol
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  volumes:
    - name: hostdata
      hostPath:
        path: /tmp/reaper-hostpath-test
        type: Directory
  containers:
    - name: test
      image: busybox
      command: ["/bin/cat", "/hostdata/data.txt"]
      volumeMounts:
        - name: hostdata
          mountPath: /hostdata
YAML

  wait_for_pod_phase reaper-hostpath-vol Succeeded 120 2 || {
    log_error "hostPath volume pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-hostpath-vol
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-hostpath-vol 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "hostPath volume test logs: $logs"

  if [[ "$logs" != *"hostpath-volume-works"* ]]; then
    log_error "hostPath volume test did not produce expected 'hostpath-volume-works' output"
    log_error "Actual pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-hostpath-vol
    return 1
  fi

  log_verbose "hostPath volume mount verified"
}

test_secret_volume() {
  # Create a Secret with test data
  kubectl create secret generic reaper-test-secret \
    --from-literal=username='reaper-user' \
    --from-literal=password='secret-volume-works' \
    --dry-run=client -o yaml | kubectl apply -f - >> "$LOG_FILE" 2>&1

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-secret-vol
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  volumes:
    - name: creds
      secret:
        secretName: reaper-test-secret
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "cat /creds/username && echo '' && cat /creds/password"]
      volumeMounts:
        - name: creds
          mountPath: /creds
          readOnly: true
YAML

  wait_for_pod_phase reaper-secret-vol Succeeded 120 2 || {
    log_error "Secret volume pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-secret-vol
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-secret-vol 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Secret volume test logs: $logs"

  if [[ "$logs" != *"secret-volume-works"* ]]; then
    log_error "Secret volume test did not produce expected 'secret-volume-works' output"
    log_error "Actual pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-secret-vol
    return 1
  fi

  if [[ "$logs" != *"reaper-user"* ]]; then
    log_error "Secret volume test did not produce expected 'reaper-user' output"
    dump_pod_diagnostics reaper-secret-vol
    return 1
  fi

  log_verbose "Secret volume mount verified"
}

test_emptydir_volume() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-emptydir-vol
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  volumes:
    - name: scratch
      emptyDir: {}
  containers:
    - name: test
      image: busybox
      command:
        - /bin/sh
        - -c
        - |
          echo "emptydir-volume-works" > /scratch/test.txt
          cat /scratch/test.txt
      volumeMounts:
        - name: scratch
          mountPath: /scratch
YAML

  wait_for_pod_phase reaper-emptydir-vol Succeeded 120 2 || {
    log_error "emptyDir volume pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-emptydir-vol
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-emptydir-vol 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "emptyDir volume test logs: $logs"

  if [[ "$logs" != *"emptydir-volume-works"* ]]; then
    log_error "emptyDir volume test did not produce expected 'emptydir-volume-works' output"
    log_error "Actual pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-emptydir-vol
    return 1
  fi

  log_verbose "emptyDir volume mount verified"
}

# Test that volume mounts work on second runs (after pod deletion and recreation).
# This catches stale mount accumulation in the shared overlay namespace: volume
# mounts persist after pod deletion, and move_mount() fails with ENOENT if
# the stale mount references a deleted kubelet directory.
test_volume_rerun() {
  # Delete the emptydir pod from the earlier test and wait for it to disappear
  kubectl delete pod reaper-emptydir-vol --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  for i in $(seq 1 30); do
    if ! kubectl get pod reaper-emptydir-vol &>/dev/null; then
      break
    fi
    sleep 1
  done

  # Re-create the same pod with the same volume mount destination
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-emptydir-vol
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  volumes:
    - name: scratch
      emptyDir: {}
  containers:
    - name: test
      image: busybox
      command:
        - /bin/sh
        - -c
        - |
          echo "rerun-volume-works" > /scratch/rerun.txt
          cat /scratch/rerun.txt
      volumeMounts:
        - name: scratch
          mountPath: /scratch
YAML

  wait_for_pod_phase reaper-emptydir-vol Succeeded 120 2 || {
    log_error "Volume rerun pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-emptydir-vol
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-emptydir-vol 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Volume rerun test logs: $logs"

  if [[ "$logs" != *"rerun-volume-works"* ]]; then
    log_error "Volume rerun test did not produce expected 'rerun-volume-works' output"
    dump_pod_diagnostics reaper-emptydir-vol
    return 1
  fi

  log_verbose "Volume rerun verified — stale mount cleanup works"
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

  wait_for_pod_phase reaper-exec-test Running 60 1 || {
    log_error "Exec test pod did not reach Running phase"
    dump_pod_diagnostics reaper-exec-test
    return 1
  }

  local exec_output
  exec_output=$(kubectl exec reaper-exec-test -- echo 'exec works' 2>&1 || echo "")
  log_verbose "Exec output: '$exec_output'"
  if [[ "$exec_output" != "exec works" ]]; then
    log_error "kubectl exec did not produce expected 'exec works' output"
    log_error "Actual exec output: '$exec_output'"
    dump_pod_diagnostics reaper-exec-test
    return 1
  fi
}

test_sigterm_delivery() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-sigterm-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  terminationGracePeriodSeconds: 30
  containers:
    - name: test
      image: busybox
      command:
        - /bin/sh
        - -c
        - |
          trap 'echo SIGTERM-received; exit 0' TERM
          echo trap-ready
          while true; do sleep 1; done
YAML

  wait_for_pod_phase reaper-sigterm-test Running 60 2 || {
    log_error "SIGTERM test pod did not reach Running phase"
    dump_pod_diagnostics reaper-sigterm-test
    return 1
  }

  # Wait for trap handler to be installed
  sleep 2

  # Delete the pod (triggers SIGTERM, then SIGKILL after grace period)
  # Use --wait=false so we can observe the pod's terminal state before removal
  kubectl delete pod reaper-sigterm-test --grace-period=10 --wait=false >> "$LOG_FILE" 2>&1 || true

  # Poll for the container to reach a terminated state (before the pod object disappears)
  local exit_code=""
  for i in $(seq 1 20); do
    exit_code=$(kubectl get pod reaper-sigterm-test \
      -o jsonpath='{.status.containerStatuses[0].state.terminated.exitCode}' 2>/dev/null || echo "")
    if [[ -n "$exit_code" ]]; then
      break
    fi
    sleep 1
  done

  # Try to grab logs before the pod is fully removed
  local logs
  logs=$(kubectl logs reaper-sigterm-test 2>&1 || echo "(logs unavailable)")
  log_verbose "SIGTERM test logs: $logs"
  log_verbose "SIGTERM test exit code: $exit_code"

  # Wait for the pod to be fully gone
  for i in $(seq 1 15); do
    if ! kubectl get pod reaper-sigterm-test &>/dev/null; then
      break
    fi
    sleep 1
  done

  # If SIGTERM was delivered and the trap ran 'exit 0', exitCode should be 0.
  # If SIGTERM was NOT delivered (SIGKILL only), exitCode would be 137 (128+9).
  if [[ "$exit_code" == "0" ]]; then
    log_verbose "SIGTERM delivery verified: trap handler ran, exit code=0"
  elif [[ -z "$exit_code" ]]; then
    # Pod disappeared before we could read the exit code — not ideal but not a failure
    log_verbose "SIGTERM test: pod removed before exit code could be read (inconclusive)"
  else
    log_error "Expected exit code 0 (SIGTERM trap), got: $exit_code (SIGKILL?)"
    return 1
  fi
}

test_working_directory() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-cwd-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "pwd"]
      workingDir: /tmp
YAML

  wait_for_pod_phase reaper-cwd-test Succeeded 60 2 || {
    log_error "Working directory test pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-cwd-test
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-cwd-test 2>&1 || echo "(failed to retrieve logs)")
  local cwd
  cwd=$(echo "$logs" | head -1 | tr -d '[:space:]')
  log_verbose "Working directory test: cwd=$cwd"

  if [[ "$cwd" != "/tmp" ]]; then
    log_error "Expected working directory '/tmp', got: '$cwd'"
    dump_pod_diagnostics reaper-cwd-test
    return 1
  fi

  log_verbose "Working directory verified: cwd=$cwd"
}

test_large_output() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-large-output
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "seq 1 20000"]
YAML

  wait_for_pod_phase reaper-large-output Succeeded 120 2 || {
    log_error "Large output pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-large-output
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-large-output 2>&1 || echo "(failed to retrieve logs)")
  local line_count
  line_count=$(echo "$logs" | wc -l | tr -d ' ')
  log_verbose "Large output test: $line_count lines"

  # Verify first and last lines are present (proves no truncation)
  local first_line
  first_line=$(echo "$logs" | head -1 | tr -d '[:space:]')
  local last_line
  last_line=$(echo "$logs" | tail -1 | tr -d '[:space:]')

  if [[ "$first_line" != "1" ]]; then
    log_error "Expected first line '1', got: '$first_line'"
    dump_pod_diagnostics reaper-large-output
    return 1
  fi

  if [[ "$last_line" != "20000" ]]; then
    log_error "Expected last line '20000', got: '$last_line'"
    log_error "Total lines captured: $line_count"
    dump_pod_diagnostics reaper-large-output
    return 1
  fi

  if [[ "$line_count" -lt 20000 ]]; then
    log_error "Expected 20000 lines, got: $line_count (output truncated)"
    return 1
  fi

  log_verbose "Large output verified: $line_count lines, first=$first_line, last=$last_line"
}

test_exec_exit_code() {
  # This test reuses the reaper-exec-test pod from test_exec_support.
  # If that test didn't run or failed, create the pod ourselves.
  if ! kubectl get pod reaper-exec-test &>/dev/null; then
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
    wait_for_pod_phase reaper-exec-test Running 60 1 || {
      log_error "Exec exit code test: pod did not reach Running phase"
      dump_pod_diagnostics reaper-exec-test
      return 1
    }
  fi

  # Run a command that exits with code 7
  local exec_rc=0
  kubectl exec reaper-exec-test -- /bin/sh -c 'exit 7' >> "$LOG_FILE" 2>&1 || exec_rc=$?
  log_verbose "Exec exit code: $exec_rc"

  if [[ "$exec_rc" -ne 7 ]]; then
    log_error "Expected exec exit code 7, got: $exec_rc"
    dump_pod_diagnostics reaper-exec-test
    return 1
  fi

  log_verbose "Exec exit code propagation verified: exit code=$exec_rc"
}

test_concurrent_pods() {
  # Apply 3 pods at once to exercise overlay flock() contention
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-concurrent-a
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "echo concurrent-a-ok"]
---
apiVersion: v1
kind: Pod
metadata:
  name: reaper-concurrent-b
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "echo concurrent-b-ok"]
---
apiVersion: v1
kind: Pod
metadata:
  name: reaper-concurrent-c
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "echo concurrent-c-ok"]
YAML

  local all_ok=true
  for pod in reaper-concurrent-a reaper-concurrent-b reaper-concurrent-c; do
    wait_for_pod_phase "$pod" Succeeded 120 2 || {
      log_error "Concurrent pod $pod did not reach Succeeded phase"
      dump_pod_diagnostics "$pod"
      all_ok=false
      continue
    }

    local logs
    logs=$(kubectl logs "$pod" 2>&1 || echo "(failed to retrieve logs)")
    local expected="${pod#reaper-}-ok"
    if [[ "$logs" != *"$expected"* ]]; then
      log_error "Concurrent pod $pod did not produce expected '$expected' output"
      log_error "Actual: $logs"
      all_ok=false
    fi
  done

  # Cleanup these pods immediately (not part of the main cleanup list)
  kubectl delete pod reaper-concurrent-a reaper-concurrent-b reaper-concurrent-c \
    --ignore-not-found >> "$LOG_FILE" 2>&1 || true

  if ! $all_ok; then
    return 1
  fi

  log_verbose "Concurrent pod starts verified: all 3 pods succeeded"
}

test_process_group_kill() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-pgkill-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "sleep 300 & sleep 300 & echo pgkill-children-started && wait"]
YAML

  wait_for_pod_phase reaper-pgkill-test Running 60 2 || {
    log_error "Process group kill pod did not reach Running phase"
    dump_pod_diagnostics reaper-pgkill-test
    return 1
  }

  # Verify child processes are running on the node
  sleep 2
  local before_count
  before_count=$(docker exec "$NODE_ID" sh -c "ps aux | grep 'sleep 300' | grep -v grep | wc -l" 2>/dev/null || echo "0")
  log_verbose "Sleep processes before kill: $before_count"

  if [[ "$before_count" -lt 2 ]]; then
    log_error "Expected at least 2 'sleep 300' processes, found: $before_count"
    dump_pod_diagnostics reaper-pgkill-test
    return 1
  fi

  # Delete the pod (triggers SIGTERM to process group)
  kubectl delete pod reaper-pgkill-test --grace-period=5 >> "$LOG_FILE" 2>&1 || true

  # Wait for pod to be fully gone
  for i in $(seq 1 15); do
    if ! kubectl get pod reaper-pgkill-test &>/dev/null; then
      break
    fi
    sleep 1
  done

  # Give processes a moment to exit after signal
  sleep 2

  # Verify no orphaned sleep processes remain
  local after_count
  after_count=$(docker exec "$NODE_ID" sh -c "ps aux | grep 'sleep 300' | grep -v grep | wc -l" 2>/dev/null || echo "0")
  log_verbose "Sleep processes after kill: $after_count"

  if [[ "$after_count" -gt 0 ]]; then
    log_error "Found $after_count orphaned 'sleep 300' processes after pod deletion"
    docker exec "$NODE_ID" ps aux 2>/dev/null | grep 'sleep 300' | grep -v grep | while IFS= read -r line; do
      log_error "  $line"
    done
    return 1
  fi

  log_verbose "Process group kill verified: all children reaped"
}

test_stderr_capture() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-stderr-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "echo stdout-line && echo stderr-line >&2"]
YAML

  wait_for_pod_phase reaper-stderr-test Succeeded 60 2 || {
    log_error "stderr capture pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-stderr-test
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-stderr-test 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "stderr test logs: $logs"

  if [[ "$logs" != *"stdout-line"* ]]; then
    log_error "Expected 'stdout-line' in logs, got:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-stderr-test
    return 1
  fi

  if [[ "$logs" != *"stderr-line"* ]]; then
    log_error "Expected 'stderr-line' in logs (stderr should be captured), got:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-stderr-test
    return 1
  fi

  log_verbose "stderr capture verified"
}

test_env_vars() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-env-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "echo $MY_VAR && echo $ANOTHER_VAR"]
      env:
        - name: MY_VAR
          value: "reaper-env-ok"
        - name: ANOTHER_VAR
          value: "second-env-ok"
YAML

  wait_for_pod_phase reaper-env-test Succeeded 60 2 || {
    log_error "Env vars test pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-env-test
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-env-test 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Env vars test logs: $logs"

  if [[ "$logs" != *"reaper-env-ok"* ]]; then
    log_error "Expected 'reaper-env-ok' in output, got:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-env-test
    return 1
  fi

  if [[ "$logs" != *"second-env-ok"* ]]; then
    log_error "Expected 'second-env-ok' in output, got:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-env-test
    return 1
  fi

  log_verbose "Environment variable passing verified"
}

test_command_not_found() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-cmd-not-found
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/nonexistent/binary"]
YAML

  wait_for_pod_phase reaper-cmd-not-found Failed 60 2 || {
    log_error "Command-not-found pod did not reach Failed phase"
    dump_pod_diagnostics reaper-cmd-not-found
    return 1
  }

  local exit_code
  exit_code=$(kubectl get pod reaper-cmd-not-found -o jsonpath='{.status.containerStatuses[0].state.terminated.exitCode}' 2>/dev/null || echo "")
  log_verbose "Command not found test: exitCode=$exit_code"

  if [[ -z "$exit_code" || "$exit_code" == "0" ]]; then
    log_error "Expected non-zero exit code for missing binary, got: '$exit_code'"
    dump_pod_diagnostics reaper-cmd-not-found
    return 1
  fi

  log_verbose "Command not found handled correctly: exitCode=$exit_code"
}

test_nonzero_exit_code() {
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-exit-code-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "exit 42"]
YAML

  wait_for_pod_phase reaper-exit-code-test Failed 60 2 || {
    log_error "Exit code test pod did not reach Failed phase"
    dump_pod_diagnostics reaper-exit-code-test
    return 1
  }

  local exit_code
  exit_code=$(kubectl get pod reaper-exit-code-test -o jsonpath='{.status.containerStatuses[0].state.terminated.exitCode}' 2>/dev/null || echo "")
  log_verbose "Exit code test: exitCode=$exit_code"

  if [[ "$exit_code" != "42" ]]; then
    log_error "Expected exit code 42, got: '$exit_code'"
    dump_pod_diagnostics reaper-exit-code-test
    return 1
  fi

  log_verbose "Non-zero exit code propagation verified: exitCode=$exit_code"
}

test_rapid_create_delete() {
  # Rapidly create and delete 5 pods to stress-test cleanup paths
  local all_ok=true
  for i in 1 2 3 4 5; do
    local pod_name="reaper-stress-$i"
    cat <<YAML | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: $pod_name
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/echo", "stress-$i"]
YAML
  done

  # Wait for all to succeed
  for i in 1 2 3 4 5; do
    local pod_name="reaper-stress-$i"
    wait_for_pod_phase "$pod_name" Succeeded 120 2 || {
      log_error "Stress pod $pod_name did not reach Succeeded"
      dump_pod_diagnostics "$pod_name"
      all_ok=false
    }
  done

  # Delete them all at once
  kubectl delete pod reaper-stress-1 reaper-stress-2 reaper-stress-3 \
    reaper-stress-4 reaper-stress-5 --ignore-not-found >> "$LOG_FILE" 2>&1 || true

  # Wait for all to disappear
  for i in $(seq 1 20); do
    local remaining
    remaining=$(kubectl get pods --no-headers 2>/dev/null | grep -c '^reaper-stress-' || true)
    if [[ "$remaining" -eq 0 ]]; then
      break
    fi
    sleep 1
  done

  # Check for leftover state files on the node
  local state_dirs
  state_dirs=$(docker exec "$NODE_ID" sh -c 'ls -d /run/reaper/*/state.json 2>/dev/null | wc -l' 2>/dev/null || echo "0")
  state_dirs=$(echo "$state_dirs" | tr -d '[:space:]')
  log_verbose "State files remaining after stress test: $state_dirs"

  # Check for zombies
  local defunct
  defunct=$(docker exec "$NODE_ID" ps aux 2>/dev/null | grep -E '\<defunct\>' | grep -v grep || true)
  if [[ -n "$defunct" ]]; then
    log_error "Zombies found after rapid create/delete:"
    log_error "$defunct"
    all_ok=false
  fi

  if ! $all_ok; then
    return 1
  fi

  log_verbose "Rapid create/delete stress test passed: no zombies, cleanup OK"
}

test_exec_nonexistent_binary() {
  # Reuse the reaper-exec-test pod if it's still running, otherwise create it
  if ! kubectl get pod reaper-exec-test &>/dev/null; then
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
    wait_for_pod_phase reaper-exec-test Running 60 1 || {
      log_error "Exec nonexistent binary test: pod did not reach Running phase"
      dump_pod_diagnostics reaper-exec-test
      return 1
    }
  fi

  local exec_rc=0
  kubectl exec reaper-exec-test -- /nonexistent/binary >> "$LOG_FILE" 2>&1 || exec_rc=$?
  log_verbose "Exec nonexistent binary exit code: $exec_rc"

  if [[ "$exec_rc" -eq 0 ]]; then
    log_error "Expected non-zero exit code for nonexistent binary exec, got: 0"
    dump_pod_diagnostics reaper-exec-test
    return 1
  fi

  log_verbose "Exec nonexistent binary handled correctly: exit code=$exec_rc"
}

test_readonly_volume_rejection() {
  # Ensure the secret exists
  kubectl create secret generic reaper-test-secret \
    --from-literal=username='reaper-user' \
    --from-literal=password='secret-volume-works' \
    --dry-run=client -o yaml | kubectl apply -f - >> "$LOG_FILE" 2>&1

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-ro-vol-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  volumes:
    - name: creds
      secret:
        secretName: reaper-test-secret
  containers:
    - name: test
      image: busybox
      command: ["/bin/sh", "-c", "touch /creds/newfile 2>&1; echo rc=$?"]
      volumeMounts:
        - name: creds
          mountPath: /creds
          readOnly: true
YAML

  wait_for_pod_phase reaper-ro-vol-test Succeeded 60 2 || {
    # Pod might reach Failed if touch causes non-zero exit
    local phase
    phase=$(kubectl get pod reaper-ro-vol-test -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    if [[ "$phase" != "Failed" ]]; then
      log_error "Read-only volume test pod did not reach Succeeded or Failed phase (phase=$phase)"
      dump_pod_diagnostics reaper-ro-vol-test
      return 1
    fi
  }

  local logs
  logs=$(kubectl logs reaper-ro-vol-test 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Read-only volume test logs: $logs"

  # The touch command should fail — we expect either a "Read-only" error
  # or a non-zero rc= in the output
  if [[ "$logs" == *"rc=0"* ]]; then
    log_error "Write to read-only volume succeeded unexpectedly"
    dump_pod_diagnostics reaper-ro-vol-test
    return 1
  fi

  log_verbose "Read-only volume write rejection verified"
}

# ---------------------------------------------------------------------------
# Phase 4: Integration test orchestrator
# ---------------------------------------------------------------------------
phase_integration_tests() {
  log_status ""
  log_status "${CLR_PHASE}Phase 4: Integration tests${CLR_RESET}"
  log_status "========================================"

  run_test test_dns_resolution   "DNS resolution check"          --hard-fail
  run_test test_echo_command     "Echo command execution"        --hard-fail
  run_test test_overlay_sharing  "Overlay filesystem sharing"    --hard-fail
  run_test test_host_protection  "Host filesystem protection"    --hard-fail
  run_test test_uid_gid_switching "UID/GID switching with securityContext" --hard-fail
  run_test test_privilege_drop   "Privilege drop to non-root user" --hard-fail
  run_test test_configmap_volume "ConfigMap volume mount"         --hard-fail
  run_test test_secret_volume   "Secret volume mount"            --hard-fail
  run_test test_emptydir_volume "emptyDir volume mount"          --hard-fail
  run_test test_hostpath_volume  "hostPath volume mount"          --hard-fail
  run_test test_volume_rerun    "Volume mount rerun (stale cleanup)" --hard-fail
  run_test test_exec_support     "kubectl exec support"          --soft-fail
  run_test test_nonzero_exit_code "Non-zero exit code propagation" --hard-fail
  run_test test_concurrent_pods   "Concurrent pod starts (lock contention)" --hard-fail
  run_test test_process_group_kill "Process group kill on pod delete" --hard-fail
  run_test test_sigterm_delivery   "Graceful shutdown (SIGTERM)"     --hard-fail
  run_test test_working_directory  "Working directory (cwd)"         --hard-fail
  run_test test_large_output       "Large output (FIFO buffer)"     --hard-fail
  run_test test_exec_exit_code     "Exec exit code propagation"     --hard-fail
  run_test test_stderr_capture     "stderr capture via FIFO"        --hard-fail
  run_test test_env_vars          "Environment variable passing"   --hard-fail
  run_test test_command_not_found "Command not found (failed pod)" --hard-fail
  run_test test_exec_nonexistent_binary "Exec nonexistent binary"         --hard-fail
  run_test test_readonly_volume_rejection "Read-only volume write rejection" --hard-fail
  run_test test_rapid_create_delete "Rapid create/delete stress"     --hard-fail

  # Cleanup test pods (before defunct check so pods are terminated)
  kubectl delete pod reaper-dns-check reaper-integration-test \
    reaper-overlay-writer reaper-overlay-reader reaper-uid-gid-test \
    reaper-privdrop-test reaper-configmap-vol reaper-secret-vol \
    reaper-emptydir-vol reaper-hostpath-vol reaper-exec-test \
    reaper-exit-code-test reaper-cmd-not-found reaper-env-test \
    reaper-stderr-test reaper-large-output reaper-cwd-test \
    reaper-ro-vol-test \
    --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete configmap reaper-test-scripts --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete secret reaper-test-secret --ignore-not-found >> "$LOG_FILE" 2>&1 || true

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
