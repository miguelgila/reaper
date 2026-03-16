# Configuration

Reaper is configured through a combination of node-level configuration files, environment variables, and per-pod Kubernetes annotations.

## Node Configuration

Reaper reads configuration from `/etc/reaper/reaper.conf` on each node. The Helm chart creates this file automatically via the node DaemonSet init container.

### Config File Format

```ini
# /etc/reaper/reaper.conf (KEY=VALUE, one per line)
REAPER_DNS_MODE=kubernetes
REAPER_RUNTIME_LOG=/run/reaper/runtime.log
REAPER_OVERLAY_BASE=/run/reaper/overlay
REAPER_OVERLAY_ISOLATION=namespace
```

### Load Order

1. Config file defaults (`/etc/reaper/reaper.conf`)
2. Environment variables override file values

### Settings Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `REAPER_CONFIG` | `/etc/reaper/reaper.conf` | Override config file path |
| `REAPER_DNS_MODE` | `host` | DNS resolution: `host` (node's resolv.conf) or `kubernetes`/`k8s` (CoreDNS) |
| `REAPER_OVERLAY_ISOLATION` | `namespace` | Overlay isolation: `namespace` (per-K8s-namespace) or `node` (shared) |
| `REAPER_OVERLAY_BASE` | `/run/reaper/overlay` | Base directory for overlay upper/work layers |
| `REAPER_RUNTIME_LOG` | *(none)* | Runtime log file path |
| `REAPER_SHIM_LOG` | *(none)* | Shim log file path |
| `REAPER_ANNOTATIONS_ENABLED` | `true` | Master switch for pod annotation overrides |
| `REAPER_FILTER_ENABLED` | `true` | Enable sensitive file filtering in overlay |
| `REAPER_FILTER_PATHS` | *(none)* | Additional colon-separated paths to filter |
| `REAPER_FILTER_MODE` | `append` | Filter mode: `append` (add to defaults) or `replace` |
| `REAPER_FILTER_ALLOWLIST` | *(none)* | Paths to exclude from filtering |

## Pod Annotations

Users can override certain Reaper configuration parameters per-pod using Kubernetes annotations with the `reaper.runtime/` prefix.

### Supported Annotations

| Annotation | Values | Default | Description |
|------------|--------|---------|-------------|
| `reaper.runtime/dns-mode` | `host`, `kubernetes`, `k8s` | Node config (`REAPER_DNS_MODE`) | DNS resolution mode for this pod |
| `reaper.runtime/overlay-name` | DNS label (e.g., `pippo`) | *(none — uses namespace overlay)* | Named overlay group within the namespace |

### Example

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-task
  annotations:
    reaper.runtime/dns-mode: "kubernetes"
    reaper.runtime/overlay-name: "my-group"
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: task
      image: busybox
      command: ["/bin/sh", "-c", "nslookup kubernetes.default"]
```

### Security Model

- Only annotations in the allowlist above are honored. Unknown annotation keys are silently ignored.
- Administrator-controlled parameters (overlay paths, filter settings, isolation mode) **cannot** be overridden via annotations.
- Administrators can disable all annotation processing: `REAPER_ANNOTATIONS_ENABLED=false`

### How It Works

1. The shim extracts `reaper.runtime/*` annotations from the OCI config (populated by kubelet from pod metadata).
2. Annotations are stored in the container state during `create`.
3. During `start`, annotations are validated against the allowlist and applied. Invalid values are logged and ignored.
4. If no annotation is set, the node-level configuration is used as the default.

## Helm Chart Values

The Helm chart (`deploy/helm/reaper/`) configures most settings automatically. Key values:

```yaml
# Node configuration written to /etc/reaper/reaper.conf
config:
  dnsMode: kubernetes
  runtimeLog: /run/reaper/runtime.log

# Image settings (tag defaults to Chart.AppVersion)
node:
  image:
    repository: ghcr.io/miguelgila/reaper-node
    tag: ""
controller:
  image:
    repository: ghcr.io/miguelgila/reaper-controller
    tag: ""
agent:
  enabled: true
  image:
    repository: ghcr.io/miguelgila/reaper-agent
    tag: ""
```

See [deploy/helm/reaper/values.yaml](../deploy/helm/reaper/values.yaml) for the full reference.
