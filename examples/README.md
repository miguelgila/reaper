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

## Cleanup

Each example can be cleaned up independently:

```bash
./examples/01-scheduling/setup.sh --cleanup
./examples/02-client-server/setup.sh --cleanup
./examples/03-client-server-runas/setup.sh --cleanup
```
