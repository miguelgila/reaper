# ReaperPod CRD

The ReaperPod Custom Resource Definition provides a simplified, Reaper-native way to run workloads without standard container boilerplate.

## API Reference

- **Group:** `reaper.io`
- **Version:** `v1alpha1`
- **Kind:** `ReaperPod`

## Spec

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `command` | `string[]` | Yes | Command to execute on the host |
| `args` | `string[]` | No | Arguments to the command |
| `env` | `EnvVar[]` | No | Environment variables (simplified format) |
| `volumes` | `Volume[]` | No | Volume mounts (simplified format) |
| `nodeSelector` | `map[string]string` | No | Node selection constraints |
| `dnsMode` | `string` | No | DNS resolution mode (`host` or `kubernetes`) |
| `overlayName` | `string` | No | Named overlay group |

## Status

| Field | Type | Description |
|-------|------|-------------|
| `phase` | `string` | Current phase: `Pending`, `Running`, `Succeeded`, `Failed` |
| `podName` | `string` | Name of the backing Pod |
| `nodeName` | `string` | Node where the workload runs |
| `exitCode` | `int` | Process exit code (when completed) |
| `startTime` | `string` | When the workload started |
| `completionTime` | `string` | When the workload completed |

## Simplified Volumes

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

## Examples

### Simple Task

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: hello-world
spec:
  command: ["/bin/sh", "-c", "echo Hello from $(hostname) at $(date)"]
```

### With Volumes

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

### With Node Selector

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

## Controller

The `reaper-controller` watches ReaperPod resources and creates backing Pods with `runtimeClassName: reaper-v2`. It translates the simplified ReaperPod spec into a full Pod spec.

- Pod name matches ReaperPod name (1:1 mapping)
- Owner references enable automatic garbage collection
- Status is mirrored from the backing Pod
