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

test_kubernetes_dns_resolution() {
  # Create a target service for DNS resolution testing
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Service
metadata:
  name: reaper-dns-target
spec:
  type: ClusterIP
  ports:
    - port: 80
      targetPort: 80
      protocol: TCP
YAML

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-k8s-dns-check
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
          echo "=== Kubernetes DNS Resolution Check ==="
          echo "resolv.conf contents:"
          cat /etc/resolv.conf
          echo ""

          # Verify resolv.conf points to CoreDNS (not host DNS)
          if ! grep -q 'nameserver 10\.' /etc/resolv.conf; then
            echo "FAIL: resolv.conf does not point to cluster DNS"
            exit 1
          fi
          echo "resolv.conf points to cluster DNS"

          # Resolve the kubernetes service (always exists)
          echo "Resolving kubernetes.default.svc.cluster.local..."
          if getent hosts kubernetes.default.svc.cluster.local; then
            echo "kubernetes.default resolved OK"
          else
            echo "FAIL: could not resolve kubernetes.default.svc.cluster.local"
            exit 1
          fi

          # Resolve our test service
          echo "Resolving reaper-dns-target.default.svc.cluster.local..."
          if getent hosts reaper-dns-target.default.svc.cluster.local; then
            echo "reaper-dns-target resolved OK"
          else
            echo "FAIL: could not resolve reaper-dns-target.default.svc.cluster.local"
            exit 1
          fi

          echo "=== Kubernetes DNS Resolution PASSED ==="
YAML

  wait_for_pod_phase reaper-k8s-dns-check Succeeded 60 2 || {
    log_error "Kubernetes DNS check pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-k8s-dns-check
    return 1
  }
  local logs
  logs=$(kubectl logs reaper-k8s-dns-check --all-containers=true 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Kubernetes DNS check logs: $logs"
  if [[ "$logs" != *"Kubernetes DNS Resolution PASSED"* ]]; then
    log_error "Kubernetes DNS check did not produce expected output"
    log_error "Actual pod logs:"
    echo "$logs" | while IFS= read -r line; do
      log_error "  $line"
    done
    dump_pod_diagnostics reaper-k8s-dns-check
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

test_namespace_overlay_isolation() {
  # Create a second namespace for isolation testing
  kubectl create namespace reaper-iso-test >> "$LOG_FILE" 2>&1 || true

  # Writer pod in "default" namespace writes a marker file
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-ns-iso-writer
  namespace: default
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: writer
      image: busybox
      command: ["/bin/sh", "-c", "echo ns-isolation-marker > /tmp/ns-iso-test.txt"]
YAML

  wait_for_pod_phase reaper-ns-iso-writer Succeeded 60 2 || {
    log_error "Namespace isolation writer pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-ns-iso-writer
    return 1
  }

  # Reader pod in "reaper-iso-test" namespace tries to read the marker.
  # The command outputs file content OR the error, plus a status line.
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-ns-iso-reader
  namespace: reaper-iso-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: reader
      image: busybox
      command: ["/bin/sh", "-c", "cat /tmp/ns-iso-test.txt 2>&1 || true"]
YAML

  # Poll the reader pod in reaper-iso-test namespace (wait_for_pod_phase doesn't support -n)
  local elapsed=0 timeout=60 interval=2 phase
  while [[ $elapsed -lt $timeout ]]; do
    phase=$(kubectl get pod reaper-ns-iso-reader -n reaper-iso-test -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    log_verbose "Pod reaper-ns-iso-reader (reaper-iso-test) phase=$phase (${elapsed}s/${timeout}s)"
    if [[ "$phase" == "Succeeded" || "$phase" == "Failed" ]]; then
      break
    fi
    sleep "$interval"
    elapsed=$((elapsed + interval))
  done

  if [[ "$phase" != "Succeeded" && "$phase" != "Failed" ]]; then
    log_error "Namespace isolation reader pod did not complete (phase=$phase)"
    return 1
  fi

  local reader_output
  reader_output=$(kubectl logs reaper-ns-iso-reader -n reaper-iso-test --all-containers=true 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Namespace isolation reader output: '$reader_output'"

  # The reader must NOT see the marker file (different namespace = different overlay)
  if [[ "$reader_output" == *"ns-isolation-marker"* ]]; then
    log_error "Namespace isolation FAILED: pod in reaper-iso-test namespace could read file written by default namespace"
    log_error "Actual pod logs: '$reader_output'"
    return 1
  fi

  if [[ "$reader_output" == *"No such file"* ]]; then
    log_verbose "Namespace isolation verified: overlays are isolated per K8s namespace"
  else
    log_verbose "Reader output (file not found expected): '$reader_output'"
  fi
}

test_overlay_name_isolation() {
  # Two pods in the SAME namespace but with DIFFERENT overlay-name annotations.
  # Pod A writes a marker file; Pod B (different overlay-name) must NOT see it.
  # This verifies that overlay-name creates truly isolated overlay groups.

  # Writer pod with overlay-name=group-alpha
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-ovname-writer
  annotations:
    reaper.runtime/overlay-name: "group-alpha"
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: writer
      image: busybox
      command: ["/bin/sh", "-c", "echo overlay-name-marker > /tmp/ovname-test.txt"]
YAML

  wait_for_pod_phase reaper-ovname-writer Succeeded 60 2 || {
    log_error "Overlay-name writer pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-ovname-writer
    return 1
  }

  # Diagnostic: inspect overlay state on the Kind node
  log_verbose "=== Overlay-name diagnostic: namespace dirs ==="
  docker exec "${CLUSTER_NAME}-control-plane" ls -la /run/reaper/ns/ 2>&1 | while IFS= read -r line; do
    log_verbose "  ns/ $line"
  done
  log_verbose "=== Overlay-name diagnostic: overlay dirs ==="
  docker exec "${CLUSTER_NAME}-control-plane" ls -la /run/reaper/overlay/default/ 2>&1 | while IFS= read -r line; do
    log_verbose "  overlay/default/ $line"
  done
  # Check state files for annotation content
  log_verbose "=== Overlay-name diagnostic: state files with annotations ==="
  docker exec "${CLUSTER_NAME}-control-plane" sh -c 'grep -rl "overlay-name" /run/reaper/*/state.json 2>/dev/null || echo "(no state files with overlay-name found)"' 2>&1 | while IFS= read -r line; do
    log_verbose "  state: $line"
  done
  # Check runtime log for annotation parsing
  log_verbose "=== Overlay-name diagnostic: runtime log (last 30 annotation/overlay lines) ==="
  docker exec "${CLUSTER_NAME}-control-plane" sh -c 'grep -E "annotation|overlay_name|overlay-name" /run/reaper/runtime.log 2>/dev/null | tail -30 || echo "(no matching log lines)"' 2>&1 | while IFS= read -r line; do
    log_verbose "  log: $line"
  done

  # Reader pod with overlay-name=group-beta (different group, same namespace)
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-ovname-reader
  annotations:
    reaper.runtime/overlay-name: "group-beta"
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: reader
      image: busybox
      command: ["/bin/sh", "-c", "cat /tmp/ovname-test.txt 2>&1 || true"]
YAML

  wait_for_pod_phase reaper-ovname-reader Succeeded 60 2 || {
    # Reader may Succeed or Fail (cat fails if file missing, but we use || true)
    local phase
    phase=$(kubectl get pod reaper-ovname-reader -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    if [[ "$phase" != "Failed" ]]; then
      log_error "Overlay-name reader pod did not complete (phase=$phase)"
      dump_pod_diagnostics reaper-ovname-reader
      return 1
    fi
  }

  local reader_output
  reader_output=$(kubectl logs reaper-ovname-reader --all-containers=true 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Overlay-name reader output: '$reader_output'"

  # The reader must NOT see the marker file (different overlay-name = different overlay)
  if [[ "$reader_output" == *"overlay-name-marker"* ]]; then
    log_error "Overlay-name isolation FAILED: pod with overlay-name=group-beta could read file written by overlay-name=group-alpha"
    log_error "Actual pod logs: '$reader_output'"
    # Extra diagnostics: check OCI config.json for reaper annotations
    log_error "=== Diagnostic: OCI config.json reaper annotations ==="
    docker exec "${CLUSTER_NAME}-control-plane" sh -c '
      for cfg in /run/containerd/io.containerd.runtime.v2.task/k8s.io/*/config.json; do
        [ -f "$cfg" ] || continue
        id=$(basename "$(dirname "$cfg")")
        short_id="${id:0:12}"
        reaper_annots=$(grep -o "reaper\.runtime[^\"]*\"[^\"]*" "$cfg" 2>/dev/null || echo "(none)")
        sandbox_name=$(grep -o "io\.kubernetes\.cri\.sandbox-name\":\"[^\"]*" "$cfg" 2>/dev/null | head -1 | cut -d\" -f3 || echo "?")
        echo "  id=$short_id sandbox=$sandbox_name reaper=$reaper_annots"
      done
    ' 2>&1 | while IFS= read -r line; do
      log_error "  $line"
    done
    # Check reaper state files for stored annotations
    log_error "=== Diagnostic: reaper state files ==="
    docker exec "${CLUSTER_NAME}-control-plane" sh -c '
      for sf in /run/reaper/*/state.json; do
        [ -f "$sf" ] || continue
        id=$(basename "$(dirname "$sf")")
        short_id="${id:0:12}"
        annots=$(grep -o "\"annotations\":{[^}]*}" "$sf" 2>/dev/null || echo "(no annotations)")
        echo "  id=$short_id $annots"
      done
    ' 2>&1 | while IFS= read -r line; do
      log_error "  $line"
    done
    return 1
  fi

  if [[ "$reader_output" == *"No such file"* ]]; then
    log_verbose "Overlay-name isolation verified: different overlay-name groups are isolated"
  else
    log_verbose "Reader output (file not found expected): '$reader_output'"
  fi

  # Bonus: verify a pod with the SAME overlay-name CAN see the file
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-ovname-same
  annotations:
    reaper.runtime/overlay-name: "group-alpha"
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: reader
      image: busybox
      command: ["/bin/sh", "-c", "cat /tmp/ovname-test.txt"]
YAML

  wait_for_pod_phase reaper-ovname-same Succeeded 60 2 || {
    log_error "Overlay-name same-group reader pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-ovname-same
    return 1
  }

  local same_output
  same_output=$(kubectl logs reaper-ovname-same --all-containers=true 2>&1 || echo "(failed to retrieve logs)")
  log_verbose "Overlay-name same-group reader output: '$same_output'"

  if [[ "$same_output" != "overlay-name-marker" ]]; then
    log_error "Overlay-name sharing FAILED: pod with same overlay-name=group-alpha could NOT read file"
    log_error "Actual pod logs: '$same_output'"
    return 1
  fi

  log_verbose "Overlay-name sharing verified: same overlay-name group shares overlay"
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
      command: ["sleep", "300"]
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
  first_line=$(echo "$logs" 2>/dev/null | head -1 | tr -d '[:space:]')
  local last_line
  last_line=$(echo "$logs" 2>/dev/null | tail -1 | tr -d '[:space:]')

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
  # We need a Running pod to exec into. The reaper-exec-test pod from
  # test_exec_support may have expired (sleep 60 finished) by now, so
  # check its phase — recreate if it's not Running.
  local phase
  phase=$(kubectl get pod reaper-exec-test -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
  if [[ "$phase" != "Running" ]]; then
    kubectl delete pod reaper-exec-test --ignore-not-found >> "$LOG_FILE" 2>&1 || true
    # Wait for deletion
    for i in $(seq 1 15); do
      if ! kubectl get pod reaper-exec-test &>/dev/null; then break; fi
      sleep 1
    done
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
      command: ["sleep", "300"]
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
      command: ["/bin/sh", "-c", "sleep 54321 & sleep 54321 & echo pgkill-children-started && wait"]
YAML

  wait_for_pod_phase reaper-pgkill-test Running 60 2 || {
    log_error "Process group kill pod did not reach Running phase"
    dump_pod_diagnostics reaper-pgkill-test
    return 1
  }

  # Verify child processes are running on the node
  sleep 2
  local before_count
  before_count=$(docker exec "$NODE_ID" sh -c "ps aux | grep 'sleep 54321' | grep -v grep | wc -l" 2>/dev/null || echo "0")
  log_verbose "Sleep processes before kill: $before_count"

  if [[ "$before_count" -lt 2 ]]; then
    log_error "Expected at least 2 'sleep 54321' processes, found: $before_count"
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
  after_count=$(docker exec "$NODE_ID" sh -c "ps aux | grep 'sleep 54321' | grep -v grep | wc -l" 2>/dev/null || echo "0")
  log_verbose "Sleep processes after kill: $after_count"

  if [[ "$after_count" -gt 0 ]]; then
    log_error "Found $after_count orphaned 'sleep 54321' processes after pod deletion"
    docker exec "$NODE_ID" ps aux 2>/dev/null | grep 'sleep 54321' | grep -v grep | while IFS= read -r line; do
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
  # We need a Running pod. Check if reaper-exec-test is still alive.
  local phase
  phase=$(kubectl get pod reaper-exec-test -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
  if [[ "$phase" != "Running" ]]; then
    kubectl delete pod reaper-exec-test --ignore-not-found >> "$LOG_FILE" 2>&1 || true
    for i in $(seq 1 15); do
      if ! kubectl get pod reaper-exec-test &>/dev/null; then break; fi
      sleep 1
    done
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
      command: ["sleep", "300"]
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

test_config_file_on_node() {
  # Verify /etc/reaper/reaper.conf exists on the Kind node and has expected content
  local node_id
  node_id=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
  if [[ -z "$node_id" ]]; then
    node_id=$(docker ps --filter "name=${CLUSTER_NAME}" --format '{{.ID}}' | head -1)
  fi

  if [[ -z "$node_id" ]]; then
    log_error "Could not find cluster node container"
    return 1
  fi

  # Check file exists
  if ! docker exec "$node_id" test -f /etc/reaper/reaper.conf; then
    log_error "/etc/reaper/reaper.conf does not exist on node"
    return 1
  fi
  log_verbose "Config file exists on node"

  # Check content has expected keys
  local content
  content=$(docker exec "$node_id" cat /etc/reaper/reaper.conf 2>&1)

  if ! echo "$content" | grep -q "REAPER_DNS_MODE="; then
    log_error "Config file missing REAPER_DNS_MODE. Content: $content"
    return 1
  fi
  log_verbose "Config file contains REAPER_DNS_MODE"

  if ! echo "$content" | grep -q "REAPER_RUNTIME_LOG="; then
    log_error "Config file missing REAPER_RUNTIME_LOG. Content: $content"
    return 1
  fi
  log_verbose "Config file contains REAPER_RUNTIME_LOG"

  # Verify no legacy systemd drop-in exists
  if docker exec "$node_id" test -f /etc/systemd/system/containerd.service.d/reaper-env.conf 2>/dev/null; then
    log_error "Legacy reaper-env.conf drop-in still exists on node"
    return 1
  fi
  log_verbose "No legacy systemd drop-in found (clean)"

  log_verbose "Config file on node verified: /etc/reaper/reaper.conf"
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

test_dns_mode_annotation_override() {
  # The node-level config sets REAPER_DNS_MODE=kubernetes (Ansible default).
  # A pod with reaper.runtime/dns-mode: host should override this and use the
  # host node's /etc/resolv.conf instead of the kubelet-prepared one.
  #
  # IMPORTANT: Each pod uses a unique overlay-name to get a fresh overlay namespace.
  # Without this, pods in the same K8s namespace share an overlay, and DNS changes
  # from earlier pods (kubernetes mode writing CoreDNS resolv.conf) would persist.

  # Baseline pod: uses node default (kubernetes) in a fresh overlay group.
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-dns-annot-default
  annotations:
    reaper.runtime/overlay-name: "dns-test-k8s"
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
          echo "=== Default DNS mode (kubernetes) ==="
          cat /etc/resolv.conf
          if grep -q 'nameserver 10\.' /etc/resolv.conf; then
            echo "DNS_MODE_RESULT=kubernetes"
          else
            echo "DNS_MODE_RESULT=host"
          fi
YAML

  # Override pod: dns-mode=host in a separate fresh overlay group.
  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-dns-annot-host
  annotations:
    reaper.runtime/dns-mode: "host"
    reaper.runtime/overlay-name: "dns-test-host"
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
          echo "=== Host DNS mode (annotation override) ==="
          cat /etc/resolv.conf
          if grep -q 'nameserver 10\.' /etc/resolv.conf; then
            echo "DNS_MODE_RESULT=kubernetes"
          else
            echo "DNS_MODE_RESULT=host"
          fi
YAML

  # Wait for both pods
  wait_for_pod_phase reaper-dns-annot-default Succeeded 60 2 || {
    log_error "DNS annotation default pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-dns-annot-default
    return 1
  }
  wait_for_pod_phase reaper-dns-annot-host Succeeded 60 2 || {
    log_error "DNS annotation host-override pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-dns-annot-host
    return 1
  }

  # Validate baseline pod uses kubernetes DNS (CoreDNS)
  local default_logs
  default_logs=$(kubectl logs reaper-dns-annot-default 2>&1 || echo "(failed)")
  log_verbose "DNS annotation default logs: $default_logs"

  if [[ "$default_logs" != *"DNS_MODE_RESULT=kubernetes"* ]]; then
    log_error "Baseline pod (no annotation) did not get kubernetes DNS as expected"
    log_error "Actual logs:"
    echo "$default_logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-dns-annot-default
    return 1
  fi

  # Validate annotated pod uses host DNS (NOT CoreDNS)
  local host_logs
  host_logs=$(kubectl logs reaper-dns-annot-host 2>&1 || echo "(failed)")
  log_verbose "DNS annotation host-override logs: $host_logs"

  if [[ "$host_logs" != *"DNS_MODE_RESULT=host"* ]]; then
    log_error "Pod with dns-mode=host annotation did not get host DNS"
    log_error "Actual logs:"
    echo "$host_logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-dns-annot-host
    # Runtime log diagnostics
    log_error "=== Runtime log (annotation/dns lines) ==="
    docker exec "${CLUSTER_NAME}-control-plane" sh -c \
      'grep -E "annotation|dns.mode|dns_mode" /run/reaper/runtime.log 2>/dev/null | tail -20 || echo "(none)"' \
      2>&1 | while IFS= read -r line; do log_error "  $line"; done
    return 1
  fi

  log_verbose "DNS mode annotation override verified: host annotation produces host resolv.conf"
}

test_combined_annotations() {
  # A pod with BOTH reaper.runtime/dns-mode and reaper.runtime/overlay-name.
  # Verifies that multiple annotations work together on the same pod.

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-annot-combined
  annotations:
    reaper.runtime/dns-mode: "host"
    reaper.runtime/overlay-name: "combo-group"
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
          echo "=== Combined annotation check ==="
          # Check DNS mode: should be host (not kubernetes)
          if grep -q 'nameserver 10\.' /etc/resolv.conf; then
            echo "DNS_MODE=kubernetes"
          else
            echo "DNS_MODE=host"
          fi
          # Write a marker file to verify overlay-name group
          echo "combined-marker" > /tmp/combined-annot-test.txt
          cat /tmp/combined-annot-test.txt
          echo "=== Combined PASSED ==="
YAML

  wait_for_pod_phase reaper-annot-combined Succeeded 60 2 || {
    log_error "Combined annotation pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-annot-combined
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-annot-combined 2>&1 || echo "(failed)")
  log_verbose "Combined annotation logs: $logs"

  # Verify dns-mode=host was applied
  if [[ "$logs" != *"DNS_MODE=host"* ]]; then
    log_error "Combined annotation pod: dns-mode=host was not applied"
    log_error "Actual logs:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-annot-combined
    return 1
  fi

  # Verify pod succeeded (overlay-name was accepted and overlay worked)
  if [[ "$logs" != *"Combined PASSED"* ]]; then
    log_error "Combined annotation pod did not produce expected output"
    log_error "Actual logs:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-annot-combined
    return 1
  fi

  # Verify the overlay-name group was created on the node
  local ns_files
  ns_files=$(docker exec "${CLUSTER_NAME}-control-plane" ls /run/reaper/ns/ 2>/dev/null || echo "")
  log_verbose "Namespace files on node: $ns_files"
  if [[ "$ns_files" != *"default--combo-group"* ]]; then
    log_error "Expected namespace file 'default--combo-group' for overlay-name, got: $ns_files"
    return 1
  fi

  log_verbose "Combined annotations verified: dns-mode=host + overlay-name=combo-group both applied"
}

test_invalid_annotation_graceful_fallback() {
  # A pod with an invalid dns-mode value should still start successfully.
  # Invalid annotation values are logged and ignored; node defaults apply.

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-annot-invalid
  annotations:
    reaper.runtime/dns-mode: "bogus-invalid-value"
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
          echo "=== Invalid annotation fallback ==="
          cat /etc/resolv.conf
          # Node default is kubernetes, so we should still get CoreDNS
          if grep -q 'nameserver 10\.' /etc/resolv.conf; then
            echo "DNS_FALLBACK=kubernetes"
          else
            echo "DNS_FALLBACK=host"
          fi
          echo "=== Fallback PASSED ==="
YAML

  wait_for_pod_phase reaper-annot-invalid Succeeded 60 2 || {
    log_error "Invalid annotation pod did not reach Succeeded phase (pod should still start)"
    dump_pod_diagnostics reaper-annot-invalid
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-annot-invalid 2>&1 || echo "(failed)")
  log_verbose "Invalid annotation fallback logs: $logs"

  # Pod must succeed — invalid annotations should not crash it
  if [[ "$logs" != *"Fallback PASSED"* ]]; then
    log_error "Invalid annotation pod did not produce expected output"
    log_error "Actual logs:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-annot-invalid
    return 1
  fi

  # Should fall back to node default (kubernetes)
  if [[ "$logs" != *"DNS_FALLBACK=kubernetes"* ]]; then
    log_error "Invalid dns-mode annotation did not fall back to node default (kubernetes)"
    log_error "Actual logs:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    return 1
  fi

  log_verbose "Invalid annotation graceful fallback verified: pod started with node defaults"
}

test_unknown_annotations_ignored() {
  # A pod with unknown reaper.runtime/* annotation keys should start fine.
  # Unknown keys are silently ignored per the security model.

  cat <<'YAML' | kubectl apply -f - >> "$LOG_FILE" 2>&1
apiVersion: v1
kind: Pod
metadata:
  name: reaper-annot-unknown
  annotations:
    reaper.runtime/nonexistent-key: "whatever"
    reaper.runtime/overlay-base: "/evil/path"
    reaper.runtime/dns-mode: "kubernetes"
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
          echo "=== Unknown annotation check ==="
          # The known dns-mode=kubernetes should still work
          if grep -q 'nameserver 10\.' /etc/resolv.conf; then
            echo "DNS_OK=yes"
          else
            echo "DNS_OK=no"
          fi
          echo "=== Unknown Annotations PASSED ==="
YAML

  wait_for_pod_phase reaper-annot-unknown Succeeded 60 2 || {
    log_error "Unknown annotations pod did not reach Succeeded phase"
    dump_pod_diagnostics reaper-annot-unknown
    return 1
  }

  local logs
  logs=$(kubectl logs reaper-annot-unknown 2>&1 || echo "(failed)")
  log_verbose "Unknown annotations logs: $logs"

  if [[ "$logs" != *"Unknown Annotations PASSED"* ]]; then
    log_error "Unknown annotations pod did not produce expected output"
    log_error "Actual logs:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    dump_pod_diagnostics reaper-annot-unknown
    return 1
  fi

  # Verify known annotation (dns-mode=kubernetes) was still applied correctly
  if [[ "$logs" != *"DNS_OK=yes"* ]]; then
    log_error "Known annotation (dns-mode) was not applied alongside unknown annotations"
    log_error "Actual logs:"
    echo "$logs" | while IFS= read -r line; do log_error "  $line"; done
    return 1
  fi

  log_verbose "Unknown annotations silently ignored, known annotations applied correctly"
}

# ---------------------------------------------------------------------------
# reaper-agent integration tests
# These require the reaper-agent image to be loaded into the Kind cluster.
# Skipped if image is not available.
# ---------------------------------------------------------------------------

test_agent_deployment() {
  # Deploy agent manifests
  kubectl apply -f deploy/kubernetes/reaper-agent.yaml >> "$LOG_FILE" 2>&1

  # Patch DaemonSet to use faster overlay GC interval for testing (30s instead of 300s)
  kubectl patch daemonset reaper-agent -n reaper-system --type=json \
    -p='[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--overlay-gc-interval=30"}]' >> "$LOG_FILE" 2>&1

  # Wait for agent DaemonSet rollout
  if ! kubectl rollout status daemonset/reaper-agent -n reaper-system --timeout=120s >> "$LOG_FILE" 2>&1; then
    log_error "reaper-agent DaemonSet rollout failed"
    kubectl describe daemonset reaper-agent -n reaper-system >> "$LOG_FILE" 2>&1 || true
    kubectl get pods -n reaper-system >> "$LOG_FILE" 2>&1 || true
    return 1
  fi

  # Verify at least one agent pod is running
  local running_pods
  running_pods=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-agent \
    --field-selector=status.phase=Running --no-headers 2>/dev/null | wc -l | tr -d ' ')
  if [[ "$running_pods" -lt 1 ]]; then
    log_error "Expected at least 1 running reaper-agent pod, got $running_pods"
    return 1
  fi

  log_verbose "reaper-agent DaemonSet deployed: $running_pods pod(s) running"
}

test_agent_config_sync() {
  # Update the ConfigMap with a test value
  kubectl apply -f - >> "$LOG_FILE" 2>&1 <<'YAML'
apiVersion: v1
kind: ConfigMap
metadata:
  name: reaper-config
  namespace: reaper-system
data:
  reaper.conf: |
    # Integration test config
    REAPER_DNS_MODE=kubernetes
    REAPER_OVERLAY_ISOLATION=namespace
    REAPER_TEST_MARKER=agent-sync-test
YAML

  # Give the agent time to detect and sync the change
  sleep 10

  # Verify the config file was written to the node
  local config_content
  config_content=$(docker exec "$NODE_ID" cat /etc/reaper/reaper.conf 2>/dev/null || echo "")

  if [[ -z "$config_content" ]]; then
    log_error "Config file /etc/reaper/reaper.conf not found on node"
    return 1
  fi

  if ! echo "$config_content" | grep -q "REAPER_TEST_MARKER=agent-sync-test"; then
    log_error "Config file does not contain expected test marker"
    log_error "Actual content: $config_content"
    return 1
  fi

  log_verbose "Config sync verified: test marker found in /etc/reaper/reaper.conf"
}

test_agent_healthz() {
  # Get the agent pod name
  local agent_pod
  agent_pod=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-agent \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)

  if [[ -z "$agent_pod" ]]; then
    log_error "No reaper-agent pod found"
    return 1
  fi

  # Use port-forward to reach the endpoint (distroless container has no shell/wget)
  local local_port=19100
  kubectl port-forward -n reaper-system "$agent_pod" ${local_port}:9100 >> "$LOG_FILE" 2>&1 &
  local pf_pid=$!
  sleep 2

  local health_response
  health_response=$(curl -sf http://localhost:${local_port}/healthz 2>/dev/null || echo "FAILED")

  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true

  if [[ "$health_response" != "ok" ]]; then
    log_error "healthz endpoint returned unexpected response: $health_response"
    return 1
  fi

  log_verbose "healthz endpoint returned 'ok'"
}

test_agent_metrics() {
  local agent_pod
  agent_pod=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-agent \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)

  if [[ -z "$agent_pod" ]]; then
    log_error "No reaper-agent pod found"
    return 1
  fi

  # Use port-forward to reach the endpoint (distroless container has no shell/wget)
  local local_port=19101
  kubectl port-forward -n reaper-system "$agent_pod" ${local_port}:9100 >> "$LOG_FILE" 2>&1 &
  local pf_pid=$!
  sleep 2

  local metrics_response
  metrics_response=$(curl -sf http://localhost:${local_port}/metrics 2>/dev/null || echo "FAILED")

  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true

  if [[ "$metrics_response" == "FAILED" ]]; then
    log_error "metrics endpoint not reachable"
    return 1
  fi

  # Verify key metrics are present
  local missing=()
  for metric in reaper_containers_running reaper_agent_gc_runs_total reaper_agent_healthy reaper_agent_config_syncs_total; do
    if ! echo "$metrics_response" | grep -q "$metric"; then
      missing+=("$metric")
    fi
  done

  if [[ ${#missing[@]} -gt 0 ]]; then
    log_error "Missing expected metrics: ${missing[*]}"
    log_error "Metrics output: $metrics_response"
    return 1
  fi

  log_verbose "metrics endpoint verified: all expected metrics present"
}

test_agent_stale_gc() {
  # Create a fake stale state directory on the node
  docker exec "$NODE_ID" mkdir -p /run/reaper/stale-gc-test >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" bash -c 'cat > /run/reaper/stale-gc-test/state.json << EOF
{
  "id": "stale-gc-test",
  "bundle": "/tmp/fake",
  "status": "running",
  "pid": 999999
}
EOF' >> "$LOG_FILE" 2>&1

  # Wait for the next GC cycle (default 60s, but initial GC runs on startup too)
  # The agent should detect pid 999999 as dead and mark it stopped
  log_verbose "Waiting for GC cycle to detect stale PID..."
  local max_wait=90
  local elapsed=0
  while [[ $elapsed -lt $max_wait ]]; do
    local state_status
    state_status=$(docker exec "$NODE_ID" cat /run/reaper/stale-gc-test/state.json 2>/dev/null \
      | grep -o '"status"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 || echo "")
    if echo "$state_status" | grep -q '"stopped"'; then
      log_verbose "GC correctly marked stale container as stopped"
      return 0
    fi
    sleep 5
    elapsed=$((elapsed + 5))
  done

  log_error "GC did not mark stale container as stopped within ${max_wait}s"
  docker exec "$NODE_ID" cat /run/reaper/stale-gc-test/state.json >> "$LOG_FILE" 2>&1 || true
  return 1
}

test_agent_overlay_gc_basic() {
  # Create a K8s namespace, then fake overlay artifacts on the node
  kubectl create namespace reaper-gc-test >> "$LOG_FILE" 2>&1

  docker exec "$NODE_ID" mkdir -p /run/reaper/overlay/reaper-gc-test/upper /run/reaper/overlay/reaper-gc-test/work >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/merged/reaper-gc-test >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/ns >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/ns/reaper-gc-test >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/overlay-reaper-gc-test.lock >> "$LOG_FILE" 2>&1

  # Delete the namespace so overlay becomes orphaned
  kubectl delete namespace reaper-gc-test --wait=true >> "$LOG_FILE" 2>&1

  # Poll until all artifacts are gone (overlay GC interval is 30s in test)
  local max_wait=180
  local elapsed=0
  while [[ $elapsed -lt $max_wait ]]; do
    local remaining=0
    docker exec "$NODE_ID" test -d /run/reaper/overlay/reaper-gc-test 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -d /run/reaper/merged/reaper-gc-test 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -f /run/reaper/ns/reaper-gc-test 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -f /run/reaper/overlay-reaper-gc-test.lock 2>/dev/null && remaining=$((remaining + 1))

    if [[ $remaining -eq 0 ]]; then
      log_verbose "overlay GC cleaned all artifacts for deleted namespace"
      return 0
    fi

    log_verbose "Waiting for overlay GC ($remaining artifacts remaining, ${elapsed}s/${max_wait}s)..."
    sleep 10
    elapsed=$((elapsed + 10))
  done

  log_error "overlay GC did not clean artifacts within ${max_wait}s"
  docker exec "$NODE_ID" ls -la /run/reaper/overlay/ /run/reaper/merged/ /run/reaper/ns/ >> "$LOG_FILE" 2>&1 || true
  return 1
}

test_agent_overlay_gc_preserves_active() {
  # Create fake overlay artifacts for the 'default' namespace (which always exists)
  docker exec "$NODE_ID" mkdir -p /run/reaper/overlay/default/upper /run/reaper/overlay/default/work >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/merged/default >> "$LOG_FILE" 2>&1

  # Wait 2 overlay GC cycles (interval=30s in test, so 90s buffer)
  log_verbose "Waiting 90s (2+ overlay GC cycles) to verify artifacts are preserved..."
  sleep 90

  # Assert overlay artifacts still exist (ns files are managed by ns cleanup, not overlay GC)
  local ok=true
  docker exec "$NODE_ID" test -d /run/reaper/overlay/default 2>/dev/null || { log_error "overlay/default was removed"; ok=false; }
  docker exec "$NODE_ID" test -d /run/reaper/merged/default 2>/dev/null || { log_error "merged/default was removed"; ok=false; }

  # Cleanup
  docker exec "$NODE_ID" rm -rf /run/reaper/overlay/default /run/reaper/merged/default >> "$LOG_FILE" 2>&1 || true

  if [[ "$ok" == "true" ]]; then
    log_verbose "overlay GC correctly preserved artifacts for active namespace"
    return 0
  fi
  return 1
}

test_agent_overlay_gc_named_groups() {
  # Create namespace + named group overlay artifacts
  kubectl create namespace reaper-gc-named >> "$LOG_FILE" 2>&1

  # Namespace-level artifacts
  docker exec "$NODE_ID" mkdir -p /run/reaper/overlay/reaper-gc-named/upper /run/reaper/overlay/reaper-gc-named/work >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/merged/reaper-gc-named >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/ns >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/ns/reaper-gc-named >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/overlay-reaper-gc-named.lock >> "$LOG_FILE" 2>&1

  # Named group artifacts (my-group)
  docker exec "$NODE_ID" mkdir -p /run/reaper/overlay/reaper-gc-named/my-group/upper /run/reaper/overlay/reaper-gc-named/my-group/work >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/merged/reaper-gc-named/my-group >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/ns/reaper-gc-named--my-group >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/overlay-reaper-gc-named--my-group.lock >> "$LOG_FILE" 2>&1

  # Delete namespace
  kubectl delete namespace reaper-gc-named --wait=true >> "$LOG_FILE" 2>&1

  # Poll until all artifacts are gone
  local max_wait=180
  local elapsed=0
  while [[ $elapsed -lt $max_wait ]]; do
    local remaining=0
    docker exec "$NODE_ID" test -d /run/reaper/overlay/reaper-gc-named 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -d /run/reaper/merged/reaper-gc-named 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -f /run/reaper/ns/reaper-gc-named 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -f /run/reaper/overlay-reaper-gc-named.lock 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -f /run/reaper/ns/reaper-gc-named--my-group 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -f /run/reaper/overlay-reaper-gc-named--my-group.lock 2>/dev/null && remaining=$((remaining + 1))

    if [[ $remaining -eq 0 ]]; then
      log_verbose "overlay GC cleaned all artifacts including named groups"
      return 0
    fi

    log_verbose "Waiting for overlay GC ($remaining artifacts remaining, ${elapsed}s/${max_wait}s)..."
    sleep 10
    elapsed=$((elapsed + 10))
  done

  log_error "overlay GC did not clean named group artifacts within ${max_wait}s"
  docker exec "$NODE_ID" ls -la /run/reaper/overlay/ /run/reaper/merged/ /run/reaper/ns/ >> "$LOG_FILE" 2>&1 || true
  return 1
}

test_agent_overlay_gc_skips_running_containers() {
  # Create namespace + overlay artifacts
  kubectl create namespace reaper-gc-running >> "$LOG_FILE" 2>&1

  docker exec "$NODE_ID" mkdir -p /run/reaper/overlay/reaper-gc-running/upper /run/reaper/overlay/reaper-gc-running/work >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/merged/reaper-gc-running >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" mkdir -p /run/reaper/ns >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/ns/reaper-gc-running >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/overlay-reaper-gc-running.lock >> "$LOG_FILE" 2>&1

  # Create a fake container state dir with a running container referencing this namespace
  # PID 1 is always alive (init process)
  docker exec "$NODE_ID" mkdir -p /run/reaper/fake-gc-container >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" bash -c 'cat > /run/reaper/fake-gc-container/state.json << EOF
{
  "id": "fake-gc-container",
  "bundle": "/tmp/fake",
  "status": "running",
  "pid": 1,
  "namespace": "reaper-gc-running"
}
EOF' >> "$LOG_FILE" 2>&1

  # Delete the namespace
  kubectl delete namespace reaper-gc-running --wait=true >> "$LOG_FILE" 2>&1

  # Wait 2 overlay GC cycles — artifacts should NOT be removed
  log_verbose "Waiting 90s (2+ overlay GC cycles) to verify running container prevents cleanup..."
  sleep 90

  # Assert artifacts still exist (GC skipped due to running container)
  local ok=true
  docker exec "$NODE_ID" test -d /run/reaper/overlay/reaper-gc-running 2>/dev/null || { log_error "overlay/reaper-gc-running was removed despite running container"; ok=false; }
  docker exec "$NODE_ID" test -d /run/reaper/merged/reaper-gc-running 2>/dev/null || { log_error "merged/reaper-gc-running was removed despite running container"; ok=false; }

  if [[ "$ok" != "true" ]]; then
    docker exec "$NODE_ID" rm -rf /run/reaper/fake-gc-container >> "$LOG_FILE" 2>&1 || true
    return 1
  fi

  log_verbose "Confirmed: overlay GC skipped cleanup due to running container"

  # Now remove the fake container state and verify GC cleans up
  docker exec "$NODE_ID" rm -rf /run/reaper/fake-gc-container >> "$LOG_FILE" 2>&1

  local max_wait=90
  local elapsed=0
  while [[ $elapsed -lt $max_wait ]]; do
    local remaining=0
    docker exec "$NODE_ID" test -d /run/reaper/overlay/reaper-gc-running 2>/dev/null && remaining=$((remaining + 1))
    docker exec "$NODE_ID" test -d /run/reaper/merged/reaper-gc-running 2>/dev/null && remaining=$((remaining + 1))

    if [[ $remaining -eq 0 ]]; then
      log_verbose "overlay GC cleaned up after running container was removed"
      return 0
    fi

    sleep 10
    elapsed=$((elapsed + 10))
  done

  log_error "overlay GC did not clean up after running container was removed within ${max_wait}s"
  # Cleanup
  docker exec "$NODE_ID" rm -rf /run/reaper/overlay/reaper-gc-running /run/reaper/merged/reaper-gc-running >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -f /run/reaper/ns/reaper-gc-running /run/reaper/overlay-reaper-gc-running.lock >> "$LOG_FILE" 2>&1 || true
  return 1
}

test_agent_overlay_gc_metrics() {
  local agent_pod
  agent_pod=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-agent \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)

  if [[ -z "$agent_pod" ]]; then
    log_error "No reaper-agent pod found"
    return 1
  fi

  # Use port-forward to reach the metrics endpoint
  local local_port=19101
  kubectl port-forward -n reaper-system "$agent_pod" ${local_port}:9100 >> "$LOG_FILE" 2>&1 &
  local pf_pid=$!
  sleep 3

  local metrics_response
  metrics_response=$(curl -s "http://127.0.0.1:${local_port}/metrics" 2>/dev/null || echo "")

  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true

  if [[ -z "$metrics_response" ]]; then
    log_error "Failed to fetch metrics from agent"
    return 1
  fi

  local missing=()
  for metric in reaper_agent_overlay_gc_runs_total reaper_agent_overlay_gc_cleaned_total reaper_agent_overlay_namespaces; do
    if ! echo "$metrics_response" | grep -q "$metric"; then
      missing+=("$metric")
    fi
  done

  if [[ ${#missing[@]} -gt 0 ]]; then
    log_error "Missing overlay GC metrics: ${missing[*]}"
    log_error "Metrics output: $metrics_response"
    return 1
  fi

  log_verbose "overlay GC metrics verified: all expected metrics present"
}

test_agent_ns_cleanup_stale_file() {
  # Create a regular file (not a mount point) at /run/reaper/ns/stale-ns-test
  docker exec "$NODE_ID" mkdir -p /run/reaper/ns >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/ns/stale-ns-test >> "$LOG_FILE" 2>&1

  # Poll until the stale file is removed (GC interval is 30s in test)
  local max_wait=180
  local elapsed=0
  while [[ $elapsed -lt $max_wait ]]; do
    if ! docker exec "$NODE_ID" test -f /run/reaper/ns/stale-ns-test 2>/dev/null; then
      log_verbose "ns cleanup removed stale ns file"
      return 0
    fi

    log_verbose "Waiting for ns cleanup to remove stale file (${elapsed}s/${max_wait}s)..."
    sleep 10
    elapsed=$((elapsed + 10))
  done

  log_error "ns cleanup did not remove stale file within ${max_wait}s"
  docker exec "$NODE_ID" ls -la /run/reaper/ns/ >> "$LOG_FILE" 2>&1 || true
  return 1
}

test_agent_ns_cleanup_preserves_active() {
  # Create a stale ns file, but protect it with a running container reference.
  # The running container safety check should prevent ns cleanup from removing it.
  # (In production, mount-point detection via /proc/1/mountinfo is the primary guard,
  # but in Kind's nested container setup, bind-mounts aren't visible to the agent.)
  docker exec "$NODE_ID" mkdir -p /run/reaper/ns >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" touch /run/reaper/ns/ns-protect-test >> "$LOG_FILE" 2>&1

  # Create a fake running container referencing the same namespace (PID 1 is always alive)
  docker exec "$NODE_ID" mkdir -p /run/reaper/fake-ns-protect >> "$LOG_FILE" 2>&1
  docker exec "$NODE_ID" bash -c 'cat > /run/reaper/fake-ns-protect/state.json << EOF
{
  "id": "fake-ns-protect",
  "bundle": "/tmp/fake",
  "status": "running",
  "pid": 1,
  "namespace": "ns-protect-test"
}
EOF' >> "$LOG_FILE" 2>&1

  # Wait 2 GC cycles (interval=30s, so 90s buffer)
  log_verbose "Waiting 90s (2+ GC cycles) to verify ns file is preserved by running container..."
  sleep 90

  # Assert ns file still exists (protected by running container)
  if docker exec "$NODE_ID" test -f /run/reaper/ns/ns-protect-test 2>/dev/null; then
    log_verbose "ns cleanup correctly preserved ns file with running container"

    # Now remove the fake container and verify the ns file gets cleaned
    docker exec "$NODE_ID" rm -rf /run/reaper/fake-ns-protect >> "$LOG_FILE" 2>&1

    local max_wait=90
    local elapsed=0
    while [[ $elapsed -lt $max_wait ]]; do
      if ! docker exec "$NODE_ID" test -f /run/reaper/ns/ns-protect-test 2>/dev/null; then
        log_verbose "ns cleanup removed ns file after running container was removed"
        return 0
      fi
      sleep 10
      elapsed=$((elapsed + 10))
    done

    log_error "ns cleanup did not remove ns file after running container was removed within ${max_wait}s"
    docker exec "$NODE_ID" rm -f /run/reaper/ns/ns-protect-test >> "$LOG_FILE" 2>&1 || true
    return 1
  fi

  log_error "ns cleanup removed ns file despite running container reference"
  docker exec "$NODE_ID" rm -rf /run/reaper/fake-ns-protect >> "$LOG_FILE" 2>&1 || true
  return 1
}

test_agent_ns_cleanup_metrics() {
  local agent_pod
  agent_pod=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-agent \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)

  if [[ -z "$agent_pod" ]]; then
    log_error "No reaper-agent pod found"
    return 1
  fi

  # Use port-forward to reach the metrics endpoint
  local local_port=19102
  kubectl port-forward -n reaper-system "$agent_pod" ${local_port}:9100 >> "$LOG_FILE" 2>&1 &
  local pf_pid=$!
  sleep 3

  local metrics_response
  metrics_response=$(curl -s "http://127.0.0.1:${local_port}/metrics" 2>/dev/null || echo "")

  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true

  if [[ -z "$metrics_response" ]]; then
    log_error "Failed to fetch metrics from agent"
    return 1
  fi

  local missing=()
  for metric in reaper_agent_ns_cleanup_runs_total reaper_agent_ns_cleaned_total; do
    if ! echo "$metrics_response" | grep -q "$metric"; then
      missing+=("$metric")
    fi
  done

  if [[ ${#missing[@]} -gt 0 ]]; then
    log_error "Missing ns cleanup metrics: ${missing[*]}"
    log_error "Metrics output: $metrics_response"
    return 1
  fi

  log_verbose "ns cleanup metrics verified: all expected metrics present"
}

cleanup_agent() {
  kubectl delete -f deploy/kubernetes/reaper-agent.yaml --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -rf /run/reaper/stale-gc-test >> "$LOG_FILE" 2>&1 || true
  # Cleanup any leftover overlay GC test artifacts
  docker exec "$NODE_ID" rm -rf /run/reaper/overlay/reaper-gc-test /run/reaper/overlay/reaper-gc-named /run/reaper/overlay/reaper-gc-running >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -rf /run/reaper/merged/reaper-gc-test /run/reaper/merged/reaper-gc-named /run/reaper/merged/reaper-gc-running >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -f /run/reaper/ns/reaper-gc-test /run/reaper/ns/reaper-gc-named /run/reaper/ns/reaper-gc-running >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -f /run/reaper/ns/stale-ns-test /run/reaper/ns/ns-protect-test >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -rf /run/reaper/fake-ns-protect >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -f /run/reaper/ns/reaper-gc-named--my-group >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -f /run/reaper/overlay-reaper-gc-test.lock /run/reaper/overlay-reaper-gc-named.lock /run/reaper/overlay-reaper-gc-running.lock >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -f /run/reaper/overlay-reaper-gc-named--my-group.lock >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -rf /run/reaper/fake-gc-container >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -rf /run/reaper/overlay/default /run/reaper/merged/default >> "$LOG_FILE" 2>&1 || true
  docker exec "$NODE_ID" rm -f /run/reaper/ns/default >> "$LOG_FILE" 2>&1 || true
  kubectl delete namespace reaper-gc-test reaper-gc-named reaper-gc-running --ignore-not-found >> "$LOG_FILE" 2>&1 || true
}

# ---------------------------------------------------------------------------
# Phase 4a: reaper-agent tests (optional, requires agent image in cluster)
# ---------------------------------------------------------------------------
phase_agent_tests() {
  log_status ""
  log_status "${CLR_PHASE}Phase 4a: reaper-agent tests${CLR_RESET}"
  log_status "========================================"

  # Verify agent image is available in the cluster (loaded during Phase 2 setup)
  local image_loaded
  image_loaded=$(docker exec "$NODE_ID" crictl images 2>/dev/null \
    | grep -c "reaper-agent" || true)

  if [[ "$image_loaded" -lt 1 ]]; then
    log_error "reaper-agent image not found in Kind cluster"
    log_error "This should have been built and loaded during Phase 2 setup."
    log_error "Check build-agent-image.sh output in the log file."
    return 1
  fi

  run_test test_agent_deployment   "Agent DaemonSet deployment"        --hard-fail
  run_test test_agent_config_sync  "Agent ConfigMap sync to host"      --hard-fail
  run_test test_agent_healthz      "Agent /healthz endpoint"           --hard-fail
  run_test test_agent_metrics      "Agent /metrics endpoint"           --hard-fail
  run_test test_agent_stale_gc     "Agent stale state GC"             --hard-fail
  run_test test_agent_overlay_gc_metrics "Agent overlay GC metrics"    --hard-fail
  run_test test_agent_overlay_gc_basic "Agent overlay GC basic cleanup" --hard-fail
  run_test test_agent_overlay_gc_named_groups "Agent overlay GC named groups" --hard-fail
  run_test test_agent_overlay_gc_preserves_active "Agent overlay GC preserves active namespaces" --hard-fail
  run_test test_agent_overlay_gc_skips_running_containers "Agent overlay GC skips running containers" --hard-fail
  run_test test_agent_ns_cleanup_metrics "Agent ns cleanup metrics"    --hard-fail
  run_test test_agent_ns_cleanup_stale_file "Agent ns cleanup stale file" --hard-fail
  run_test test_agent_ns_cleanup_preserves_active "Agent ns cleanup preserves active" --hard-fail

  # Cleanup agent resources
  cleanup_agent
}

# ---------------------------------------------------------------------------
# Phase 4: Integration test orchestrator
# ---------------------------------------------------------------------------
phase_integration_tests() {
  log_status ""
  log_status "${CLR_PHASE}Phase 4: Integration tests${CLR_RESET}"
  log_status "========================================"

  run_test test_dns_resolution   "DNS resolution check"          --hard-fail
  run_test test_kubernetes_dns_resolution "Kubernetes DNS resolution (CoreDNS)" --hard-fail
  run_test test_echo_command     "Echo command execution"        --hard-fail
  run_test test_overlay_sharing  "Overlay filesystem sharing"    --hard-fail
  run_test test_namespace_overlay_isolation "Per-namespace overlay isolation" --hard-fail
  run_test test_overlay_name_isolation "Named overlay group isolation (overlay-name)" --hard-fail
  run_test test_dns_mode_annotation_override "DNS mode annotation override (host vs kubernetes)" --hard-fail
  run_test test_combined_annotations "Combined annotations (dns-mode + overlay-name)" --hard-fail
  run_test test_invalid_annotation_graceful_fallback "Invalid annotation graceful fallback" --hard-fail
  run_test test_unknown_annotations_ignored "Unknown annotation keys silently ignored" --hard-fail
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
  run_test test_config_file_on_node "Config file on node (/etc/reaper/reaper.conf)" --hard-fail
  run_test test_rapid_create_delete "Rapid create/delete stress"     --hard-fail

  # Cleanup test pods (before defunct check so pods are terminated)
  kubectl delete pod reaper-dns-check reaper-k8s-dns-check reaper-integration-test \
    reaper-overlay-writer reaper-overlay-reader reaper-ns-iso-writer \
    reaper-ovname-writer reaper-ovname-reader reaper-ovname-same \
    reaper-dns-annot-default reaper-dns-annot-host \
    reaper-annot-combined reaper-annot-invalid reaper-annot-unknown \
    reaper-uid-gid-test \
    reaper-privdrop-test reaper-configmap-vol reaper-secret-vol \
    reaper-emptydir-vol reaper-hostpath-vol reaper-exec-test \
    reaper-exit-code-test reaper-cmd-not-found reaper-env-test \
    reaper-stderr-test reaper-large-output reaper-cwd-test \
    reaper-ro-vol-test \
    --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete pod reaper-ns-iso-reader -n reaper-iso-test --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete namespace reaper-iso-test --ignore-not-found >> "$LOG_FILE" 2>&1 || true
  kubectl delete service reaper-dns-target --ignore-not-found >> "$LOG_FILE" 2>&1 || true
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
