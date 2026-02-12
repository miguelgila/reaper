# Kubernetes Integration for Reaper Runtime

This directory contains configuration files for integrating the Reaper containerd shim v2 with Kubernetes.

**For testing and integration workflows, see [TESTING.md](../TESTING.md).**

## Quick Start

### For Kind Clusters (Testing/CI)

```bash
# Quick install to Kind cluster
./scripts/install-reaper.sh --kind <cluster-name>

# Or run full integration test suite
./scripts/run-integration-tests.sh
```

See [TESTING.md](../TESTING.md) for full details.

### For Production Clusters

Use Ansible for idempotent, production-ready deployment:

```bash
# 1. Create inventory
cp ansible/inventory.ini.example ansible/inventory.ini
# Edit ansible/inventory.ini with your nodes

# 2. Test connectivity
ansible -i ansible/inventory.ini k8s_nodes -m ping

# 3. Install
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml

# 4. Create RuntimeClass
kubectl apply -f kubernetes/runtimeclass.yaml
```

See [ansible/README.md](../ansible/README.md) for complete Ansible documentation.

## Installation Options

### Option 1: Ansible (Recommended for Production)

**Why Ansible?**
- External orchestration (no containerd circular dependencies)
- Idempotent (safe to re-run)
- Built-in rollback support
- Rolling updates across nodes
- Standard practice for cluster node configuration

**Quick usage:**
```bash
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml
```

See [ansible/README.md](../ansible/README.md) for:
- Inventory configuration examples
- Cloud provider setup (GKE, EKS, AKS)
- Rolling updates and parallel deployment
- Rollback procedures
- Troubleshooting

### Option 2: Shell Script (For Kind Clusters)

The `install-reaper.sh` script is optimized for Kind clusters:

```bash
# Install to Kind cluster
./scripts/install-reaper.sh --kind <cluster-name>

# Use pre-built binaries (faster for CI)
./scripts/install-reaper.sh --kind test --skip-build --binaries-path ./binaries

# Verify existing installation
./scripts/install-reaper.sh --verify-only

# Get help
./scripts/install-reaper.sh --help
```

The script automatically:
- Detects node architecture (x86_64, aarch64)
- Builds static musl binaries (or uses pre-built)
- Deploys binaries via `docker cp`
- Creates overlay filesystem directories
- Configures containerd
- Creates RuntimeClass
- Verifies installation

### Option 3: Manual Installation

If you need manual control over the installation process:

#### 1. Build and Install Binaries

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

#### 2. Configure containerd

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

#### 3. Create RuntimeClass

Apply the RuntimeClass:

```bash
kubectl apply -f kubernetes/runtimeclass.yaml
```

#### 4. Test a Pod

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