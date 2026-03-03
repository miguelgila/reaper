# Known Bugs and Flaky Tests

## DNS Mode Annotation Override Test Flake

**Test:** `DNS mode annotation override (host vs kubernetes)`
**Severity:** Low (intermittent, CI-only)
**Status:** Open

### Symptoms

The test times out (64s) waiting for the `reaper-dns-annot-default` or
`reaper-dns-annot-host` pod to reach `Succeeded` phase. The pod gets stuck
and containerd reports:

```
failed to stop sandbox: task must be stopped before deletion: running: failed precondition
```

### Root Cause

A timing race in containerd's sandbox lifecycle. When the shim reports the
container has exited, containerd sometimes tries to delete the task before
it has fully transitioned out of the `running` state. This causes a
`failed precondition` error that prevents sandbox teardown, leaving the pod
stuck.

This is a containerd-level issue, not a Reaper bug. It tends to surface
under load (e.g., when many pods are created/deleted in quick succession
during the integration test suite).

### Workarounds

- Re-running the test suite usually passes on retry.
- The `--agent-only` flag skips this test entirely for fast agent iteration.
- Running with `--no-cleanup` and re-running `--skip-cargo --no-cleanup`
  often avoids the race since the cluster is warmer.

### Related

- Observed in Kind clusters with containerd v1.7+.
- The `Combined annotations` test exercises similar annotation logic and
  passes reliably, suggesting the issue is timing-related rather than
  functional.
