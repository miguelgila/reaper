# Installation

## Helm (Recommended)

The recommended way to install Reaper on a Kubernetes cluster is via the Helm chart:

```bash
helm upgrade --install reaper deploy/helm/reaper/ \
  --namespace reaper-system --create-namespace \
  --wait --timeout 120s
```

This installs:
- **Node DaemonSet**: Copies shim + runtime binaries to every node
- **CRD Controller**: Watches ReaperPod resources and creates Pods
- **Agent DaemonSet**: Health monitoring and Prometheus metrics
- **RuntimeClass**: Registers `reaper-v2` with Kubernetes
- **RBAC**: Required roles and bindings

See [Helm Chart Reference](../reference/helm-chart.md) for configuration values.

## Playground (Local Testing)

Spin up a 3-node Kind cluster with Reaper pre-installed. No local Rust toolchain needed — compilation happens inside Docker:

```bash
# Build from source
./scripts/setup-playground.sh

# Or use pre-built images from GHCR
./scripts/setup-playground.sh --release

# Use a specific release version
./scripts/setup-playground.sh --release v0.2.14

# Clean up
./scripts/setup-playground.sh --cleanup
```

## Building from Source

Reaper requires Rust. The toolchain version is pinned in `rust-toolchain.toml` and installed automatically.

```bash
git clone https://github.com/miguelgila/reaper
cd reaper
cargo build --release
```

Binaries are output to `target/release/`.

### Cross-Compilation (macOS to Linux)

Since Reaper runs on Linux Kubernetes nodes, cross-compile static musl binaries:

```bash
# For x86_64 nodes
docker run --rm -v "$(pwd)":/work -w /work \
  messense/rust-musl-cross:x86_64-musl \
  cargo build --release --target x86_64-unknown-linux-musl

# For aarch64 nodes
docker run --rm -v "$(pwd)":/work -w /work \
  messense/rust-musl-cross:aarch64-musl \
  cargo build --release --target aarch64-unknown-linux-musl
```

## Requirements

**Runtime (cluster nodes):**
- Linux kernel with overlayfs support (standard since 3.18)
- Kubernetes cluster with containerd runtime
- Root access on cluster nodes

**Playground:**
- [Docker](https://docs.docker.com/get-docker/)
- [kind](https://kind.sigs.k8s.io/)
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- [Helm](https://helm.sh/docs/intro/install/)

**Building from source:**
- All of the above, plus [Rust](https://www.rust-lang.org/tools/install)
