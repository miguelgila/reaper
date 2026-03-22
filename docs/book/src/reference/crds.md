# Custom Resource Definitions

Reaper provides three CRDs for managing workloads, overlay filesystems, and node-wide configuration tasks.

## ReaperPod

A simplified, Reaper-native way to run workloads without standard container boilerplate.

- **Group:** `reaper.io`
- **Version:** `v1alpha1`
- **Kind:** `ReaperPod`
- **Short name:** `rpod` (`kubectl get rpod`)

### Spec

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `command` | `string[]` | Yes | Command to execute on the host |
| `args` | `string[]` | No | Arguments to the command |
| `env` | `EnvVar[]` | No | Environment variables (simplified format) |
| `volumes` | `Volume[]` | No | Volume mounts (simplified format) |
| `nodeSelector` | `map[string]string` | No | Node selection constraints |
| `dnsMode` | `string` | No | DNS resolution mode (`host` or `kubernetes`) |
| `overlayName` | `string` | No | Named overlay group (requires matching ReaperOverlay) |

### Status

| Field | Type | Description |
|-------|------|-------------|
| `phase` | `string` | Current phase: `Pending`, `Running`, `Succeeded`, `Failed` |
| `podName` | `string` | Name of the backing Pod |
| `nodeName` | `string` | Node where the workload runs |
| `exitCode` | `int` | Process exit code (when completed) |
| `startTime` | `string` | When the workload started |
| `completionTime` | `string` | When the workload completed |

### Simplified Volumes

ReaperPod volumes use a flat format instead of the nested Kubernetes volume spec:

```yaml
volumes:
  - name: config
    mountPath: /etc/config
    configMap: "my-configmap"     # ConfigMap name (string)
    readOnly: true
  - name: secret
    mountPath: /etc/secret
    secret: "my-secret"           # Secret name (string)
  - name: host
    mountPath: /data
    hostPath: "/opt/data"         # Host path (string)
  - name: scratch
    mountPath: /tmp/work
    emptyDir: true                # EmptyDir (bool)
```

### Examples

#### Simple Task

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: hello-world
spec:
  command: ["/bin/sh", "-c", "echo Hello from $(hostname) at $(date)"]
```

#### With Volumes

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: with-config
spec:
  command: ["/bin/sh", "-c", "cat /config/greeting"]
  volumes:
    - name: config
      mountPath: /config
      configMap: "app-config"
      readOnly: true
```

#### With Node Selector

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: compute-task
spec:
  command: ["/bin/sh", "-c", "echo Running on $(hostname)"]
  nodeSelector:
    workload-type: compute
```

### Controller

The `reaper-controller` watches ReaperPod resources and creates backing Pods with `runtimeClassName: reaper-v2`. It translates the simplified ReaperPod spec into a full Pod spec.

- Pod name matches ReaperPod name (1:1 mapping)
- Owner references enable automatic garbage collection
- Status is mirrored from the backing Pod
- If `overlayName` is set, the Pod stays Pending until a matching ReaperOverlay is Ready

---

## ReaperOverlay

A PVC-like resource that manages named overlay filesystem lifecycles independently from ReaperPod workloads. Enables Kubernetes-native overlay creation, reset, and deletion without requiring direct node access.

- **Group:** `reaper.io`
- **Version:** `v1alpha1`
- **Kind:** `ReaperOverlay`
- **Short name:** `rovl` (`kubectl get rovl`)

### Spec

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `resetPolicy` | `string` | `Manual` | When to reset: `Manual`, `OnFailure`, `OnDelete` |
| `resetGeneration` | `int` | `0` | Increment to trigger a reset on all nodes |

### Status

| Field | Type | Description |
|-------|------|-------------|
| `phase` | `string` | Current phase: `Pending`, `Ready`, `Resetting`, `Failed` |
| `observedResetGeneration` | `int` | Last resetGeneration fully applied |
| `nodes[]` | `array` | Per-node overlay state |
| `nodes[].nodeName` | `string` | Node name |
| `nodes[].ready` | `bool` | Whether the overlay is available |
| `nodes[].lastResetTime` | `string` | ISO 8601 timestamp of last reset |
| `message` | `string` | Human-readable status message |

### PVC-like Behavior

ReaperOverlay works like a PersistentVolumeClaim:

- **Blocking:** ReaperPods with `overlayName` stay Pending until the matching ReaperOverlay exists and is Ready
- **Cleanup on delete:** A finalizer ensures on-disk overlay data is cleaned up on all nodes when the ReaperOverlay is deleted
- **Reset:** Increment `spec.resetGeneration` to trigger overlay teardown and recreation on all nodes

### Examples

#### Create an Overlay

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperOverlay
metadata:
  name: slurm
spec:
  resetPolicy: Manual
```

#### Use with a ReaperPod

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: install-slurm
spec:
  overlayName: slurm
  command: ["bash", "-c", "apt-get update && apt-get install -y slurm-wlm"]
```

#### Reset a Corrupt Overlay

```bash
kubectl patch rovl slurm --type merge -p '{"spec":{"resetGeneration":1}}'
kubectl get rovl slurm -w   # watch until phase returns to Ready
```

#### Delete an Overlay

```bash
kubectl delete rovl slurm   # finalizer cleans up on-disk data on all nodes
```

---

## ReaperDaemonJob

A "DaemonSet for Jobs" that runs a command to completion on every matching node, with support for dependency ordering, retry policies, and shared overlays. Designed for node configuration tasks like Ansible playbooks that compose via shared overlays.

- **Group:** `reaper.io`
- **Version:** `v1alpha1`
- **Kind:** `ReaperDaemonJob`
- **Short name:** `rdjob` (`kubectl get rdjob`)

### Spec

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command` | `string[]` | *(required)* | Command to execute on each node |
| `args` | `string[]` | | Arguments to the command |
| `env` | `EnvVar[]` | | Environment variables (same format as ReaperPod) |
| `workingDir` | `string` | | Working directory for the command |
| `overlayName` | `string` | | Named overlay group for shared filesystem |
| `nodeSelector` | `map[string]string` | | Target specific nodes by labels (all nodes if empty) |
| `dnsMode` | `string` | | DNS resolution mode (`host` or `kubernetes`) |
| `runAsUser` | `int` | | UID for the process |
| `runAsGroup` | `int` | | GID for the process |
| `volumes` | `Volume[]` | | Volume mounts (same format as ReaperPod) |
| `tolerations` | `Toleration[]` | | Tolerations for the underlying Pods |
| `triggerOn` | `string` | `NodeReady` | Trigger events: `NodeReady` or `Manual` |
| `after` | `string[]` | | Dependency ordering — names of other ReaperDaemonJobs that must complete first |
| `retryLimit` | `int` | `0` | Maximum retries per node on failure |
| `concurrencyPolicy` | `string` | `Skip` | What to do on re-trigger while running: `Skip` or `Replace` |

### Status

| Field | Type | Description |
|-------|------|-------------|
| `phase` | `string` | Overall phase: `Pending`, `Running`, `Completed`, `PartiallyFailed` |
| `readyNodes` | `int` | Number of nodes that completed successfully |
| `totalNodes` | `int` | Total number of targeted nodes |
| `observedGeneration` | `int` | Last spec generation reconciled |
| `nodeStatuses[]` | `array` | Per-node execution status |
| `nodeStatuses[].nodeName` | `string` | Node name |
| `nodeStatuses[].phase` | `string` | Per-node phase: `Pending`, `Running`, `Succeeded`, `Failed` |
| `nodeStatuses[].reaperPodName` | `string` | Name of the ReaperPod created for this node |
| `nodeStatuses[].exitCode` | `int` | Exit code on this node |
| `nodeStatuses[].retryCount` | `int` | Number of retries so far |
| `message` | `string` | Human-readable status message |

### Controller Layering

`ReaperDaemonJob` → `ReaperPod` → `Pod`. The DaemonJob controller creates one ReaperPod per matching node, pinned via `nodeName`. The existing ReaperPod controller then creates the backing Pods. No changes to the runtime or shim.

### Dependency Ordering

The `after` field lists other ReaperDaemonJobs that must reach `Completed` phase before this job starts on any node. This enables composable workflows where one job's output is another's input (via shared overlays).

### Examples

#### Simple Node Info

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperDaemonJob
metadata:
  name: node-info
spec:
  command: ["/bin/sh", "-c"]
  args:
    - |
      echo "Node: $(hostname)"
      echo "Kernel: $(uname -r)"
```

#### Composable Node Config with Dependencies

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperDaemonJob
metadata:
  name: mount-filesystems
spec:
  command: ["/bin/sh", "-c"]
  args: ["mkdir -p /mnt/shared && mount -t nfs server:/export /mnt/shared"]
  overlayName: node-config
  nodeSelector:
    role: compute
---
apiVersion: reaper.io/v1alpha1
kind: ReaperDaemonJob
metadata:
  name: install-packages
spec:
  command: ["/bin/sh", "-c"]
  args: ["apt-get update && apt-get install -y htop"]
  overlayName: node-config
  after:
    - mount-filesystems
  nodeSelector:
    role: compute
  retryLimit: 2
```
