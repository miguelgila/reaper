# Reaper

[![CI](https://github.com/miguelgila/reaper/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/miguelgila/reaper/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/miguelgila/reaper/branch/main/graph/badge.svg)](https://codecov.io/gh/miguelgila/reaper)

**Reaper is a lightweight Kubernetes container-less runtime that executes commands directly on cluster nodes without traditional container isolation.** Think of it as a way to run host-native processes through Kubernetes' orchestration layer.

## Disclaimer

Reaper is an experimental, personal project built to explore what's possible with AI-assisted development. It is under continuous development with no stability guarantees — there is no assurance it will work correctly in your environment. No support of any kind is provided. Unless you fully understand what Reaper does and how it works, you probably don't want to run it. Use entirely at your own risk. That said, the code is open — feel free to read it and send PRs.

## What is Reaper?

Reaper is a containerd shim that runs processes directly on the host system while integrating with Kubernetes' workload management. Unlike traditional container runtimes that provide isolation through namespaces and cgroups, Reaper intentionally runs processes with full host access.

**What Reaper provides:**
- Standard Kubernetes API (Pods, kubectl logs, kubectl exec)
- Process lifecycle management (start, stop, restart)
- Shared overlay filesystem for workload isolation from host changes
- Kubernetes volumes (ConfigMap, Secret, hostPath, emptyDir)
- Sensitive host file filtering (SSH keys, passwords, SSL keys)
- Interactive sessions (PTY support for `kubectl run -it` and `kubectl exec -it`)
- UID/GID switching with `securityContext`
- Per-pod configuration via Kubernetes annotations
- Custom Resource Definitions: [ReaperPod](docs/book/src/reference/crds.md#reaperpod) (simplified workloads), [ReaperOverlay](docs/book/src/reference/crds.md#reaperoverlay) (overlay lifecycle), [ReaperDaemonJob](docs/book/src/reference/crds.md#reaperdaemonjob) (node-wide config tasks)
- [Helm chart](deploy/helm/reaper/) for one-command installation (`helm install`)

**What Reaper does NOT provide:**
- Container isolation (namespaces, cgroups)
- Resource limits (CPU, memory)
- Network isolation (uses host networking)
- Container image pulling

**Use cases:** privileged system utilities, cluster maintenance, legacy host-level applications, HPC workloads, development and debugging workflows.

## Quick Start

### Playground (try it locally)

Spin up a 3-node Kind cluster with Reaper pre-installed:

```bash
# Build from source (compiles inside Docker — no local Rust toolchain needed)
./scripts/setup-playground.sh

# Or use pre-built images from the latest release (no build)
./scripts/setup-playground.sh --release
```

Once ready:

```bash
kubectl apply -f - <<'YAML'
apiVersion: reaper.giar.dev/v1alpha1
kind: ReaperPod
metadata:
  name: hello
spec:
  command: ["/bin/sh", "-c", "echo Hello from $(hostname) && uname -a"]
YAML

kubectl logs hello
```

To tear down: `./scripts/setup-playground.sh --cleanup`

### Install on a Kubernetes Cluster

```bash
helm upgrade --install reaper deploy/helm/reaper/ \
  --namespace reaper-system --create-namespace \
  --wait --timeout 120s
```

### Build from Source

```bash
git clone https://github.com/miguelgila/reaper && cd reaper
cargo build --release
```

For cross-compilation to Linux (from macOS), see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Usage

### Run a Command on the Host

```yaml
apiVersion: reaper.giar.dev/v1alpha1
kind: ReaperPod
metadata:
  name: my-task
spec:
  command: ["/bin/sh", "-c", "echo Hello from host && uname -a"]
```

```bash
kubectl apply -f my-task.yaml
kubectl logs my-task
kubectl get reaperpods
```

### With Volumes

```yaml
apiVersion: reaper.giar.dev/v1alpha1
kind: ReaperPod
metadata:
  name: config-reader
spec:
  command: ["/bin/sh", "-c", "cat /config/settings.yaml"]
  volumes:
    - name: config
      mountPath: /config
      configMap: "my-config"
      readOnly: true
```

### With Node Selector

```yaml
apiVersion: reaper.giar.dev/v1alpha1
kind: ReaperPod
metadata:
  name: compute-task
spec:
  command: ["/bin/sh", "-c", "echo Running on $(hostname)"]
  nodeSelector:
    workload-type: compute
```

### Using Raw Pods

You can also use standard Kubernetes Pods with `runtimeClassName: reaper-v2` directly. This gives access to the full Pod API (interactive sessions, DaemonSets, Deployments, etc.). See the [Quick Start guide](docs/book/src/getting-started/quick-start.md) for details and [Pod Compatibility](docs/COMPATIBILITY.md) for supported fields.

## Architecture

```
Kubernetes/containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2  (shim binary)
        ↓ (exec: create/start/state/delete)
reaper-runtime  (OCI runtime binary)
        ↓ (fork + spawn)
monitoring daemon → workload process
```

- **Fork-first architecture**: Daemon monitors workload, captures real exit codes
- **Shared overlay filesystem**: Writable layer per K8s namespace (host root is read-only)
- **PTY support**: Interactive containers with `kubectl run -it` and `kubectl exec -it`

For architecture details, see [docs/SHIMV2_DESIGN.md](docs/SHIMV2_DESIGN.md) and [docs/OVERLAY_DESIGN.md](docs/OVERLAY_DESIGN.md).

## Configuration

Reaper reads configuration from `/etc/reaper/reaper.conf` on each node. Per-pod overrides are available via Kubernetes annotations:

```yaml
annotations:
  reaper.runtime/dns-mode: "kubernetes"
  reaper.runtime/overlay-name: "my-group"
```

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for the full reference.

## Examples

The [examples/](examples/) directory contains runnable demos, each with a `setup.sh` that creates a Kind cluster with Reaper pre-installed:

| Example | Description |
|---------|-------------|
| **[01-scheduling](examples/01-scheduling/)** | DaemonSets on all nodes vs. a labeled subset |
| **[02-client-server](examples/02-client-server/)** | TCP server + clients across nodes via host networking |
| **[03-client-server-runas](examples/03-client-server-runas/)** | Client-server running as a shared non-root user |
| **[04-volumes](examples/04-volumes/)** | Kubernetes volume mounts (ConfigMap, Secret, hostPath, emptyDir) |
| **[05-kubemix](examples/05-kubemix/)** | Jobs, DaemonSets, and Deployments on a 10-node cluster |
| **[06-ansible-jobs](examples/06-ansible-jobs/)** | Sequential Jobs: install Ansible, then run a playbook |
| **[07-ansible-complex](examples/07-ansible-complex/)** | DaemonSet bootstrap + role-based Ansible playbooks |
| **[08-mix-container-runtime-engines](examples/08-mix-container-runtime-engines/)** | Mixed runtimes: OpenLDAP (default) + SSSD (Reaper) |
| **[09-reaperpod](examples/09-reaperpod/)** | ReaperPod CRD: simplified Reaper-native workloads |
| **[10-slurm-hpc](examples/10-slurm-hpc/)** | Slurm HPC: containerized scheduler + Reaper worker daemons |
| **[11-node-monitoring](examples/11-node-monitoring/)** | Prometheus node_exporter (Reaper) + Prometheus server |

## Documentation

| Document | Description |
|----------|-------------|
| [CONFIGURATION.md](docs/CONFIGURATION.md) | Node config, environment variables, pod annotations |
| [COMPATIBILITY.md](docs/COMPATIBILITY.md) | Pod field compatibility reference |
| [SHIMV2_DESIGN.md](docs/SHIMV2_DESIGN.md) | Shim v2 protocol implementation |
| [OVERLAY_DESIGN.md](docs/OVERLAY_DESIGN.md) | Overlay filesystem design |
| [DEVELOPMENT.md](docs/DEVELOPMENT.md) | Development setup, tooling, contributing |
| [TESTING.md](docs/TESTING.md) | Testing guide (unit, integration, coverage) |
| [CONTRIBUTING.md](docs/CONTRIBUTING.md) | Contributing guidelines |
| [examples/README.md](examples/README.md) | Runnable examples with Kind clusters |

## Requirements

**Runtime (cluster nodes):** Linux kernel with overlayfs (3.18+), Kubernetes with containerd, root access on nodes.

**Playground:** [Docker](https://docs.docker.com/get-docker/), [kind](https://kind.sigs.k8s.io/), [kubectl](https://kubernetes.io/docs/tasks/tools/), [Helm](https://helm.sh/docs/intro/install/).

**Building from source:** All of the above, plus [Rust](https://www.rust-lang.org/tools/install) (version pinned in `rust-toolchain.toml`).

## Testing

```bash
cargo test                            # Unit tests (fast)
./scripts/run-integration-tests.sh    # Full integration tests (Kubernetes)
```

See [docs/TESTING.md](docs/TESTING.md) for the complete guide.

## Known Issues

- **"write on closed stream 0" warning on interactive exit**: Cosmetic race condition in containerd's CRI streaming handler during FIFO teardown. Does not affect workload exit code or pod status.

## Contributing

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) and [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## License

MIT
