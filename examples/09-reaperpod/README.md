# Example 09: ReaperPod CRD

Demonstrates the **ReaperPod** Custom Resource Definition — a simplified, Reaper-native way to run workloads without container boilerplate.

## Use Case

Run commands on host nodes without writing full Pod specs. The `reaper-controller` watches ReaperPod resources and creates real Pods with `runtimeClassName: reaper-v2` pre-configured.

## Prerequisites

- Reaper installed on target nodes
- CRDs installed (`reaperpods.reaper.giar.dev`)
- Controller running (`reaper-controller`)

The easiest way is via the Helm chart:

```bash
./scripts/setup-playground.sh
```

## Examples

### Simple Task

Runs a command and reports system info:

```bash
kubectl apply -f examples/09-reaperpod/simple-task.yaml
kubectl get reaperpods
kubectl logs hello-world
```

### With Volumes

Mounts a ConfigMap and emptyDir:

```bash
kubectl create configmap app-config --from-literal=greeting="Hello from ConfigMap"
kubectl apply -f examples/09-reaperpod/with-volumes.yaml
kubectl logs config-demo
```

### With Node Selector

Targets specific nodes (label a node first):

```bash
kubectl label node <node-name> workload-type=compute
kubectl apply -f examples/09-reaperpod/with-node-selector.yaml
kubectl get reaperpods
```

## Key Features Demonstrated

- **No image field** — busybox placeholder handled automatically by the controller
- **Simplified volumes** — flat format: `configMap: "name"` instead of nested Kubernetes volume specs
- **Reaper-specific fields** — `dnsMode`, `overlayName`, `tolerations`
- **Status tracking** — phase, podName, nodeName, exitCode via `kubectl get rpod`

## Cleanup

```bash
kubectl delete reaperpod --all
```
