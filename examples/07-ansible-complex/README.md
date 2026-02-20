# 07 — Ansible Complex

Builds on [06-ansible-jobs](../06-ansible-jobs/) with two key improvements:

1. **Reboot-resilient Ansible installation** — uses a DaemonSet instead of a Job so Ansible is automatically reinstalled if a node reboots.
2. **Role-based node targeting** — workers are labeled `login` or `compute`, and workloads target specific node roles.

## Why a DaemonSet Instead of a Job?

Reaper's overlay filesystem lives on tmpfs (`/run/reaper/overlay`). When a node reboots, tmpfs is wiped and the overlay resets — any packages installed by a previous Job are gone.

| Approach | Runs once | Survives reboot | Stays running |
|----------|-----------|-----------------|---------------|
| **Job** | Yes | No — overlay resets, Ansible is lost | No |
| **DaemonSet** | Yes, and re-runs after restart | **Yes** — kubelet restarts pod, reinstalls | Yes |

The `ansible-bootstrap` DaemonSet installs Ansible and then sleeps. Kubelet keeps the pod alive and restarts it after any node reboot, which re-enters a fresh overlay and reinstalls Ansible automatically.

```
Node boots → kubelet starts DaemonSet pod → Ansible installed in overlay
     ↓
Node reboots → tmpfs wiped → overlay resets → kubelet restarts pod → Ansible reinstalled
```

## Topology

```
┌──────────────────────────────────────────────────────────┐
│                control-plane (no workloads)               │
├──────────────┬───────────────────────────────────────────┤
│  role=login  │              role=compute                  │
│              │                                           │
│  ┌────────┐  │  ┌────────┐ ┌────────┐ ┌────────┐        │
│  │worker 1│  │  │worker 3│ │worker 4│ │worker 5│        │
│  ├────────┤  │  ├────────┤ ├────────┤ ├────────┤        │
│  │worker 2│  │  │worker 6│ │worker 7│ │worker 8│        │
│  └────────┘  │  ├────────┤                               │
│              │  │worker 9│                               │
│  2 nodes     │  └────────┘                               │
│              │  7 nodes                                  │
├──────────────┴───────────────────────────────────────────┤
│  ansible-bootstrap DaemonSet (ALL 9 workers)             │
│  nginx-login Job (login nodes only)                      │
└──────────────────────────────────────────────────────────┘
```

## Workloads

| Kind | Name | Target nodes | What it does |
|------|------|-------------|-------------|
| **DaemonSet** | `ansible-bootstrap` | All workers (9 pods) | Installs Ansible, sleeps; survives reboots |
| **Job** | `nginx-login` | `role=login` (2 pods) | Runs Ansible playbook to install/verify nginx |

## Setup

From the repository root:

```bash
# Uses the latest published GitHub release (no build required)
./examples/07-ansible-complex/setup.sh

# Or pin a specific version
./examples/07-ansible-complex/setup.sh v0.2.5
```

This creates:
- A 10-node Kind cluster (1 control-plane + 9 workers)
- Downloads and installs pre-built Reaper binaries from [GitHub Releases](https://github.com/miguelgila/reaper/releases)
- Node labels: 2 workers as `role=login`, 7 as `role=compute`
- ConfigMap `nginx-login-playbook` containing the Ansible playbook

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- curl
- Ansible (`pip install ansible`)

## Running the Demo

### Step 1: Deploy the Ansible bootstrap DaemonSet

```bash
kubectl apply -f examples/07-ansible-complex/ansible-bootstrap-daemonset.yaml
kubectl rollout status daemonset/ansible-bootstrap --timeout=300s
```

Check that Ansible is installed on all workers:

```bash
kubectl logs -l app=ansible-bootstrap --all-containers --prefix
```

### Step 2: Run the nginx playbook on login nodes

```bash
kubectl apply -f examples/07-ansible-complex/nginx-login-job.yaml
kubectl wait --for=condition=Complete job/nginx-login --timeout=300s
```

Check the output:

```bash
kubectl logs -l job-name=nginx-login --all-containers --prefix
```

Each pod on a login node reads the playbook from the ConfigMap, runs it with `ansible-playbook`, and the playbook installs nginx, creates a custom index page, verifies it responds, and stops it.

### Verify node placement

```bash
# DaemonSet should have 9 pods (all workers)
kubectl get pods -l app=ansible-bootstrap -o wide

# Job should have 2 pods (login nodes only)
kubectl get pods -l job-name=nginx-login -o wide
```

### Simulate a reboot (optional)

To see the DaemonSet recover after an overlay reset:

```bash
# Pick a worker node
NODE=$(kubectl get pods -l app=ansible-bootstrap -o wide --no-headers | head -1 | awk '{print $7}')

# Restart the docker container (simulates reboot, wipes tmpfs)
docker restart "$NODE"

# Wait for the node to come back
kubectl wait --for=condition=Ready node "$NODE" --timeout=60s

# The DaemonSet pod is restarted by kubelet, reinstalls Ansible
kubectl logs -l app=ansible-bootstrap --all-containers --prefix | grep "$NODE"
```

## Cleanup

```bash
# Delete workloads
kubectl delete -f examples/07-ansible-complex/

# Delete cluster and ConfigMaps
./examples/07-ansible-complex/setup.sh --cleanup
```
