# ReaperOverlay CRD — Implementation Plan

**Issue:** [#42](https://github.com/miguelgila/reaper/issues/42) — No mechanism to reset/clean overlay namespaces without direct node access

**Branch:** `fix/overlay-cleanup-crd`

## Concept

**ReaperOverlay** is to Reaper what **PVC** is to standard Kubernetes storage. It decouples
overlay lifecycle from pod lifecycle, giving users a Kubernetes-native way to create, inspect,
reset, and delete overlay filesystems.

- `ReaperOverlay` objects are **namespace-scoped** (matching overlay namespace isolation)
- `metadata.name` maps directly to the overlay name (no indirection)
- ReaperPods that reference an `overlayName` **block** (stay Pending) until a matching
  `ReaperOverlay` exists and is Ready — just like Pods with unbound PVCs
- Deleting a `ReaperOverlay` triggers on-disk cleanup on all nodes via a finalizer

## CRD Design

```yaml
apiVersion: reaper.giar.dev/v1alpha1   # see #46 for planned migration to reaper.giar.dev
kind: ReaperOverlay
metadata:
  name: slurm              # = overlay-name
  namespace: default        # = K8s namespace for overlay isolation
spec:
  resetPolicy: Manual       # Manual (default) | OnFailure | OnDelete
  resetGeneration: 0        # Increment to trigger a reset on all nodes
status:
  phase: Ready              # Pending | Ready | Resetting | Failed
  observedResetGeneration: 0
  nodes:
    - nodeName: worker-1
      ready: true
      lastResetTime: "2026-03-19T10:00:00Z"
    - nodeName: worker-2
      ready: true
      lastResetTime: "2026-03-19T10:00:00Z"
  message: ""
```

### Print Columns

| Name | JSONPath | Type |
|------|----------|------|
| Phase | `.status.phase` | string |
| Reset Gen | `.spec.resetGeneration` | integer |
| Observed | `.status.observedResetGeneration` | integer |
| Age | `.metadata.creationTimestamp` | date |

### Spec Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `resetPolicy` | enum | `Manual` | When to auto-reset: `Manual` (only on generation bump), `OnFailure` (when a ReaperPod using this overlay fails), `OnDelete` (when the ReaperOverlay is deleted and recreated) |
| `resetGeneration` | int | `0` | Monotonically increasing counter. Increment to trigger a reset. Controller compares against `status.observedResetGeneration` |

### Status Fields

| Field | Type | Description |
|-------|------|-------------|
| `phase` | enum | `Pending` (no nodes ready), `Ready` (at least one node ready), `Resetting` (reset in progress), `Failed` (reset failed) |
| `observedResetGeneration` | int | Last `resetGeneration` that was fully applied |
| `nodes[]` | array | Per-node overlay state |
| `nodes[].nodeName` | string | Node name |
| `nodes[].ready` | bool | Whether overlay is available on this node |
| `nodes[].lastResetTime` | string | ISO 8601 timestamp of last reset |
| `message` | string | Human-readable status message |

## PVC-like Behavior

### ReaperPod blocks until overlay is Ready

When a `ReaperPod` specifies `overlayName: "slurm"`, the controller:

1. Looks up `ReaperOverlay` named `slurm` in the same namespace
2. If not found or phase != `Ready`: sets `ReaperPod.status.phase = Pending` with
   message `"Waiting for ReaperOverlay 'slurm' to be Ready"`
3. If found and Ready: proceeds to create the Pod as normal
4. Requeues the ReaperPod for reconciliation (watches ReaperOverlay changes)

### Deletion triggers cleanup (finalizer)

When a `ReaperOverlay` is deleted:

1. Finalizer `reaper.giar.dev/overlay-cleanup` prevents immediate deletion
2. Controller calls agent on each node to tear down the overlay
3. Agent kills helper process, unmounts namespace, removes overlay dirs
4. Once all nodes confirm cleanup, controller removes finalizer → object is deleted

### Reset via generation counter

```bash
# Trigger a reset
kubectl patch reaperoverlays slurm -n default --type merge \
  -p '{"spec":{"resetGeneration":1}}'

# Watch progress
kubectl get reaperoverlays slurm -n default -w
```

Controller detects `spec.resetGeneration > status.observedResetGeneration`, sets
`status.phase = Resetting`, calls agent reset on all nodes, then updates
`status.observedResetGeneration` and sets phase back to `Ready`.

## Architecture

```
                        ┌──────────────────────┐
  kubectl apply ──────► │  reaper-controller   │
  ReaperOverlay         │  (cluster singleton) │
                        └──────┬───────────────┘
                               │
                    watches ReaperOverlay CRD
                    calls agent HTTP API for reset/cleanup
                    updates status from agent responses
                               │
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
     ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
     │ reaper-agent │ │ reaper-agent │ │ reaper-agent │
     │  (node-1)    │ │  (node-2)    │ │  (node-3)    │
     └──────────────┘ └──────────────┘ └──────────────┘
           │                │                │
     overlay dirs      overlay dirs     overlay dirs
     /run/reaper/      /run/reaper/     /run/reaper/
```

### Controller-to-Agent Communication

**v1 (this implementation): Direct HTTP**

Controller discovers agent pods via label selector (`app.kubernetes.io/component: agent`),
then calls the agent's HTTP API on each node's pod IP directly.

- Simple, fast, works within the cluster
- Controller already has a K8s client to list agent pods

**Future consideration: Annotation-based (Option B)**

Controller sets annotations on agent DaemonSet pods (e.g.,
`reaper.giar.dev/reset-overlay: "<ns>/<name>"`). Agent watches its own pod annotations
and acts on them. More decoupled but slower and more complex. Could be useful if
agent pods are not directly reachable from the controller (e.g., network policies).

## Implementation Steps

### Step 1: CRD Types (`src/crds/reaper_overlay.rs`)

New file following the same pattern as `reaper_pod.rs`:

- `ReaperOverlaySpec`: `resetPolicy`, `resetGeneration`
- `ReaperOverlayStatus`: `phase`, `observedResetGeneration`, `nodes[]`, `message`
- `ReaperOverlayNodeStatus`: `nodeName`, `ready`, `lastResetTime`
- Derive: `CustomResource`, `JsonSchema`, `Serialize`, `Deserialize`, `Clone`, `Debug`
- Same API group (`reaper.giar.dev`) and version (`v1alpha1`)
- Export from `src/crds/mod.rs`

### Step 2: CRD Generation

- Add `ReaperOverlay` to `--generate-crds` in `src/bin/reaper-controller/main.rs`
- Update `scripts/generate-crds.sh` to generate both CRDs
- Output: `deploy/helm/reaper/crds/reaperoverlays.reaper.giar.dev.yaml`
- Output: `deploy/kubernetes/crds/reaperoverlays.reaper.giar.dev.yaml`

### Step 3: Agent Overlay Reset Endpoint

New HTTP endpoints in `src/bin/reaper-agent/`:

**`DELETE /api/v1/overlays/{namespace}/{name}`** — Reset/destroy a named overlay:
1. Check no running containers reference this overlay (refuse with 409 Conflict if active)
2. Kill helper process (read PID from `ns/<ns>--<name>.pid`)
3. Unmount namespace bind-mount (`ns/<ns>--<name>`)
4. Remove overlay dirs (`overlay/<ns>/<name>/`, `merged/<ns>/<name>/`)
5. Remove lock file (`overlay-<ns>--<name>.lock`)
6. Return 200 OK or error

**`GET /api/v1/overlays`** — List overlays on this node:
- Scan `/run/reaper/ns/` and `/run/reaper/overlay/` for existing overlays
- Return array of `{ namespace, name, ready, helperPid }`

Extract the cleanup logic from existing `overlay_gc.rs` into a reusable function
that both the GC loop and the new endpoint can call.

### Step 4: Controller Overlay Reconciler (`src/bin/reaper-controller/overlay_reconciler.rs`)

New reconciler registered alongside the existing ReaperPod reconciler:

**Watches:** `ReaperOverlay` objects (all namespaces)

**Reconciliation logic:**

1. **Ensure finalizer** `reaper.giar.dev/overlay-cleanup` is present
2. **If being deleted** (deletionTimestamp set):
   - Call `DELETE /api/v1/overlays/{ns}/{name}` on all agent pods
   - Remove finalizer when all nodes confirm cleanup
3. **If `spec.resetGeneration > status.observedResetGeneration`:**
   - Set `status.phase = Resetting`
   - Call `DELETE /api/v1/overlays/{ns}/{name}` on all agent pods
   - On success: update `status.observedResetGeneration`, set `status.phase = Ready`
   - On failure: set `status.phase = Failed` with message
4. **Status update:**
   - Call `GET /api/v1/overlays` on each agent pod
   - Update `status.nodes[]` with per-node state
   - Set `status.phase = Ready` if at least one node is ready (overlay is lazily created)

### Step 5: ReaperPod Reconciler Changes (`src/bin/reaper-controller/reconciler.rs`)

Modify existing reconciler to enforce PVC-like blocking:

- When `ReaperPod.spec.overlayName` is set:
  1. Look up `ReaperOverlay` with that name in the same namespace
  2. If not found: set status `Pending` with message, requeue
  3. If found but phase != `Ready`: set status `Pending` with message, requeue
  4. If found and `Ready`: proceed to create Pod as normal
- Add `watches(ReaperOverlay)` to the controller builder so ReaperPods are
  re-reconciled when their referenced overlay changes state

### Step 6: Helm Chart Updates

- Add `deploy/helm/reaper/crds/reaperoverlays.reaper.giar.dev.yaml`
- Update `deploy/helm/reaper/templates/controller-rbac.yaml`:
  - Add `reaperoverlays` to the ClusterRole (get, list, watch, create, update, patch, delete)
  - Add `reaperoverlays/status` (get, patch, update)
- No changes to agent RBAC or DaemonSet

### Step 7: Integration Tests

Add to `scripts/lib/test-integration-suite.sh`:

1. **ReaperOverlay lifecycle**: Create overlay → verify status → delete → verify cleanup
2. **ReaperPod blocking**: Create ReaperPod with overlayName but no ReaperOverlay → verify Pending → create ReaperOverlay → verify Pod starts
3. **Reset**: Create overlay → run workload → reset overlay → verify overlay is clean
4. **Backward compat**: ReaperPod without overlayName still works (no ReaperOverlay needed)
5. **Finalizer cleanup**: Delete ReaperOverlay with overlay on disk → verify on-disk cleanup

## Files Changed

| File | Change |
|------|--------|
| `src/crds/mod.rs` | Export `reaper_overlay` module |
| `src/crds/reaper_overlay.rs` | **New** — CRD types |
| `src/bin/reaper-controller/main.rs` | Register overlay reconciler, add to `--generate-crds` |
| `src/bin/reaper-controller/overlay_reconciler.rs` | **New** — reconciliation logic |
| `src/bin/reaper-controller/reconciler.rs` | Add PVC-like blocking for overlayName |
| `src/bin/reaper-agent/main.rs` | Register new routes |
| `src/bin/reaper-agent/overlay_gc.rs` | Extract cleanup into reusable function |
| `deploy/helm/reaper/crds/reaperoverlays.reaper.giar.dev.yaml` | **New** — generated CRD |
| `deploy/kubernetes/crds/reaperoverlays.reaper.giar.dev.yaml` | **New** — generated CRD |
| `deploy/helm/reaper/templates/controller-rbac.yaml` | Add reaperoverlays permissions |
| `scripts/generate-crds.sh` | Generate both CRDs |
| `scripts/lib/test-integration-suite.sh` | New integration tests |

## What Does NOT Change

- **reaper-runtime** — No changes. Continues to lazily create/join overlays.
- **Pod annotation mechanism** — `reaper.runtime/overlay-name` unchanged.
- **Existing overlay GC loops in agent** — Unchanged (handle orphans, not user-initiated resets).
- **Shim** — No changes.

## Example Usage

```yaml
# 1. Create the overlay (like creating a PVC)
apiVersion: reaper.giar.dev/v1alpha1
kind: ReaperOverlay
metadata:
  name: slurm
  namespace: default
spec:
  resetPolicy: Manual

---
# 2. Use the overlay in a ReaperPod (like referencing a PVC)
apiVersion: reaper.giar.dev/v1alpha1
kind: ReaperPod
metadata:
  name: install-slurm
  namespace: default
spec:
  overlayName: slurm
  command: ["bash", "-c", "apt-get update && apt-get install -y slurm-wlm"]

---
# 3. Reset after corruption
# kubectl patch reaperoverlays slurm --type merge -p '{"spec":{"resetGeneration":1}}'
```
