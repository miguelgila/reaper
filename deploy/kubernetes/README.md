# Kubernetes Integration for Reaper Runtime

This directory contains configuration files for integrating the Reaper containerd shim v2 with Kubernetes.

**For testing and integration workflows, see [TESTING.md](../../docs/TESTING.md).**

## Quick Start

### Via Helm (recommended)

```bash
helm upgrade --install reaper deploy/helm/reaper/ \
  --namespace reaper-system --create-namespace \
  --wait --timeout 120s
```

This installs the node DaemonSet (binary installer), CRD, controller, RuntimeClass, and RBAC.

See [Helm chart README](../helm/reaper/README.md) for configuration options.

### For Testing/CI

```bash
# Playground (creates Kind cluster + installs via Helm)
./scripts/setup-playground.sh

# Full integration test suite
./scripts/run-integration-tests.sh
```

See [TESTING.md](../../docs/TESTING.md) for full details.

### Via Ansible (deprecated)

> **DEPRECATED**: Use the Helm chart instead. See [ansible/README.md](../ansible/README.md).

```bash
./scripts/install-reaper.sh --kind <cluster-name>
```

### Manual Installation

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
  pod_annotations = ["reaper.runtime/*"]
```

> **Important:** The `pod_annotations` line is required for per-pod annotation overrides (e.g., `reaper.runtime/dns-mode`, `reaper.runtime/overlay-name`). Without it, containerd will not propagate pod annotations to the OCI config and annotations will be silently ignored.

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

## Configuration

Reaper reads configuration from `/etc/reaper/reaper.conf` on each node. The Helm chart creates this file automatically via the node DaemonSet init container. Environment variables of the same name override file values.

```ini
# /etc/reaper/reaper.conf — written by Helm node DaemonSet
REAPER_DNS_MODE=kubernetes
REAPER_RUNTIME_LOG=/run/reaper/runtime.log
```

Common settings:

| Variable | Default | Description |
|----------|---------|-------------|
| `REAPER_DNS_MODE` | `host` | DNS mode: `host` (node resolv.conf) or `kubernetes` (CoreDNS) |
| `REAPER_RUNTIME_LOG` | (none) | Runtime log file path |
| `REAPER_SHIM_LOG` | (none) | Shim log file path |
| `REAPER_OVERLAY_BASE` | `/run/reaper/overlay` | Overlay filesystem base directory |
| `REAPER_FILTER_ENABLED` | `true` | Enable sensitive host file filtering |
| `REAPER_ANNOTATIONS_ENABLED` | `true` | Enable per-pod annotation overrides (set `false` to ignore all annotations) |

**Per-pod annotation overrides:** Users can set `reaper.runtime/dns-mode` and `reaper.runtime/overlay-name` on individual pods to override node-level settings. `overlay-name` creates isolated overlay groups within a namespace. See the main [README.md](../../README.md#pod-annotations) for details and examples. Administrators can disable all annotations with `REAPER_ANNOTATIONS_ENABLED=false`.

For manual installations, create the file:

```bash
sudo mkdir -p /etc/reaper
sudo tee /etc/reaper/reaper.conf << 'EOF'
REAPER_DNS_MODE=kubernetes
REAPER_RUNTIME_LOG=/run/reaper/runtime.log
EOF
sudo systemctl restart containerd
```

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