# Kubernetes Integration for Reaper Runtime

This directory contains configuration files and examples for integrating the Reaper containerd shim v2 with Kubernetes.

## Prerequisites

- Kubernetes cluster (minikube, kind, or production)
- containerd as the container runtime
- Reaper shim binary installed on nodes

## Installation

### 1. Install Reaper Binaries

Build and install both binaries on all Kubernetes nodes:

```bash
cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime
sudo cp target/release/containerd-shim-reaper-v2 /usr/local/bin/
sudo cp target/release/reaper-runtime /usr/local/bin/
sudo chmod +x /usr/local/bin/containerd-shim-reaper-v2 /usr/local/bin/reaper-runtime
```

### 2. Configure containerd

Add the Reaper runtime to containerd configuration on all nodes:

```bash
sudo mkdir -p /etc/containerd
sudo tee -a /etc/containerd/config.toml > /dev/null <<EOF
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
  runtime_type = "io.containerd.reaper.v2"
  sandbox_mode = "podsandbox"
EOF
```

Restart containerd:

```bash
sudo systemctl restart containerd
```

> **Note:** The shim binary (`containerd-shim-reaper-v2`) must be in `$PATH` (e.g., `/usr/local/bin/`). Containerd discovers shims by name convention, not by explicit path.

### 3. Label Nodes (Optional)

Label nodes that should run Reaper workloads:

```bash
kubectl label nodes <node-name> reaper-runtime=true
```

### 4. Create RuntimeClass

Apply the RuntimeClass configuration:

```bash
kubectl apply -f runtimeclass.yaml
```

## Testing

### Basic Pod Test

Run the example pod:

```bash
kubectl apply -f runtimeclass.yaml
kubectl logs -f reaper-example
```

Expected output: `Hello from Reaper runtime!`

### End-to-End Testing

1. Create a pod with the reaper runtime
2. Verify pod status: `kubectl get pods` (should reach `Completed` for one-shot tasks)
3. Check logs: `kubectl logs <pod-name>`
4. Test deletion: `kubectl delete pod <pod-name>`

Or use the automated kind integration script:
```bash
./scripts/run-integration-tests.sh
```

> **Note:** `kubectl exec` is not supported — the shim returns UNIMPLEMENTED for exec requests.

## Configuration Files

- `runtimeclass.yaml`: Kubernetes RuntimeClass definition and example pod
- `containerd-config.toml`: containerd runtime configuration snippet

## Troubleshooting

### Pod Stuck in Pending

- Check node labels match RuntimeClass nodeSelector
- Verify containerd configuration is correct
- Check shim binary is executable and in PATH

### Pod Stuck in ContainerCreating

- Check containerd logs: `journalctl -u containerd`
- Verify shim binary path in containerd config
- Ensure bundle directory exists with config.json

### Pod Failed with "sandbox container is not running"

- This was a known bug fixed in February 2026
- Ensure you are using the latest shim binary where sandbox `wait()` blocks until `kill()`
- Rebuild and redeploy the shim: `cargo build --release && sudo cp target/release/containerd-shim-reaper-v2 /usr/local/bin/`

### Command Execution Issues

- Verify config.json format in bundle
- Check command permissions on host
- Review shim logs for TTRPC errors (set `REAPER_SHIM_LOG=/var/log/reaper-shim.log`)

## Overlay Filesystem

By default, all Reaper workloads share a mount namespace with an overlay filesystem:
- The host root is the read-only lower layer
- Writes go to a shared upper layer at `/run/reaper/overlay/upper`
- Cross-deployment file sharing works (workload A writes → workload B reads)
- Host filesystem is protected from modifications

Overlay is mandatory — workloads cannot run without filesystem isolation.

See `docs/OVERLAY_DESIGN.md` for full architecture details.

## Notes

- Reaper executes commands directly on the host (through an overlay by default)
- The overlay protects the host filesystem; it is always enabled on Linux
- Monitor host resources as commands share the node's resources