# 07 — Ansible Complex

Builds on [06-ansible-jobs](../06-ansible-jobs/) with three key improvements:

1. **Reboot-resilient Ansible installation** — uses a DaemonSet instead of a Job so Ansible is automatically reinstalled if a node reboots.
2. **Role-based node targeting** — workers are labeled `login` or `compute`, and workloads target specific node roles.
3. **Init container dependencies** — the nginx Job uses an init container that waits for Ansible to appear in the shared overlay, so everything can be deployed with a single `kubectl apply -f`.

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
| **Job** | `nginx-login` | `role=login` (2 pods) | Waits for Ansible (init container), then runs playbook |

### Init Container Dependency

The `nginx-login` Job pods have an init container that polls for `ansible-playbook` in the shared overlay. This creates an implicit dependency on the `ansible-bootstrap` DaemonSet without requiring sequential `kubectl apply`:

```
kubectl apply -f examples/07-ansible-complex/
  │
  ├─ DaemonSet: ansible-bootstrap pods start installing Ansible
  │
  └─ Job: nginx-login pods start, init container polls...
       │
       └─ Init container sees ansible-playbook → main container runs playbook
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
- ConfigMap `nginx-login-playbook` containing the Ansible playbook

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- curl
- Ansible (`pip install ansible`)

## Running the Demo

### Deploy everything at once

The init container dependency means a single apply is all you need:

```bash
kubectl apply -f examples/07-ansible-complex/
```

The DaemonSet pods start installing Ansible immediately. The Job pods start too, but their init containers block until Ansible appears in the overlay. Once the DaemonSet finishes on a login node, the Job pod on that node proceeds.

Wait for the Job to complete:

```bash
kubectl wait --for=condition=Complete job/nginx-login --timeout=300s
```

### Check the output

```bash
# DaemonSet bootstrap logs (all 9 workers)
kubectl logs -l app=ansible-bootstrap --all-containers --prefix

# Job playbook logs (login nodes only)
kubectl logs -l job-name=nginx-login --all-containers --prefix
```

Each Job pod on a login node reads the playbook from the ConfigMap, runs it with `ansible-playbook`, and the playbook installs nginx, creates a custom index page, verifies it responds, and stops it.

### Verify node placement

```bash
# DaemonSet should have 9 pods (all workers)
kubectl get pods -l app=ansible-bootstrap -o wide

# Job should have 2 pods (login nodes only)
kubectl get pods -l job-name=nginx-login -o wide
```

### Step-by-step alternative

If you prefer sequential deployment:

```bash
# Step 1: Bootstrap Ansible
kubectl apply -f examples/07-ansible-complex/ansible-bootstrap-daemonset.yaml
kubectl rollout status daemonset/ansible-bootstrap --timeout=300s

# Step 2: Run playbook
kubectl apply -f examples/07-ansible-complex/nginx-login-job.yaml
kubectl wait --for=condition=Complete job/nginx-login --timeout=300s
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
