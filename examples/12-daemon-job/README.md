# Example 12: ReaperDaemonJob CRD

Demonstrates the **ReaperDaemonJob** Custom Resource Definition — a "DaemonSet for Jobs" that runs a command to completion on every matching node.

## Use Case

Node configuration tasks that need to run on every node (or a subset) and re-trigger on node events. Examples: mounting filesystems, installing packages, running Ansible playbooks.

## Prerequisites

- Reaper installed on target nodes
- CRDs installed (`reaperdaemonjobs.reaper.giar.dev`)
- Controller running (`reaper-controller`)

The easiest way is via the Helm chart:

```bash
./scripts/setup-playground.sh
```

## Examples

### Simple Daemon Job

Runs a command on every ready node and reports system info:

```bash
kubectl apply -f examples/12-daemon-job/simple-daemon-job.yaml
kubectl get rdjob -w          # watch phase progress to Completed
kubectl get rdjob node-info   # check ready/total counts
```

### Composable Node Config

Two ReaperDaemonJobs sharing the `node-config` overlay with dependency ordering:

1. `mount-filesystems` — runs first on all `role: compute` nodes
2. `install-packages` — waits for `mount-filesystems` to complete (`after` field), then runs on the same nodes with up to 2 retries

```bash
kubectl apply -f examples/12-daemon-job/composable-node-config.yaml
kubectl get rdjob -w          # watch both jobs progress
kubectl get rpod              # see per-node ReaperPods created by the controller
```

## Key Features Demonstrated

- **Per-node execution**: one ReaperPod created per matching node
- **Dependency ordering**: `after: [mount-filesystems]` blocks until the dependency completes
- **Shared overlays**: `overlayName: node-config` lets both jobs share filesystem state
- **Node selector**: `nodeSelector: { role: compute }` targets a subset of nodes
- **Retry support**: `retryLimit: 2` retries failed nodes automatically
- **Status tracking**: `kubectl get rdjob` shows PHASE, READY, and TOTAL columns

## Cleanup

```bash
kubectl delete reaperdaemonjob --all
```
