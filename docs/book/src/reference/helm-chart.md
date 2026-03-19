# Helm Chart Reference

The Reaper Helm chart is located at `deploy/helm/reaper/`.

## Installation

```bash
helm upgrade --install reaper deploy/helm/reaper/ \
  --namespace reaper-system --create-namespace \
  --wait --timeout 120s
```

## Values

### Node Installer DaemonSet

| Value | Default | Description |
|-------|---------|-------------|
| `node.image.repository` | `ghcr.io/miguelgila/reaper-node` | Node installer image |
| `node.image.tag` | `""` (uses `appVersion`) | Image tag |
| `node.image.pullPolicy` | `IfNotPresent` | Pull policy |
| `node.installPath` | `/usr/local/bin` | Binary install path on host |
| `node.configureContainerd` | `false` | Whether to configure and restart containerd |

### CRD Controller Deployment

| Value | Default | Description |
|-------|---------|-------------|
| `controller.image.repository` | `ghcr.io/miguelgila/reaper-controller` | Controller image |
| `controller.image.tag` | `""` (uses `appVersion`) | Image tag |
| `controller.image.pullPolicy` | `IfNotPresent` | Pull policy |
| `controller.replicas` | `1` | Number of controller replicas |
| `controller.resources.requests.cpu` | `10m` | CPU request |
| `controller.resources.requests.memory` | `32Mi` | Memory request |
| `controller.resources.limits.cpu` | `100m` | CPU limit |
| `controller.resources.limits.memory` | `64Mi` | Memory limit |

### Agent DaemonSet

| Value | Default | Description |
|-------|---------|-------------|
| `agent.enabled` | `true` | Enable the agent DaemonSet |
| `agent.image.repository` | `ghcr.io/miguelgila/reaper-agent` | Agent image |
| `agent.image.tag` | `""` (uses `appVersion`) | Image tag |
| `agent.image.pullPolicy` | `IfNotPresent` | Pull policy |
| `agent.resources.requests.cpu` | `10m` | CPU request |
| `agent.resources.requests.memory` | `32Mi` | Memory request |
| `agent.resources.limits.cpu` | `100m` | CPU limit |
| `agent.resources.limits.memory` | `64Mi` | Memory limit |

### RuntimeClass

| Value | Default | Description |
|-------|---------|-------------|
| `runtimeClass.name` | `reaper-v2` | RuntimeClass name |
| `runtimeClass.handler` | `reaper-v2` | Containerd handler name |

### Reaper Configuration

| Value | Default | Description |
|-------|---------|-------------|
| `config.dnsMode` | `kubernetes` | DNS resolution mode |
| `config.runtimeLog` | `/run/reaper/runtime.log` | Runtime log path |

## What Gets Installed

The chart installs:

1. **CRDs** (`deploy/helm/reaper/crds/`) — ReaperPod CRD definition
2. **Namespace** — `reaper-system` (created by `--create-namespace`)
3. **Node DaemonSet** — Init container copies shim + runtime binaries to host
4. **Controller Deployment** — Watches ReaperPod CRDs, creates Pods
5. **Agent DaemonSet** — Health monitoring and Prometheus metrics
6. **RuntimeClass** — Registers `reaper-v2` with Kubernetes
7. **RBAC** — ServiceAccount, ClusterRole, ClusterRoleBinding for controller and agent
