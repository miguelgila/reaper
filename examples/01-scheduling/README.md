# Scheduling Examples

Demonstrates how to run Reaper workloads across Kubernetes nodes using standard scheduling primitives.

## Setup

From the repository root:

```bash
./examples/scheduling/setup.sh
```

This creates a 3-node Kind cluster (`reaper-scheduling-demo`) with:
- 1 control-plane node
- 2 worker nodes (one labeled `node-role=login`, one unlabeled)
- Reaper runtime installed on all nodes
- `reaper-v2` RuntimeClass configured

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- Ansible (`pip install ansible`)

## Examples

### Run on all nodes (DaemonSet)

Deploys a lightweight monitor on every node that logs load average and available memory every 60 seconds.

```bash
kubectl apply -f examples/scheduling/all-nodes-daemonset.yaml

# Check pods — one per node
kubectl get pods -l app=node-monitor -o wide

# View logs
kubectl logs -l app=node-monitor --all-containers --prefix
```

### Run on a subset of nodes (DaemonSet with nodeSelector)

Deploys a monitor only on nodes labeled `node-role=login`. In the demo cluster, only one worker has this label — the DaemonSet schedules a pod there and skips the other worker.

```bash
kubectl apply -f examples/scheduling/subset-nodes-daemonset.yaml

# Check pods — only on the labeled node
kubectl get pods -l app=login-monitor -o wide

# View logs
kubectl logs -l app=login-monitor --all-containers --prefix
```

You can label additional nodes at any time and the DaemonSet will automatically schedule pods on them:

```bash
kubectl label node <node-name> node-role=login
```

## Cleanup

```bash
# Remove the example workloads
kubectl delete -f examples/scheduling/

# Delete the Kind cluster
./examples/scheduling/setup.sh --cleanup
```
