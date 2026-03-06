# CRD Implementation Plan: ReaperPod

## Goal

Introduce a `ReaperPod` Custom Resource Definition (CRD) that provides a simplified,
Reaper-native way to run workloads on Kubernetes. A new `reaper-controller` binary
watches `ReaperPod` resources and creates real Pods with `runtimeClassName: reaper-v2`
pre-configured, stripping away container-centric fields that are meaningless for Reaper.

## Motivation

Every Reaper workload today requires boilerplate:
- `image: busybox` (pulled by kubelet, ignored by Reaper)
- `runtimeClassName: reaper-v2` (must remember to set)
- Fields like `resources`, `imagePullPolicy`, container probes are noise
- Users must know which Pod fields are relevant vs. ignored

A `ReaperPod` CRD makes the intent explicit and the spec minimal.

## Architecture Decision: Separate Binary

The controller runs as a **single-replica Deployment** (`reaper-controller`), separate
from the per-node `reaper-agent` DaemonSet. Rationale:

- CRD controllers are cluster-scoped singletons; the agent is per-node
- Separation of concerns: agent manages node-level state, controller manages K8s resources
- Standard pattern (every operator framework does this)
- Avoids leader-election complexity in the agent

## CRD Design

### API Group & Version

```
apiGroup: reaper.io
version: v1alpha1
```

### ReaperPod Spec

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: my-task
  namespace: default
spec:
  # Required: the command to run on the node
  command: ["/bin/sh", "-c", "echo hello"]

  # Optional fields
  args: ["--verbose"]
  env:
    - name: MY_VAR
      value: "hello"
    - name: SECRET_VAR
      secretKeyRef:
        name: my-secret
        key: password
    - name: CONFIG_VAR
      configMapKeyRef:
        name: my-config
        key: some-key
  workingDir: /tmp

  # Node targeting (at most one of these)
  nodeName: worker-1
  nodeSelector:
    role: compute

  # Reaper-specific annotations
  dnsMode: kubernetes          # maps to reaper.runtime/dns-mode
  overlayName: my-group        # maps to reaper.runtime/overlay-name

  # User identity
  runAsUser: 1000
  runAsGroup: 1000

  # Volumes (simplified: mountPath is inline, not in a separate volumeMounts array)
  volumes:
    - name: config
      mountPath: /config
      readOnly: true
      configMap: my-config           # ConfigMap name (string)
    - name: data
      mountPath: /data
      hostPath: /var/data            # Host path (string)
    - name: creds
      mountPath: /secrets
      readOnly: true
      secret: my-creds               # Secret name (string)
    - name: scratch
      mountPath: /scratch
      emptyDir: true                  # Boolean flag

  # Restart policy (default: Never for run-to-completion semantics)
  restartPolicy: Never

  # Tolerations (passed through to Pod)
  tolerations:
    - key: node-role.kubernetes.io/control-plane
      operator: Exists
      effect: NoSchedule
```

### ReaperPod Status

```yaml
status:
  phase: Running          # Pending | Running | Succeeded | Failed
  podName: my-task-xk2f9  # name of the created Pod
  nodeName: worker-1      # node where the Pod was scheduled
  startTime: "2026-03-06T10:00:00Z"
  completionTime: "2026-03-06T10:00:05Z"
  exitCode: 0
  conditions:
    - type: PodCreated
      status: "True"
      lastTransitionTime: "2026-03-06T10:00:00Z"
    - type: Complete
      status: "True"
      lastTransitionTime: "2026-03-06T10:00:05Z"
```

## Implementation Steps

### Step 1: Project scaffolding — DONE

Create the new binary and shared CRD types.

**Files created:**
- `src/lib.rs` — exposes `crds` module (feature-gated behind `controller`)
- `src/crds/mod.rs` — shared CRD type definitions
- `src/crds/reaper_pod.rs` — `ReaperPodSpec`, `ReaperPodStatus`, derive `CustomResource`
- `src/bin/reaper-controller/main.rs` — entry point, CLI args, controller setup
- `src/bin/reaper-controller/reconciler.rs` — reconcile loop
- `src/bin/reaper-controller/pod_builder.rs` — ReaperPod -> Pod translation

**Cargo.toml changes:**
- Added `schemars = { version = "0.8", optional = true }` dependency
- Added `controller` feature: `["kube", "k8s-openapi", "schemars", "futures", "chrono"]`
- Added `[[bin]]` entry for `reaper-controller` with `required-features = ["controller"]`

**Lesson learned:** k8s-openapi types don't implement `JsonSchema` without the `schemars`
feature on k8s-openapi itself. Instead of enabling that feature (which would affect the
whole crate), we defined our own simple CRD types and translate them to k8s-openapi types
in the pod_builder.

### Step 2: CRD type definitions — DONE

Defined Rust structs in `src/crds/reaper_pod.rs`:

- `ReaperPodSpec` with `#[derive(CustomResource, JsonSchema)]`
- Own types: `ReaperEnvVar`, `KeyRef`, `ReaperVolume`, `ReaperToleration`
- `ReaperPodStatus` with phase, podName, nodeName, exitCode, etc.
- `printcolumn` for Phase, Node, Exit Code, Age in `kubectl get reaperpods`
- All optional fields use `serde(default)` + `skip_serializing_if`

### Step 3: Pod builder (ReaperPod -> Pod translation) — DONE

Pure function in `src/bin/reaper-controller/pod_builder.rs` with 9 unit tests covering:
- Basic pod creation (runtimeClassName, restartPolicy, command, image, owner ref, labels)
- Reaper annotations (dnsMode -> `reaper.runtime/dns-mode`, overlayName -> `reaper.runtime/overlay-name`)
- Security context (runAsUser/runAsGroup)
- Volume translation (configMap name -> ConfigMapVolumeSource, readOnly, mountPath)
- Node targeting (nodeName passthrough)
- generateName (no fixed name, K8s generates suffix)
- Environment variables (literal values, secretKeyRef, configMapKeyRef)
- Tolerations

**Translation rules implemented:**
- `runtimeClassName: reaper-v2` (always)
- `image: "busybox:latest"` (fixed placeholder)
- `command` / `args` / `env` / `workingDir` mapped directly
- `volumes` split into Pod `volumes` + container `volumeMounts`
- `dnsMode` / `overlayName` -> pod annotations
- `runAsUser` / `runAsGroup` -> `securityContext`
- `nodeName` / `nodeSelector` / `tolerations` mapped directly
- `restartPolicy` defaulted to `Never`
- Owner reference set to the ReaperPod (for GC via `reaper.io/owner` label)

### Step 4: Reconciler — DONE

Using `kube::runtime::Controller` in `src/bin/reaper-controller/reconciler.rs`:

**Reconcile logic implemented:**
1. Fetch the `ReaperPod` resource
2. Check if owned Pod already exists (by label `reaper.io/owner=<name>`)
3. If no Pod exists -> build Pod, create it, update status to `Pending`
4. If Pod exists -> read its status, mirror phase/nodeName/exitCode back to ReaperPod status
5. Extract exit code + completion time from container terminated state
6. Re-check every 30s (also triggered by owned Pod changes)

**Watches:**
- Primary: `ReaperPod` resources (all namespaces)
- Secondary: `Pod` resources owned by ReaperPods (via `.owns()`)

### Step 5: Controller binary main.rs — DONE

- CLI args: `--generate-crds` flag to print CRD YAML and exit
- kube-rs client setup
- Graceful shutdown on ctrl-c/SIGTERM via `tokio::select!`
- Version string with GIT_HASH and BUILD_DATE (same pattern as agent)

### Step 6: Deployment manifests — DONE

**Files created:**
- `deploy/kubernetes/crds/reaperpods.reaper.io.yaml` — CRD definition (auto-generated from Rust types via `--generate-crds`)
- `deploy/kubernetes/reaper-controller.yaml` — ServiceAccount, ClusterRole, ClusterRoleBinding, Deployment

**RBAC permissions:**
- `reaperpods.reaper.io`: get, list, watch
- `reaperpods.reaper.io/status`: get, patch, update
- `pods`: get, list, watch, create, delete
- `events`: create, patch

**Key decisions:**
- Controller runs as nonroot (UID 65534) with readOnlyRootFilesystem
- Uses `reaper-system` namespace (shared with agent)
- `imagePullPolicy: IfNotPresent` for Kind compatibility

### Step 7: CRD generation script — DONE

`scripts/generate-crds.sh` — generates CRD YAML from Rust types:
- Runs `cargo run --features controller --bin reaper-controller -- --generate-crds`
- Converts JSON to YAML via Python
- Cleans up empty arrays from kube-rs derive
- Output: `deploy/kubernetes/crds/reaperpods.reaper.io.yaml`

### Step 8: Dockerfile & build script — DONE

- `Dockerfile.controller` — multi-stage: `rust-musl-cross` build → `distroless/static-debian12:nonroot`
- `scripts/build-controller-image.sh` — detects arch, builds image, optional `--load-kind` flag

### Step 9: Integration tests — DONE

**Unit tests (cargo test):**
- 9 tests in pod_builder covering all field combinations
- All pass, clippy clean

**Integration tests (Kind) — Phase 4b in `scripts/run-integration-tests.sh`:**
- `test_controller_crd_install` — CRD installation and establishment
- `test_controller_deployment` — Controller Deployment ready
- `test_controller_simple_reaperpod` — Simple ReaperPod creates Pod with runtimeClassName
- `test_controller_status_mirroring` — Phase, podName, nodeName mirrored to status
- `test_controller_exit_code` — Exit code 42 propagated to ReaperPod status
- `test_controller_reaperpod_annotations` — dnsMode/overlayName translated to pod annotations
- `test_controller_kubectl_get_columns` — Custom printer columns (Phase, Node, Exit Code)
- `test_controller_gc_on_delete` — Pod garbage collected when ReaperPod deleted

**Infrastructure:**
- `scripts/build-controller-image.sh` — matches agent pattern (--cluster-name, --skip-build, --quiet)
- Controller image built and loaded into Kind during Phase 2 setup
- `reaper-system` namespace created idempotently in `test_controller_deployment` (required for `--crd-only` runs that skip agent setup)

### Step 10: Examples — DONE

Created `examples/09-reaperpod/`:
- `simple-task.yaml` — run a command to completion
- `with-volumes.yaml` — ConfigMap + emptyDir volumes
- `with-node-selector.yaml` — node targeting + tolerations + dnsMode

## Dependency Changes

| Crate | Version | Feature | Purpose |
|---|---|---|---|
| `schemars` | `0.8` | — | JSON Schema generation for CRD (required by kube-rs derive) |
| `kube` | (existing) | `derive` | `#[derive(CustomResource)]` (already in deps) |
| `k8s-openapi` | (existing) | — | Pod types (already in deps) |

## File Tree (new files)

```
src/
  crds/
    mod.rs
    reaper_pod.rs
  bin/
    reaper-controller/
      main.rs
      reconciler.rs
      pod_builder.rs
deploy/
  kubernetes/
    crds/
      reaperpods.reaper.io.yaml
    reaper-controller.yaml
Dockerfile.controller
scripts/
  build-controller-image.sh
  generate-crds.sh
examples/
  09-reaperpod/
    simple-task.yaml
    with-volumes.yaml
    with-node-selector.yaml
```

## Open Questions

1. **Namespace scope vs. cluster scope?** — Namespaced (like Pods). Users create ReaperPods in their namespace.
2. **Finalizers?** — Probably not needed; owner references handle Pod cleanup. May need one if we add status cleanup logic.
3. **Events?** — Record Kubernetes Events on the ReaperPod (e.g., "Created Pod my-task-xk2f9"). Nice for `kubectl describe`.
4. **Validation webhook?** — Not for v1alpha1. Use CRD structural schema validation (OpenAPI v3) from kube-rs derive.
5. **Leader election?** — For v1alpha1, single replica is fine. Add lease-based leader election later for HA.

## Future: ReaperDaemonJob (Phase 2, not in this plan)

Once ReaperPod works, ReaperDaemonJob builds on it:
- Controller watches Nodes + ReaperDaemonJobs
- Creates one ReaperPod (or Pod) per matching node
- Tracks per-node completion in status
- Handles new node joins automatically
- Optional `rerunPolicy: OnNodeReboot`
