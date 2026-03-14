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

---

## Overlay Namespace Bind-Mount EINVAL on Kind v1.35.0

**Component:** `reaper-runtime` overlay (`src/bin/reaper-runtime/overlay.rs`)
**Severity:** High (blocks all Reaper workloads on affected nodes)
**Status:** Open
**Discovered:** 2026-03-14

### Symptoms

All Reaper pods fail immediately with exit code 1 and empty logs. The
runtime log shows:

```
ERROR reaper_runtime: do_start() - overlay setup failed: failed to create
shared namespace: bind-mounting namespace: EINVAL: Invalid argument,
refusing to run without isolation
```

### Root Cause

The overlay namespace persistence code in `inner_parent_persist()` bind-mounts
`/proc/<helper_pid>/ns/mnt` to a file under `/run/reaper/ns/`. This `mount(MS_BIND)`
call returns `EINVAL` inside Kind v1.35.0 nodes (`kindest/node:v1.35.0`).

Kind nodes are Docker containers, and newer kernels or containerd 2.x restrict
namespace bind-mounts from within nested containers. The `EINVAL` indicates the
mount is rejected by the kernel's mount namespace security checks.

Containerd also logs a secondary warning:
```
failed to load runtime info: exit status 1 (stderr: "io.containerd.reaper.v2: Env(NotPresent)")
```

### Impact

- All Reaper workloads fail on Kind v1.35.0 (smoke test, integration tests)
- CI integration tests are blocked
- Does not affect real (non-Kind) Kubernetes clusters where nodes run
  directly on the host kernel (not inside Docker containers)

### Workarounds

- Pin Kind to v1.34.x or earlier (last known working version)
- Test on real Kubernetes clusters (GKE, EKS) where the host kernel
  allows namespace bind-mounts

### Investigation Notes

- The overlay code has not changed between `main` and the failing branch
- The bind-mount target file is created successfully; only the `mount()` fails
- The same code works on older Kind versions with containerd 1.7.x
- Need to verify: is the issue in the kernel version, containerd 2.x sandbox
  restrictions, or Docker's seccomp/apparmor profile for Kind containers?
