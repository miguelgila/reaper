# 05 — Kubernetes Workload Mix

Demonstrates running **Jobs**, **DaemonSets**, and **Deployments** simultaneously on a 10-node cluster using Reaper. Each workload type targets a different set of nodes, showcasing that Reaper can handle diverse Kubernetes workload modes across different node roles — all without traditional container isolation.

All workloads read their configuration from dedicated ConfigMap volumes.

## Topology

```
┌─────────────────────────────────────────────────────────────────┐
│                     control-plane (no workloads)                │
├───────────────────┬───────────────────┬─────────────────────────┤
│  workload-type=   │  workload-type=   │  workload-type=         │
│     batch         │     daemon        │     service             │
│                   │                   │                         │
│  ┌─────────────┐  │  ┌─────────────┐  │  ┌─────────────┐       │
│  │  worker 1   │  │  │  worker 4   │  │  │  worker 7   │       │
│  │  (Job pod)  │  │  │  (DS pod)   │  │  │  (Deploy)   │       │
│  ├─────────────┤  │  ├─────────────┤  │  ├─────────────┤       │
│  │  worker 2   │  │  │  worker 5   │  │  │  worker 8   │       │
│  │  (Job pod)  │  │  │  (DS pod)   │  │  │  (Deploy)   │       │
│  ├─────────────┤  │  ├─────────────┤  │  ├─────────────┤       │
│  │  worker 3   │  │  │  worker 6   │  │  │  worker 9   │       │
│  │  (Job pod)  │  │  │  (DS pod)   │  │  │  (Deploy)   │       │
│  └─────────────┘  │  └─────────────┘  │  └─────────────┘       │
│                   │                   │                         │
│  Job: batch-      │  DS: node-health  │  Deployment: web-      │
│    report         │  (continuous      │    greeter (3 replicas, │
│  (run-to-         │   monitoring)     │    long-running)        │
│   completion)     │                   │                         │
└───────────────────┴───────────────────┴─────────────────────────┘
```

## Workloads

| Kind | Name | Nodes | ConfigMap | Behavior |
|------|------|-------|-----------|----------|
| **Job** | `batch-report` | `workload-type=batch` (3 pods) | `batch-config` | Collects node metrics (load, memory, disk, network) and exits |
| **DaemonSet** | `node-health` | `workload-type=daemon` (3 pods) | `monitor-config` | Continuously monitors health with configurable thresholds |
| **Deployment** | `web-greeter` | `workload-type=service` (3 replicas) | `greeter-config` | Logs periodic greetings with health check status |

## Setup

From the repository root:

```bash
# Uses the latest published GitHub release (no build required)
./examples/05-kubemix/setup.sh

# Or pin a specific version
```

This creates:
- A 10-node Kind cluster (1 control-plane + 9 workers)
- Downloads and installs pre-built Reaper binaries from [GitHub Releases](https://github.com/miguelgila/reaper/releases)
- Node labels partitioning workers into `batch`, `daemon`, and `service` groups
- Three ConfigMaps (`batch-config`, `monitor-config`, `greeter-config`)

No Rust toolchain or source build is needed — the setup script fetches pre-built binaries automatically.

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- curl
- [Helm](https://helm.sh/)

## Running the Demo

All three workloads can run simultaneously since they target different nodes and don't interact with each other.

### 1. Deploy all workloads

```bash
kubectl apply -f examples/05-kubemix/
```

### 2. Check pod placement

```bash
kubectl get pods -o wide
```

Expected: 3 Job pods on batch nodes, 3 DaemonSet pods on daemon nodes, 3 Deployment pods on service nodes — no overlap.

### 3. View Job output (runs to completion)

```bash
kubectl logs -l job-name=batch-report --all-containers --prefix
```

Each Job pod collects node information (load, memory, disk, network) driven by the `batch-config` ConfigMap and exits.

### 4. View DaemonSet output (continuous)

```bash
kubectl logs -l app=node-health --all-containers --prefix -f
```

Each DaemonSet pod reports health status at the interval defined in `monitor-config`, with load and memory threshold evaluation.

### 5. View Deployment output (continuous)

```bash
kubectl logs -l app=web-greeter --all-containers --prefix -f
```

Each Deployment replica logs greetings at regular intervals using settings from `greeter-config`, including periodic health check summaries.

### 6. Verify ConfigMap mounts

```bash
# Check batch config
kubectl exec $(kubectl get pods -l job-name=batch-report -o name | head -1) -- cat /config/report.conf

# Check monitor config (pick any DaemonSet pod)
kubectl exec $(kubectl get pods -l app=node-health -o name | head -1) -- cat /config/monitor.conf

# Check greeter config (pick any Deployment pod)
kubectl exec $(kubectl get pods -l app=web-greeter -o name | head -1) -- cat /config/greeter.conf
```

## How It Works

1. **Node labels** partition the 9 workers into three groups of three, each dedicated to a workload type.
2. **`nodeSelector`** in each manifest ensures pods land only on their designated nodes.
3. **ConfigMap volumes** are mounted at `/config` in each pod, providing runtime configuration without baking settings into the command.
4. Reaper executes all commands directly on the host via the shared overlay filesystem — no container images are actually used (busybox is pulled by kubelet but ignored by Reaper).
5. Each workload type operates independently. The Job pods run to completion and stop, while the DaemonSet and Deployment pods run continuously.

## Cleanup

```bash
# Delete workloads
kubectl delete -f examples/05-kubemix/

# Delete cluster and ConfigMaps
./examples/05-kubemix/setup.sh --cleanup
```
