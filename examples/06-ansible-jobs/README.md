# 06 — Ansible Jobs

Demonstrates using Reaper Jobs to install and run **Ansible** across a 10-node cluster. This showcases two key Reaper capabilities:

1. **Overlay persistence** — packages installed by one Job remain available to subsequent Jobs on the same node (shared overlay upper layer).
2. **ConfigMap as code delivery** — an Ansible playbook is stored in a ConfigMap and mounted into Job pods, enabling infrastructure-as-code workflows directly on cluster nodes.

## Topology

```
┌──────────────────────────────────────────────────────────┐
│                control-plane (no workloads)               │
├──────────────────────────────────────────────────────────┤
│                                                          │
│  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ │
│  │worker 1│ │worker 2│ │worker 3│ │worker 4│ │worker 5│ │
│  └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ │
│  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐            │
│  │worker 6│ │worker 7│ │worker 8│ │worker 9│            │
│  └────────┘ └────────┘ └────────┘ └────────┘            │
│                                                          │
│  Step 1: install-ansible Job (one pod per worker)        │
│  Step 2: nginx-playbook Job  (one pod per worker)        │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

## Jobs

| Step | Job | What it does | ConfigMap |
|------|-----|-------------|-----------|
| 1 | `install-ansible` | Installs Ansible via `apt-get` on all 9 workers | — |
| 2 | `nginx-playbook` | Runs an Ansible playbook to install, configure, and verify nginx | `nginx-playbook` |

Both Jobs use `topologySpreadConstraints` to guarantee exactly one pod per worker node (9 completions, 9 parallelism).

## How the Overlay Makes This Work

Reaper workloads share a **single overlay filesystem** per node. When Job 1 installs Ansible via `apt-get`, the installed packages are written to the overlay's upper layer. When Job 2 runs on the same node, it enters the same overlay namespace and sees all previously installed packages — including Ansible.

```
Job 1 (install-ansible)          Job 2 (nginx-playbook)
         │                                │
         ▼                                ▼
   apt-get install ansible          ansible-playbook ...
         │                                │
         ▼                                ▼
┌──────────────────────────────────────────────────┐
│              Shared Overlay Upper Dir             │
│  /usr/bin/ansible  ← persists ──→  still here!   │
│  /usr/lib/python3/... ← persists ──→ still here! │
└──────────────────────────────────────────────────┘
│              Host Root (read-only lower)          │
└──────────────────────────────────────────────────┘
```

## Setup

From the repository root:

```bash
# Uses the latest published GitHub release (no build required)
./examples/06-ansible-jobs/setup.sh

# Or pin a specific version
./examples/06-ansible-jobs/setup.sh v0.2.5
```

This creates:
- A 10-node Kind cluster (1 control-plane + 9 workers)
- Downloads and installs pre-built Reaper binaries from [GitHub Releases](https://github.com/miguelgila/reaper/releases)
- ConfigMap `nginx-playbook` containing the Ansible playbook

No Rust toolchain or source build is needed.

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- curl
- Ansible (`pip install ansible`)

## Running the Demo

Jobs must run in order — Job 2 depends on Ansible being present in the overlay from Job 1.

### Step 1: Install Ansible on all workers

```bash
kubectl apply -f examples/06-ansible-jobs/install-ansible-job.yaml
kubectl wait --for=condition=Complete job/install-ansible --timeout=300s
```

Check the output:

```bash
kubectl logs -l job-name=install-ansible --all-containers --prefix
```

Each pod installs Ansible via apt and prints the version to confirm success.

### Step 2: Run the Ansible playbook

```bash
kubectl apply -f examples/06-ansible-jobs/nginx-playbook-job.yaml
kubectl wait --for=condition=Complete job/nginx-playbook --timeout=300s
```

Check the output:

```bash
kubectl logs -l job-name=nginx-playbook --all-containers --prefix
```

Each pod reads the playbook from the ConfigMap, runs it with `ansible-playbook`, and the playbook:
1. Installs nginx and curl via apt
2. Creates a custom index page with the node hostname
3. Configures nginx to listen on port 8080
4. Starts nginx and verifies it responds
5. Stops nginx

### Inspect the playbook

The Ansible playbook is stored as a file in the example directory and loaded into the ConfigMap by `setup.sh`:

```bash
cat examples/06-ansible-jobs/nginx-playbook.ansible
```

Or read it from the ConfigMap:

```bash
kubectl get configmap nginx-playbook -o jsonpath='{.data.playbook\.yml}'
```

## Cleanup

```bash
# Delete Jobs and ConfigMap
kubectl delete -f examples/06-ansible-jobs/

# Delete cluster
./examples/06-ansible-jobs/setup.sh --cleanup
```
