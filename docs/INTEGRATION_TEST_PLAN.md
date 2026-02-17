# Integration Test Plan

## Context

After analyzing the full codebase (runtime, shim, overlay, state), all 4 examples, existing 14 K8s tests, and 36 Rust unit/integration tests, these are the highest-value gaps ordered by likelihood of catching real bugs in production.

## Tier 1 — High Value (would catch real bugs)

### 1. Non-zero exit code propagation
Run `/bin/sh -c 'exit 42'` and verify the pod reaches `Failed` phase with `containerStatuses[0].state.terminated.exitCode == 42`. This is the most fundamental missing test — if exit codes are wrong, job controllers and CI pipelines break.

### 2. Command not found (Failed pod lifecycle)
Run `/nonexistent/binary` and verify the pod reaches `Failed` (not stuck in `Pending`/`Running`). Currently no test exercises the failure path where `Command::new().spawn()` fails inside the daemon.

### 3. Environment variable passing
Pod with `env: [{name: MY_VAR, value: "reaper-env-ok"}]`, command: `printenv MY_VAR`. Validates the OCI config env parsing (`split_once('=')`) works end-to-end. Currently zero tests verify env vars at the K8s level.

### 4. stderr capture
Pod that writes to stderr (`echo "stderr-ok" >&2`), then verify `kubectl logs` captures it. Current K8s tests only validate stdout. stderr uses a separate FIFO path and relay thread — a broken stderr relay would be invisible to existing tests.

### 5. Process group kill (children survive pod delete)
Pod running `sh -c "sleep 300 & sleep 300 & wait"`. Delete the pod, then check no orphaned `sleep` processes remain on the node. Tests that `kill(-pid, sig)` reaches child processes, not just the main process.

### 6. Concurrent pod starts (overlay lock contention)
Apply 3 pods simultaneously via a single `kubectl apply`. Verify all 3 reach `Succeeded`. Currently all tests create pods sequentially — the overlay `flock()` contention path is never exercised.

### 7. Exec exit code propagation
`kubectl exec <pod> -- /bin/sh -c 'exit 7'` and verify the exit code is 7 (via `$?`). The existing exec test only checks stdout output, not return codes.

### 8. Large output (FIFO buffer boundary)
Pod that generates >64KB of output (`seq 1 20000`). Verify `kubectl logs` captures the full output (first and last lines). Tests the FIFO 4096-byte relay buffer under pressure.

## Tier 2 — Medium Value (correctness edge cases)

### 9. Working directory (cwd)
Pod with `workingDir: /tmp`, command: `pwd`. Verify output is `/tmp`. The runtime parses `cwd` from config and calls `cmd.current_dir()` — never validated at K8s level.

### 10. Graceful shutdown (SIGTERM delivery)
Long-running pod with a trap handler (`trap 'echo SIGTERM-received; exit 0' TERM; sleep 300`). Delete the pod with default grace period. Verify `kubectl logs` contains "SIGTERM-received", confirming SIGTERM is delivered before SIGKILL.

### 11. Read-only volume write rejection
Secret volume mounted read-only at `/secrets`. Command attempts `touch /secrets/newfile`. Verify the command fails (non-zero exit) and the pod eventually reaches `Failed`. Example 04 tests this but the integration suite doesn't.

### 12. Exec on non-existent binary
`kubectl exec <pod> -- /nonexistent/binary` and verify it returns non-zero. Tests the exec spawn-failure path.

## Tier 3 — Lower Value (nice-to-have)

### 13. Phase 1 missing test suites
Add `integration_io` and `integration_exec` to the Phase 1 cargo tests. They're currently skipped in CI — an I/O regression would only be caught by K8s Phase 4 tests.

### 14. Rapid create/delete stress
Create and delete 5 pods in quick succession. Verify no zombies, no orphaned shims, no state file leaks on the node. Tests the cleanup paths under load.

## What we skip (and why)

- **PTY/interactive tests** (`kubectl run -it`): Hard to automate in CI (requires pseudo-terminal plumbing in the test runner)
- **Multi-node tests**: Requires multi-node Kind config change; low ROI for the current feature set
- **Stale overlay namespace recovery**: Destructive and hard to trigger deterministically
- **Unimplemented shim methods** (pause/resume/checkpoint/stats): They're documented no-ops; testing them adds little value
- **Monitoring daemon crash recovery**: Hard to trigger reliably, and the 1-hour timeout is by design
