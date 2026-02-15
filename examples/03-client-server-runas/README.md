# Client-Server Example with RunAs (Non-Root User)

Same as the [client-server](../client-server/) example, but all workloads run as a shared non-root user (`demo-svc`, UID 1500 / GID 1500). This demonstrates Reaper's `securityContext.runAsUser` / `runAsGroup` support and mimics an LDAP environment where UIDs are consistent across all nodes.

## Topology

```
┌──────────────────┐     ┌──────────────────┐     ┌──────────────────┐
│  worker (server)  │     │ worker2 (client)  │     │ worker3 (client)  │
│                   │     │                   │     │                   │
│  socat :9090      │◄────│  socat connects   │     │  socat connects   │
│  uid=1500 (demo-  │◄──────────────────────────────│  uid=1500 (demo-  │
│       svc)        │     │  uid=1500 (demo-  │     │       svc)        │
│                   │     │       svc)        │     │                   │
└──────────────────┘     └──────────────────┘     └──────────────────┘
```

All processes drop privileges from root to `demo-svc` before executing.

## What the Setup Does

1. Creates a 4-node Kind cluster
2. Creates the `demo-svc` user (UID 1500, GID 1500) **on every worker node** with the same IDs — mimicking how LDAP/AD/SSSD provides consistent UIDs across a cluster
3. Labels nodes (`role=server` / `role=client`)
4. Installs Reaper and socat on all nodes
5. Creates a ConfigMap with the server node's IP

## Setup

From the repository root:

```bash
./examples/client-server-runas/setup.sh
```

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- Ansible (`pip install ansible`)

## Running the Demo

```bash
# Start the server (drops to uid=1500)
kubectl apply -f examples/client-server-runas/server-daemonset.yaml

# Start the clients (drop to uid=1500)
kubectl apply -f examples/client-server-runas/client-daemonset.yaml

# Watch client logs — note uid=1500 in every line
kubectl logs -l app=demo-client-runas --all-containers --prefix -f
```

Expected output:

```
[pod/demo-client-runas-abc12/client] Client starting on ...-worker2 as uid=1500 gid=1500 user=demo-svc, server at 172.18.0.3:9090
[pod/demo-client-runas-abc12/client] [...-worker2 uid=1500] 14:32:05 <- Hello from ...-worker as uid=1500 — 14:32:05
[pod/demo-client-runas-xyz34/client] [...-worker3 uid=1500] 14:32:07 <- Hello from ...-worker as uid=1500 — 14:32:07
```

Server side:

```bash
kubectl logs -l app=demo-server-runas -f
```

```
Server starting on ...-worker as uid=1500 gid=1500 user=demo-svc
```

## How It Works

1. `setup.sh` creates the `demo-svc` user with identical UID/GID on every worker node, just like LDAP or SSSD would in a real cluster.
2. The pod specs set `securityContext.runAsUser: 1500` and `runAsGroup: 1500`.
3. Reaper's runtime calls `setgroups()`, `setgid()`, then `setuid()` before executing the command, dropping from root to the target user.
4. The workload runs with the privileges of `demo-svc` — it can only access files owned by or readable to UID 1500 / GID 1500.

## Cleanup

```bash
kubectl delete -f examples/client-server-runas/
./examples/client-server-runas/setup.sh --cleanup
```
