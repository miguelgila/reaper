# Kubernetes Integration for Reaper Runtime

This directory contains configuration files for integrating the Reaper containerd shim v2 with Kubernetes.

**For testing and integration workflows, see [TESTING.md](../../docs/TESTING.md).**

## Quick Start

**Unified Ansible deployment (recommended)**:

Use the Ansible-based installer for both Kind and production clusters:

### For Kind Clusters (Testing/CI)

```bash
# Install to Kind cluster using Ansible
./scripts/install-reaper.sh --kind <cluster-name>

# Dry run (preview changes)
./scripts/install-reaper.sh --kind test --dry-run

# Or run full integration test suite
./scripts/run-integration-tests.sh
```

See [TESTING.md](../../docs/TESTING.md) for full details.

### For Production Clusters

```bash
# 1. Create inventory
cp deploy/ansible/inventory.ini.example deploy/ansible/inventory.ini
# Edit deploy/ansible/inventory.ini with your nodes

# 2. Test connectivity
ansible -i deploy/ansible/inventory.ini k8s_nodes -m ping

# 3. Install using wrapper script
./scripts/install-reaper.sh --inventory deploy/ansible/inventory.ini

# Or call Ansible directly
# ansible-playbook -i deploy/ansible/inventory.ini deploy/ansible/install-reaper.yml
```

See [ansible/README.md](../ansible/README.md) for complete Ansible documentation.

## Installation Options

### Option 1: Unified Ansible Installer (Recommended)

**Why use Ansible for everything?**
- **Single deployment method**: Same code path for Kind and production
- **Better tested**: Kind tests validate production deployment
- **Idempotent**: Safe to re-run without side effects
- **Rollback support**: Built-in rollback playbook
- **External orchestration**: No containerd circular dependencies

**How it works:**
- **Kind clusters**: Uses Docker connection (`ansible_connection=docker`)
- **Production clusters**: Uses SSH connection (default)
- **Same playbook**: Works with both without modification

**Quick usage:**
```bash
# Kind clusters
./scripts/install-reaper.sh --kind <cluster-name>

# Production clusters
./scripts/install-reaper.sh --inventory deploy/ansible/inventory.ini
```

See [ansible/README.md](../ansible/README.md) for:
- Inventory configuration examples (Kind and production)
- Cloud provider setup (GKE, EKS, AKS)
- Rolling updates and parallel deployment
- Rollback procedures
- Troubleshooting

### Option 2: Manual Installation

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
kubectl apply -f deploy/kubernetes/runtimeclass.yaml
```

#### 4. Test a Pod

The `runtimeclass.yaml` file includes an example pod. After applying the RuntimeClass, you can test:

```bash
kubectl get pods
kubectl logs reaper-example
```

Expected output: `Hello from Reaper runtime!`

## Integration Testing

See [TESTING.md](../../docs/TESTING.md) for:
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

See [docs/OVERLAY_DESIGN.md](../../docs/OVERLAY_DESIGN.md) for full architecture details.

## Troubleshooting

For troubleshooting guidance, see [TESTING.md](../../docs/TESTING.md#troubleshooting).