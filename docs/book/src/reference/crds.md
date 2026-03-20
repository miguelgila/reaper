# Custom Resource Definitions

Reaper provides two CRDs for managing workloads and overlay filesystems.

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
