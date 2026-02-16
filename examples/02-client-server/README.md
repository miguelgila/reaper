# Client-Server Example

Demonstrates a TCP server and multiple clients running across Kubernetes nodes using Reaper. The server and clients communicate over the host network using `socat`, which is available on all Kubernetes nodes.

## Topology

```
┌──────────────────┐     ┌──────────────────┐     ┌──────────────────┐
│  worker (server)  │     │ worker2 (client)  │     │ worker3 (client)  │
│                   │     │                   │     │                   │
│  socat listening  │◄────│  socat connects   │     │  socat connects   │
│  on port 9090     │◄──────────────────────────────│  every 5 seconds  │
└──────────────────┘     └──────────────────┘     └──────────────────┘
```

- **Server** responds with its hostname and a timestamp on each connection.
- **Clients** connect every 5 seconds and log the response.
- All processes run directly on the host via Reaper (no container isolation).

## Setup

From the repository root:

```bash
./examples/client-server/setup.sh
```

This creates a 4-node Kind cluster (`reaper-client-server-demo`) with:
- 1 control-plane node
- 1 worker labeled `role=server`
- 2 workers labeled `role=client`
- Reaper runtime installed on all nodes
- A `server-config` ConfigMap containing the server node's internal IP

### Prerequisites

- Docker
- [kind](https://kind.sigs.k8s.io/)
- kubectl
- Ansible (`pip install ansible`)

## Running the Demo

```bash
# Start the server (waits for connections on port 9090)
kubectl apply -f examples/client-server/server-daemonset.yaml

# Start the clients (connect to the server every 5 seconds)
kubectl apply -f examples/client-server/client-daemonset.yaml

# Watch client logs — each client reports the server's response
kubectl logs -l app=demo-client --all-containers --prefix -f
```

Expected output:

```
[pod/demo-client-abc12/client] Client starting on reaper-client-server-demo-worker2, server at 172.18.0.3:9090
[pod/demo-client-abc12/client] [reaper-...-worker2] 14:32:05 <- Hello from reaper-...-worker — 14:32:05
[pod/demo-client-xyz34/client] [reaper-...-worker3] 14:32:07 <- Hello from reaper-...-worker — 14:32:07
```

Check the server side:

```bash
kubectl logs -l app=demo-server -f
```

## How It Works

1. **Server** runs a `socat` TCP listener on port 9090. On each connection it responds with its hostname and the current time.
2. **Clients** read the server's IP from the `server-config` ConfigMap (injected as the `SERVER_IP` environment variable) and connect using `socat`.
3. Since Reaper uses host networking, the server listens on the node's real IP and clients connect directly to it — no Kubernetes Service or port-forward required.

## Cleanup

```bash
# Remove the workloads
kubectl delete -f examples/client-server/

# Delete the Kind cluster
./examples/client-server/setup.sh --cleanup
```
