# Kubernetes Integration for Reaper Runtime

This directory contains configuration files and examples for integrating the Reaper containerd shim v2 with Kubernetes.

## Prerequisites

- Kubernetes cluster (minikube, kind, or production)
- containerd as the container runtime
- Reaper shim binary installed on nodes

## Installation

### 1. Install Reaper Shim Binary

Build and install the shim binary on all Kubernetes nodes:

```bash
cargo build --release --bin containerd-shim-reaper-v2
sudo cp target/release/containerd-shim-reaper-v2 /usr/local/bin/
sudo chmod +x /usr/local/bin/containerd-shim-reaper-v2
```

### 2. Configure containerd

Add the Reaper runtime to containerd configuration on all nodes:

```bash
sudo mkdir -p /etc/containerd
sudo tee -a /etc/containerd/config.toml > /dev/null <<EOF
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
  runtime_type = "io.containerd.reaper.v2"
  [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2.options]
    BinaryName = "/usr/local/bin/containerd-shim-reaper-v2"
EOF
```

Restart containerd:

```bash
sudo systemctl restart containerd
```

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
2. Verify pod status: `kubectl get pods`
3. Check logs: `kubectl logs <pod-name>`
4. Test exec: `kubectl exec -it <pod-name> -- /bin/sh`
5. Test deletion: `kubectl delete pod <pod-name>`

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

### Command Execution Issues

- Verify config.json format in bundle
- Check command permissions on host
- Review shim logs for TTRPC errors

## Notes

- Reaper executes commands directly on the host (no container isolation)
- Ensure commands are safe to run with host privileges
- Monitor host resources as commands share the node's resources