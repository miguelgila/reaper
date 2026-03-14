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
**Status:** Fixed
**Discovered:** 2026-03-14
**Fixed:** 2026-03-14

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

### Fix: PID-File Fallback

`inner_parent_persist()` now tries bind-mount first and gracefully handles
`EINVAL` by falling back to a `.pid` file that records the helper PID and
namespace inode (e.g., `/run/reaper/ns/default.pid` with content `12345 4026531840`).

- **Namespace creation**: writes `.pid` file, tries bind-mount. On `EINVAL`,
  removes the empty ns file and joins via `/proc/<pid>/ns/mnt` directly.
- **Subsequent workloads**: `namespace_exists()` checks both bind-mount path
  AND `.pid` file (helper alive + inode matches). `join_namespace()` tries
  bind-mount first, falls back to `/proc/<pid>/ns/mnt` via the `.pid` file.
- **PID reuse safety**: inode verification prevents joining a wrong namespace
  if the helper PID is recycled by the kernel.
- **GC**: `overlay_gc.rs` skips `.pid` files in ns scan, checks PID fallback
  liveness, and cleans up stale `.pid` files + kills stale helpers.

On real clusters (GKE, EKS, bare-metal), bind-mount succeeds as before and
the `.pid` file is written but unused for joining.

### Impact

- All Reaper workloads fail on Kind v1.35.0 (smoke test, integration tests)
- CI integration tests are blocked
- Does not affect real (non-Kind) Kubernetes clusters where nodes run
  directly on the host kernel (not inside Docker containers)

---

## Named Overlay Group Helper Killed by Process Group Signal

**Test:** `Named overlay group isolation (overlay-name)`
**Severity:** Medium (named overlay sharing broken for sequential pods)
**Status:** Open
**Discovered:** 2026-03-14

### Symptoms

The `reaper-ovname-same` reader pod (overlay-name=group-alpha) exits with
code 1 and empty logs. It cannot read the file written by the writer pod
that used the same overlay-name group.

### Root Cause

When a pod finishes and is cleaned up, `do_kill()` sends SIGKILL to the
daemon's process group. The namespace helper process (which sleeps forever
to anchor the mount namespace) was forked AFTER `setsid()`, placing it in
the same session and process group as the daemon. When `do_kill()` sends
`kill(-pgid, SIGKILL)`, both the daemon and the helper are killed.

With the helper dead, the mount namespace has no references (the bind-mount
may also be gone or the PID file points to a dead process). Subsequent pods
with the same overlay-name find no live namespace and create a new one —
losing the writer's files.

This does NOT affect pods running concurrently in the same overlay group
(the helper is alive while the daemon is running). It only affects
sequential pods where the first completes before the second starts.

### Fix Direction

The helper must survive the daemon's process group kill. Options:
1. Fork the helper BEFORE `setsid()` so it's in a different process group
2. Have the helper call `setpgid(0, 0)` to create its own process group
3. Move helper spawning to `create_namespace()` before the daemon fork

### Related

- The `Overlay filesystem sharing` test passes because both writer and
  reader use the default namespace overlay (no overlay-name), and
  apparently the helper survives or namespace is re-created identically.
- The `Combined annotations` test (which uses overlay-name) passes because
  it only tests a single pod, not cross-pod sharing.

---

## Shim Processes Exit After Pod Delete (Race)

**Test:** `Shim processes exit after pod delete`
**Severity:** Low (cosmetic — shim eventually exits, just not within test window)
**Status:** Open
**Discovered:** 2026-03-14

### Symptoms

After deleting a pod, the test checks for lingering shim processes and
finds one or more `containerd-shim-reaper-v2` processes still running.
The shim exits shortly after but not within the 5-second test window.

### Root Cause

Timing race between pod deletion, containerd's sandbox teardown, and
shim process cleanup. The shim waits for containerd to acknowledge the
exit event before shutting down. Under load (many pods created/deleted
during the test suite), this acknowledgement can be delayed.

### Workarounds

- Not a functional issue — the shim does exit, just slowly.
- Increasing the test timeout would make this pass reliably.

---

## Agent and Controller Image Pull Failures in Local CI

**Component:** Integration test infrastructure
**Severity:** Medium (blocks agent/controller/CRD tests in local CI)
**Status:** Open
**Discovered:** 2026-03-14

### Symptoms

All Phase 4a (agent) and Phase 4b (controller/CRD) tests fail. The
agent DaemonSet pod enters `ImagePullBackOff`:

```
failed to pull and unpack image "ghcr.io/miguelgila/reaper-agent:latest":
failed to authorize: failed to fetch anonymous token: unexpected status
from GET request to https://ghcr.io/token?...: 403 Forbidden
```

### Root Cause

The Helm chart references `ghcr.io/miguelgila/reaper-agent:latest` and
`ghcr.io/miguelgila/reaper-controller:latest`. In local Kind clusters,
these images must be pre-built and loaded via `kind load docker-image`.
The `setup-playground.sh` script builds and loads the node installer image
but the agent and controller images either fail to build or are not loaded
into the Kind cluster, causing the DaemonSet/Deployment to attempt a pull
from GHCR which requires authentication.

### Fix Direction

- Ensure `build-controller-image.sh` and agent image build are run during
  `setup-playground.sh` and images are loaded into Kind
- Or set `imagePullPolicy: IfNotPresent` in Helm values and pre-load images
