# Kubernetes Integration for Reaper Runtime

This directory contains configuration files for integrating the Reaper containerd shim v2 with Kubernetes.

**For testing and integration workflows, see [TESTING.md](../TESTING.md).**

## Quick Start

The recommended way to test Reaper with Kubernetes is using the automated integration test suite:

```bash
./scripts/run-integration-tests.sh
```

This orchestrates everything: building binaries, creating a kind cluster, configuring containerd, and running all tests. See [TESTING.md](../TESTING.md) for full details.

## Manual Deployment

If you need to deploy to an existing Kubernetes cluster:

### 1. Build and Install Binaries

Build both binaries locally:

```bash
cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime
```

Copy to each Kubernetes node:

```bash
scp target/release/containerd-shim-reaper-v2 <node>:/usr/local/bin/
scp target/release/reaper-runtime <node>:/usr/local/bin/
ssh <node> chmod +x /usr/local/bin/{containerd-shim-reaper-v2,reaper-runtime}
```

### 2. Configure containerd

Use the automated configuration script:

```bash
./scripts/configure-containerd.sh kind <node-id>
```

Or configure manually by adding this to `/etc/containerd/config.toml`:

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
  runtime_type = "io.containerd.reaper.v2"
  sandbox_mode = "podsandbox"
```

Then restart containerd:

```bash
sudo systemctl restart containerd
```

> **Important:** The shim binary must be in `$PATH` at `/usr/local/bin/`. Containerd discovers shims by name convention, not by explicit path. Do NOT use `[options]` sections in the runtime config—this causes cgroup path bugs.

### 3. Create RuntimeClass

Apply the RuntimeClass:

```bash
kubectl apply -f kubernetes/runtimeclass.yaml
```

### 4. Test a Pod

The `runtimeclass.yaml` file includes an example pod. After applying the RuntimeClass, you can test:

```bash
kubectl get pods
kubectl logs reaper-example
```

Expected output: `Hello from Reaper runtime!`

## Integration Testing

See [TESTING.md](../TESTING.md) for:
- Running the full integration test suite
- Available test options
- Troubleshooting guide

## Files

- **`runtimeclass.yaml`** — Kubernetes RuntimeClass definition with example pod

## Architecture

By default, all Reaper workloads share a mount namespace with an overlay filesystem for host protection:
- Host root is the read-only lower layer
- Writes go to a shared upper layer at `/run/reaper/overlay/upper`
- Host filesystem is protected from modifications
- Overlay is mandatory—workloads cannot run without filesystem isolation

See [docs/OVERLAY_DESIGN.md](../docs/OVERLAY_DESIGN.md) for full architecture details.

## Troubleshooting

For troubleshooting guidance, see [TESTING.md](../TESTING.md#troubleshooting).