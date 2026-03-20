# Example 10: Slurm HPC (Mixed Runtimes)

Demonstrates a **Slurm HPC cluster** using mixed Kubernetes runtimes:

- **slurmctld** (Slurm controller) runs as a standard containerized Deployment (default runtime)
- **slurmd** (Slurm worker daemons) run on compute nodes via Reaper DaemonSet with direct host access for CPU pinning, device access, and real resource management

This mirrors a real HPC pattern where the scheduler runs in a container but worker daemons need bare-metal access.

## Architecture

```
                    ┌─────────────────┐
                    │   slurmctld     │  ← Deployment (default runtime)
                    │  (scheduler)    │     Ubuntu + Slurm packages
                    └────────┬────────┘
                             │ Slurm RPC (port 6817)
              ┌──────────────┼──────────────┐
              ▼              ▼              ▼
     ┌────────────┐  ┌────────────┐  ┌────────────┐
     │   slurmd   │  │   slurmd   │  │   slurmd   │  ← DaemonSet (Reaper)
     │  worker-0  │  │  worker-1  │  │  worker-2  │     Direct host access
     └────────────┘  └────────────┘  └────────────┘
```

## Cluster Topology

- **4 nodes**: 1 control-plane + 1 slurmctld + 2 compute workers
- `role=slurmctld`: Runs the Slurm controller (standard container)
- `role=compute`: Runs slurmd via Reaper (host-level daemon)

## Why Reaper for slurmd?

Slurm worker daemons need:
- Direct access to CPUs for `cgroup`-based job isolation
- Access to local storage and GPUs
- Ability to manage system users and groups
- Real process tree visibility for job tracking

Standard containers would isolate slurmd from the resources it needs to manage.

## Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- [Helm](https://helm.sh/)
- ReaperOverlay CRD installed (included in Helm chart, or `kubectl apply -f deploy/kubernetes/crds/reaperoverlays.reaper.io.yaml`)

## Usage

```bash
# Create the cluster (generates slurm-config with actual node names)
./examples/10-slurm-hpc/setup.sh

# Deploy Slurm components
kubectl apply -f examples/10-slurm-hpc/slurm-overlay.yaml   # ReaperOverlay (shared overlay for slurmd)
kubectl apply -f examples/10-slurm-hpc/munge-secret.yaml
kubectl apply -f examples/10-slurm-hpc/slurmctld-deployment.yaml
kubectl apply -f examples/10-slurm-hpc/slurmd-daemonset.yaml

# Wait for readiness
kubectl rollout status deployment/slurmctld --timeout=120s
kubectl rollout status daemonset/slurmd --timeout=300s

# Submit a test job
kubectl apply -f examples/10-slurm-hpc/test-job.yaml
kubectl logs test-slurm-job -f

# Check overlay status
kubectl get rovl slurm

# Clean up
kubectl delete -f examples/10-slurm-hpc/
./examples/10-slurm-hpc/setup.sh --cleanup
```

## Troubleshooting

### Corrupt overlay (broken dpkg state)

If a package installation fails (e.g., dpkg post-install script error), the shared overlay may be left in a broken state. All subsequent slurmd pods will inherit the broken state.

Reset the overlay without node access:

```bash
# Reset the overlay (kills helper, removes overlay dirs on all nodes)
kubectl patch rovl slurm --type merge -p '{"spec":{"resetGeneration":1}}'

# Watch until phase returns to Ready
kubectl get rovl slurm -w

# Restart slurmd pods to pick up the clean overlay
kubectl rollout restart daemonset/slurmd
```
