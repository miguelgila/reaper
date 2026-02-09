# Kubernetes Deployment for Reaper Shim v2

This directory contains resources for deploying the Reaper container runtime shim v2 to Kubernetes clusters.

## Files

- **`runtimeclass.yaml`**: Defines the `reaper-v2` RuntimeClass and includes an example Pod
- **`containerd-config.toml`**: Documentation showing the containerd configuration needed

## Quick Start

### Prerequisites

1. Build the shim binary:
   ```bash
   cargo build --release --bin containerd-shim-reaper-v2
   ```

2. The shim binary must be installed on all nodes at `/usr/local/bin/containerd-shim-reaper-v2`

### Deployment Options

#### Option 1: Using minikube (Recommended for testing)

```bash
./scripts/minikube-setup-runtime.sh
```

This script will:
- Start minikube with containerd
- Build the shim binary for the correct architecture
- Copy the binary to the minikube node
- Configure containerd automatically
- Apply the RuntimeClass
- Create an example pod

#### Option 2: Using kind

```bash
./scripts/run-integration-tests.sh
```

#### Option 3: Manual deployment

1. Copy the shim binary to each node:
   ```bash
   scp target/release/containerd-shim-reaper-v2 node:/usr/local/bin/
   ssh node chmod +x /usr/local/bin/containerd-shim-reaper-v2
   ```

2. Configure containerd on each node:
   ```bash
   ./scripts/configure-containerd.sh
   ```

3. Apply the RuntimeClass:
   ```bash
   kubectl apply -f kubernetes/runtimeclass.yaml
   ```

## Configuration Details

### Containerd Configuration

The shim requires specific containerd configuration. Use the unified configuration script:

```bash
# For minikube
./scripts/configure-containerd.sh minikube

# For kind (provide node container ID)
./scripts/configure-containerd.sh kind <node-id>

# For local system
./scripts/configure-containerd.sh
```

**Important**: Do NOT use the `[options]` section in the runtime configuration. This causes a cgroup path bug in the containerd-shim library. The correct configuration is:

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
  runtime_type = "io.containerd.reaper.v2"
  sandbox_mode = "podsandbox"
```

Containerd will auto-discover the shim binary by name (`containerd-shim-reaper-v2`).

## Example Pod

The `runtimeclass.yaml` file includes an example pod that uses the reaper runtime:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: reaper-example
spec:
  runtimeClassName: reaper-v2
  containers:
    - name: test
      image: busybox
      command: ["/bin/echo", "Hello from Reaper runtime!"]
```

## Troubleshooting

### Pod stuck in ContainerCreating

Check containerd logs:
```bash
# Minikube
minikube ssh -- "sudo journalctl -u containerd -n 50 | grep -i error"

# Kind
docker exec <node-id> journalctl -u containerd -n 50 | grep -i error
```

### Binary not found error

Ensure the shim is executable:
```bash
# Minikube
minikube ssh -- "ls -la /usr/local/bin/containerd-shim-reaper-v2"

# Kind
docker exec <node-id> ls -la /usr/local/bin/containerd-shim-reaper-v2
```

### Cgroup errors

This usually means the containerd config has an `[options]` section. Remove it and use only:
```toml
runtime_type = "io.containerd.reaper.v2"
sandbox_mode = "podsandbox"
```

## Current Status

⚠️ **Known Issue**: The shim currently exits with "Env(NotPresent)" error during initialization. This is being debugged. The shim binary runs but needs proper environment setup from containerd's spawn() function.

## See Also

- [../scripts/minikube-setup-runtime.sh](../scripts/minikube-setup-runtime.sh) - Minikube deployment
- [../scripts/run-integration-tests.sh](../scripts/run-integration-tests.sh) - Kind integration tests
- [../scripts/configure-containerd.sh](../scripts/configure-containerd.sh) - Unified containerd configuration
- [../examples/k8s/](../examples/k8s/) - Additional example manifests
