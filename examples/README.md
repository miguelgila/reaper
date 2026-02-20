# Examples

Runnable examples demonstrating Reaper's capabilities. Each example includes a `setup.sh` script that creates a Kind cluster with Reaper pre-installed.

## Prerequisites

All examples require:
- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- Ansible (`pip install ansible`)

Run all scripts from the **repository root**.

## Examples

### [01-scheduling/](01-scheduling/) — Node Scheduling Patterns

Demonstrates running workloads on all nodes vs. a labeled subset using DaemonSets with `nodeSelector`.

- **3-node cluster** (1 control-plane + 2 workers)
- All-node DaemonSet (load/memory monitor on every node)
- Subset DaemonSet (login-node monitor only on `node-role=login` nodes)

```bash
./examples/01-scheduling/setup.sh
kubectl apply -f examples/01-scheduling/all-nodes-daemonset.yaml
kubectl apply -f examples/01-scheduling/subset-nodes-daemonset.yaml
```

### [02-client-server/](02-client-server/) — TCP Client-Server Communication

Demonstrates cross-node networking with a socat TCP server on one node and clients connecting from other nodes over host networking.

- **4-node cluster** (1 control-plane + 3 workers)
- Server on `role=server` node, clients on `role=client` nodes
- Clients discover the server IP via a ConfigMap

```bash
./examples/02-client-server/setup.sh
kubectl apply -f examples/02-client-server/server-daemonset.yaml
kubectl apply -f examples/02-client-server/client-daemonset.yaml
kubectl logs -l app=demo-client --all-containers --prefix -f
```

### [03-client-server-runas/](03-client-server-runas/) — Client-Server with Non-Root User

Same as client-server, but all workloads run as a shared non-root user (`demo-svc`, UID 1500 / GID 1500), demonstrating Reaper's `securityContext.runAsUser` / `runAsGroup` support. The setup script creates the user on every node with identical IDs, mimicking an LDAP environment.

- **4-node cluster** (1 control-plane + 3 workers)
- Shared `demo-svc` user created on all nodes (UID 1500, GID 1500)
- All log output includes `uid=` to prove privilege drop

```bash
./examples/03-client-server-runas/setup.sh
kubectl apply -f examples/03-client-server-runas/server-daemonset.yaml
kubectl apply -f examples/03-client-server-runas/client-daemonset.yaml
kubectl logs -l app=demo-client-runas --all-containers --prefix -f
```

### [04-volumes/](04-volumes/) — Kubernetes Volume Mounts

Demonstrates Reaper's volume mount support across four volume types: ConfigMap, Secret, hostPath, and emptyDir. Showcases package installation (nginx) inside the overlay namespace without modifying the host.

- **2-node cluster** (1 control-plane + 1 worker)
- ConfigMap-configured nginx, read-only Secrets, hostPath file serving, emptyDir scratch workspace
- Software installed inside pod commands via overlay (host unmodified)

```bash
./examples/04-volumes/setup.sh
kubectl apply -f examples/04-volumes/configmap-nginx.yaml
kubectl logs configmap-nginx -f
```

### [05-kubemix/](05-kubemix/) — Kubernetes Workload Mix

Demonstrates running **Jobs**, **DaemonSets**, and **Deployments** simultaneously on a 10-node cluster. Each workload type targets a different set of labeled nodes, showcasing Reaper across diverse Kubernetes workload modes. All workloads read configuration from dedicated ConfigMap volumes.

- **10-node cluster** (1 control-plane + 9 workers)
- Workers partitioned: 3 batch (Jobs), 3 daemon (DaemonSets), 3 service (Deployments)
- Each workload reads config from its own ConfigMap volume

```bash
./examples/05-kubemix/setup.sh
kubectl apply -f examples/05-kubemix/
kubectl get pods -o wide
```

### [06-ansible-jobs/](06-ansible-jobs/) — Ansible Jobs

Demonstrates overlay persistence by running **sequential Jobs**: the first installs Ansible via apt, the second runs an Ansible playbook (from a ConfigMap) to install and verify nginx. Packages installed by Job 1 persist in the shared overlay for Job 2.

- **10-node cluster** (1 control-plane + 9 workers)
- Job 1: installs Ansible on all workers (persists in overlay)
- Job 2: runs Ansible playbook from ConfigMap to install nginx

```bash
./examples/06-ansible-jobs/setup.sh
kubectl apply -f examples/06-ansible-jobs/install-ansible-job.yaml
kubectl wait --for=condition=Complete job/install-ansible --timeout=300s
kubectl apply -f examples/06-ansible-jobs/nginx-playbook-job.yaml
```

## Cleanup

Each example can be cleaned up independently:

```bash
./examples/01-scheduling/setup.sh --cleanup
./examples/02-client-server/setup.sh --cleanup
./examples/03-client-server-runas/setup.sh --cleanup
./examples/04-volumes/setup.sh --cleanup
./examples/05-kubemix/setup.sh --cleanup
./examples/06-ansible-jobs/setup.sh --cleanup
```
