# Example 11: Node Monitoring (Prometheus + Reaper)

Demonstrates **host-level node monitoring** using mixed runtimes:

- **node_exporter** runs as a Reaper DaemonSet on every worker — direct host access gives accurate hardware and OS metrics
- **Prometheus server** runs as a standard containerized Deployment (default runtime) — scrapes node_exporter endpoints

## Why Host-Level Exporters?

Running `node_exporter` inside a standard container has limitations:
- Filesystem metrics reflect the container's overlay, not the host
- Network metrics may miss host interfaces
- CPU/memory data can be skewed by cgroup boundaries
- Hardware sensors and IPMI are inaccessible

With Reaper, `node_exporter` runs directly on the host, providing the same metric fidelity as a native systemd service.

## Architecture

```
┌──────────────────────┐
│   Prometheus         │  ← Deployment (default runtime)
│   (scraper/UI)       │     Scrapes :9100/metrics
└──────────┬───────────┘
           │ HTTP GET :9100/metrics
    ┌──────┼──────┐
    ▼      ▼      ▼
┌──────┐┌──────┐┌──────┐
│ n_e  ││ n_e  ││ n_e  │  ← DaemonSet (Reaper)
│ :9100││ :9100││ :9100│     Direct host metrics
└──────┘└──────┘└──────┘
worker-0 worker-1 worker-2
```

## Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- [Helm](https://helm.sh/)

## Usage

```bash
# Create the cluster
./examples/11-node-monitoring/setup.sh

# Deploy monitoring stack
kubectl apply -f examples/11-node-monitoring/node-exporter-daemonset.yaml
kubectl apply -f examples/11-node-monitoring/prometheus-config.yaml
kubectl apply -f examples/11-node-monitoring/prometheus-deployment.yaml

# Wait for readiness
kubectl rollout status daemonset/node-exporter --timeout=120s
kubectl rollout status deployment/prometheus --timeout=120s

# Access Prometheus UI
kubectl port-forward svc/prometheus 9090:9090
# Open http://localhost:9090 and query: up{job="node-exporter"}

# Check node_exporter metrics directly
kubectl port-forward ds/node-exporter 9100:9100
# curl http://localhost:9100/metrics | head -20

# Clean up
kubectl delete -f examples/11-node-monitoring/
./examples/11-node-monitoring/setup.sh --cleanup
```
