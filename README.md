# Reaper

[![Tests](https://github.com/miguelgi/reaper/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/miguelgi/reaper/actions/workflows/test.yml)
[![Build](https://github.com/miguelgi/reaper/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/miguelgi/reaper/actions/workflows/build.yml)
[![Coverage](https://codecov.io/gh/miguelgi/reaper/branch/main/graph/badge.svg)](https://codecov.io/gh/miguelgi/reaper)

**Reaper is a lightweight Kubernetes container runtime that executes commands directly on cluster nodes without traditional container isolation.** Think of it as a way to run host-native processes through Kubernetes' orchestration layer.

## What is Reaper?

Reaper is a containerd shim that runs processes directly on the host system while integrating with Kubernetes' workload management. Unlike traditional container runtimes that provide isolation through namespaces and cgroups, Reaper intentionally runs processes with full host access.

**Use cases:**
- Running privileged system utilities that need direct hardware access
- Cluster maintenance tasks that operate across the host filesystem
- Legacy applications that require host-level access
- Development and debugging workflows

**What Reaper provides:**
- ✅ Standard Kubernetes API (Pods, kubectl logs, kubectl exec)
- ✅ Process lifecycle management (start, stop, restart)
- ✅ Shared overlay filesystem for workload isolation from host changes
- ✅ Direct command execution on cluster nodes
- ✅ Integration with kubectl (logs, exec, describe)

**What Reaper does NOT provide:**
- ❌ Container isolation (namespaces, cgroups)
- ❌ Resource limits (CPU, memory)
- ❌ Network isolation (uses host networking)
- ❌ Container image pulling

## Quick Start

### 1. Install Reaper on a Kubernetes Cluster

**For Kind clusters (testing/CI):**
```bash
# Install Ansible if not already installed
pip install ansible  # or: brew install ansible

# Install to Kind cluster
./scripts/install-reaper.sh --kind <cluster-name>
```

**For production clusters:**
```bash
# Create inventory file (see ansible/inventory.ini.example)
vim inventory.ini

# Install via Ansible
ansible-playbook -i inventory.ini ansible/install-reaper.yml
```

See [kubernetes/README.md](kubernetes/README.md) for detailed installation instructions.

### 2. Run a Command on the Host

Create a Pod with `runtimeClassName: reaper-v2`:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-task
spec:
  runtimeClassName: reaper-v2  # Use Reaper runtime
  restartPolicy: Never
  containers:
    - name: task
      image: placeholder  # Image field is ignored by Reaper
      command: ["/bin/sh", "-c"]
      args: ["echo Hello from host && uname -a"]
      env:
        - name: MY_VAR
          value: "example"
```

Apply and check results:

```bash
kubectl apply -f my-task.yaml
kubectl logs my-task
kubectl get pod my-task  # Status: Completed
```

### 3. Interactive Sessions

Reaper supports interactive containers:

```bash
# Run interactive shell
kubectl run -it debug --rm --image=placeholder --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"reaper-v2"}}' \
  -- /bin/bash

# Exec into running containers
kubectl exec -it my-pod -- /bin/sh
```

## Important: Pod Field Compatibility

Reaper implements the Kubernetes Pod API but **ignores or doesn't support certain container-specific fields**:

| Pod Field | Behavior |
|-----------|----------|
| `spec.containers[].image` | **Ignored** — Set to `placeholder` for clarity. Reaper doesn't pull images. |
| `spec.containers[].resources.limits` | **Ignored** — No cgroup enforcement; processes use host resources. |
| `spec.containers[].resources.requests` | **Ignored** — Scheduling hints not used. |
| `spec.containers[].volumeMounts` | **Not implemented** — No volume mounting support currently. |
| `spec.containers[].securityContext.capabilities` | **Ignored** — Processes run with host-level capabilities. |
| `spec.containers[].livenessProbe` | **Ignored** — No health checking. |
| `spec.containers[].readinessProbe` | **Ignored** — No readiness checks. |
| `spec.containers[].command` | ✅ **Supported** — Program path on host (must exist). |
| `spec.containers[].args` | ✅ **Supported** — Arguments to the command. |
| `spec.containers[].env` | ✅ **Supported** — Environment variables. |
| `spec.containers[].workingDir` | ✅ **Supported** — Working directory for the process. |
| `spec.runtimeClassName` | ✅ **Required** — Must be set to `reaper-v2`. |

**Best practice:** Set `image: placeholder` to make it explicit that the image field is not used.

## Architecture Overview

Reaper consists of three components:

```
Kubernetes/containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2  (shim binary)
        ↓ (exec: create/start/state/delete)
reaper-runtime  (OCI runtime binary)
        ↓ (fork + spawn)
monitoring daemon → workload process
```

**Key features:**
- **Fork-first architecture**: Daemon monitors workload, captures real exit codes
- **Shared overlay filesystem**: All Reaper workloads share a writable overlay layer (host root is read-only)
- **PTY support**: Interactive containers work with `kubectl run -it` and `kubectl exec -it`
- **State persistence**: Process lifecycle state stored in `/run/reaper/<container-id>/`

For architecture details, see [docs/SHIMV2_DESIGN.md](docs/SHIMV2_DESIGN.md) and [docs/OVERLAY_DESIGN.md](docs/OVERLAY_DESIGN.md).

## Features

- ✅ **Full OCI runtime implementation** (create, start, state, kill, delete)
- ✅ **Containerd shim v2 protocol** (Task trait with all lifecycle methods)
- ✅ **Kubernetes integration** via RuntimeClass
- ✅ **Overlay filesystem namespace** (protects host from modifications)
- ✅ **Container I/O capture** (stdout/stderr via FIFOs for `kubectl logs`)
- ✅ **Interactive sessions** (PTY support for `kubectl run -it` and `kubectl exec -it`)
- ✅ **Process monitoring** (fork-based with real exit code capture)
- ✅ **Zombie process reaping** (proper process cleanup)
- ✅ **End-to-end testing** (validated with kind cluster integration tests)

## Documentation

- **[kubernetes/README.md](kubernetes/README.md)** - Installation and Kubernetes integration
- **[TESTING.md](TESTING.md)** - Testing guide (unit tests, integration tests, coverage)
- **[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)** - Development setup, tooling, and contributing
- **[docs/SHIMV2_DESIGN.md](docs/SHIMV2_DESIGN.md)** - Shim v2 protocol implementation details
- **[docs/OVERLAY_DESIGN.md](docs/OVERLAY_DESIGN.md)** - Overlay filesystem design and architecture
- **[docs/CURRENT_STATE.md](docs/CURRENT_STATE.md)** - Current implementation status and known issues

## Requirements

- **Linux kernel** with overlayfs support (standard since 3.18)
- **Kubernetes cluster** with containerd runtime
- **Root access** on cluster nodes (required for containerd shim installation)

**Note:** Overlay filesystem is Linux-only. On macOS, the runtime compiles but overlay features are disabled.

## Testing

```bash
# Unit tests (fast, runs locally)
cargo test

# Full integration tests (Kubernetes + unit tests)
./scripts/run-integration-tests.sh

# For complete testing guidance, see TESTING.md
```

## Configuration

### Overlay Filesystem

All Reaper workloads share a single overlay filesystem:

```bash
# Custom overlay location (default: /run/reaper/overlay)
export REAPER_OVERLAY_BASE=/custom/path
```

The host root is the read-only lower layer; writes go to a shared upper layer. This means workloads can share files with each other while protecting the host from modifications.

For details, see [docs/OVERLAY_DESIGN.md](docs/OVERLAY_DESIGN.md).

### Runtime Logging

Enable runtime logging for debugging:

```bash
export REAPER_SHIM_LOG=/var/log/reaper-shim.log
export REAPER_RUNTIME_LOG=/var/log/reaper-runtime.log
```

## Contributing

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for:
- Development environment setup
- Code formatting and linting
- Git hooks and pre-commit checks
- CI/CD workflows

## License

MIT
