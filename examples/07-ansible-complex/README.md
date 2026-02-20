# 07 — Ansible Complex

Builds on [06-ansible-jobs](../06-ansible-jobs/) with three key improvements:

1. **Fully reboot-resilient** — all workloads are DaemonSets, so Ansible, nginx, and htop are all automatically reinstalled if a node reboots.
2. **Role-based node targeting** — workers are labeled `login` or `compute`, and workloads target specific node roles.
3. **Init container dependencies** — the nginx and htop DaemonSets use init containers that wait for Ansible to appear in the shared overlay, so everything can be deployed with a single `kubectl apply -f`.

## Why DaemonSets for Everything?

Reaper's overlay filesystem lives on tmpfs (`/run/reaper/overlay`). When a node reboots, tmpfs is wiped and the overlay resets — any packages installed by a previous workload are gone.

| Approach | Survives reboot | Stays running |
|----------|-----------------|---------------|
| **Job** | No — overlay resets, packages lost | No — completed, kubelet won't re-run |
| **DaemonSet** | **Yes** — kubelet restarts pod, reinstalls | Yes — sleeps after install |

All three DaemonSets install their packages and then sleep. Kubelet keeps them alive and restarts them after any node reboot, which re-enters a fresh overlay and reinstalls everything automatically.

```
Node boots → kubelet starts all DaemonSet pods
  ├─ ansible-bootstrap: installs Ansible
  ├─ nginx-login (init container waits for Ansible) → runs nginx playbook
  └─ htop-compute (init container waits for Ansible) → runs htop playbook

Node reboots → tmpfs wiped → overlay resets → kubelet restarts all pods → everything reinstalled
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
│  nginx-login DaemonSet (login nodes only)                │
│  htop-compute DaemonSet (compute nodes only)             │
└──────────────────────────────────────────────────────────┘
```

## Workloads

| Kind | Name | Target nodes | What it does |
|------|------|-------------|-------------|
| **DaemonSet** | `ansible-bootstrap` | All workers (9 pods) | Installs Ansible, sleeps; survives reboots |
| **DaemonSet** | `nginx-login` | `role=login` (2 pods) | Waits for Ansible, installs nginx via playbook, sleeps |
| **DaemonSet** | `htop-compute` | `role=compute` (7 pods) | Waits for Ansible, installs htop via playbook, sleeps |

### Init Container Dependencies

The `nginx-login` and `htop-compute` DaemonSets have init containers that poll for `ansible-playbook` in the shared overlay. This creates an implicit dependency on the `ansible-bootstrap` DaemonSet without requiring sequential `kubectl apply`:

```
kubectl apply -f examples/07-ansible-complex/
  │
  ├─ DaemonSet: ansible-bootstrap pods start installing Ansible
  │
  ├─ DaemonSet: nginx-login pods start on login nodes, init container polls...
  │    └─ Init container sees ansible-playbook → main container runs nginx playbook → sleeps
  │
  └─ DaemonSet: htop-compute pods start on compute nodes, init container polls...
       └─ Init container sees ansible-playbook → main container runs htop playbook → sleeps
```

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
- ConfigMaps `nginx-login-playbook` and `htop-compute-playbook` containing the Ansible playbooks

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- curl
- Ansible (`pip install ansible`)

## Running the Demo

### Deploy everything at once

```bash
kubectl apply -f examples/07-ansible-complex/
```

The bootstrap DaemonSet pods start installing Ansible immediately. The nginx and htop DaemonSet pods start too, but their init containers block until Ansible appears in the overlay. Once the bootstrap finishes on a node, the role-specific DaemonSet on that node proceeds.

### Check rollout status

```bash
kubectl rollout status daemonset/ansible-bootstrap --timeout=300s
kubectl rollout status daemonset/nginx-login --timeout=300s
kubectl rollout status daemonset/htop-compute --timeout=300s
```

### Check the output

```bash
# Bootstrap logs (all 9 workers)
kubectl logs -l app=ansible-bootstrap --all-containers --prefix

# nginx playbook logs (login nodes)
kubectl logs -l app=nginx-login --all-containers --prefix

# htop playbook logs (compute nodes)
kubectl logs -l app=htop-compute --all-containers --prefix
```

### Verify node placement

```bash
# Bootstrap: 9 pods (all workers)
kubectl get pods -l app=ansible-bootstrap -o wide

# nginx: 2 pods (login nodes only)
kubectl get pods -l app=nginx-login -o wide

# htop: 7 pods (compute nodes only)
kubectl get pods -l app=htop-compute -o wide
```

### Simulate a reboot (optional)

To verify everything recovers after an overlay reset:

```bash
# Pick a login node
NODE=$(kubectl get pods -l app=nginx-login -o wide --no-headers | head -1 | awk '{print $7}')

# Restart the docker container (simulates reboot, wipes tmpfs)
docker restart "$NODE"

# Wait for the node to come back
kubectl wait --for=condition=Ready node "$NODE" --timeout=60s

# All three DaemonSets restart: Ansible, then nginx (after init container)
kubectl get pods -o wide --field-selector spec.nodeName="$NODE"
kubectl logs -l app=nginx-login --all-containers --prefix | grep "$NODE"
```

## Cleanup

```bash
# Delete workloads
kubectl delete -f examples/07-ansible-complex/

# Delete cluster and ConfigMaps
./examples/07-ansible-complex/setup.sh --cleanup
```
