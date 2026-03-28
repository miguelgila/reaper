# Reaper Helm Chart

Install Reaper runtime, CRDs, and controller on a Kubernetes cluster.

## Prerequisites

- Kubernetes 1.25+
- Helm 3.x
- containerd runtime with Reaper shim configured (Kind clusters handle this automatically)

## Install

```bash
helm upgrade --install reaper deploy/helm/reaper/ \
  --namespace reaper-system --create-namespace \
  --wait --timeout 120s
```

## What Gets Installed

| Resource | Description |
|----------|-------------|
| **DaemonSet** (`reaper-node`) | Init container copies shim + runtime binaries to every node |
| **Deployment** (`reaper-controller`) | Watches `ReaperPod` CRDs and creates Pods |
| **CRD** (`reaperpods.reaper.giar.dev`) | Custom resource for simplified Reaper workloads |
| **RuntimeClass** (`reaper-v2`) | Kubernetes RuntimeClass pointing to the Reaper shim |
| **RBAC** | ServiceAccount, ClusterRole, ClusterRoleBinding for the controller |

## Configuration

Key values in `values.yaml`:

| Value | Default | Description |
|-------|---------|-------------|
| `node.image.repository` | `ghcr.io/miguelgila/reaper-node` | Node installer image |
| `node.image.tag` | `latest` | Node image tag |
| `node.installPath` | `/usr/local/bin` | Host path for binaries |
| `node.configureContainerd` | `false` | Patch containerd config and restart (not needed for Kind) |
| `controller.image.repository` | `ghcr.io/miguelgila/reaper-controller` | Controller image |
| `controller.image.tag` | `latest` | Controller image tag |
| `config.dnsMode` | `kubernetes` | DNS mode: `host` or `kubernetes` |
| `config.runtimeLog` | `/run/reaper/runtime.log` | Runtime log path |
| `runtimeClass.name` | `reaper-v2` | RuntimeClass name |
| `runtimeClass.handler` | `reaper-v2` | containerd handler name |

## Uninstall

```bash
helm uninstall reaper --namespace reaper-system
```

Note: CRDs are not removed by `helm uninstall` (Helm convention). To remove:

```bash
kubectl delete crd reaperpods.reaper.giar.dev
```
