# Reaper Project - Claude Code Instructions

This file contains important project-specific context and instructions for Claude Code.

## Project Overview

**Reaper** is a lightweight Kubernetes container runtime that executes commands directly on cluster nodes without traditional container isolation. It implements the containerd shim v2 protocol to integrate with Kubernetes while running processes with full host access.

### What Reaper Does
- ✅ Executes commands directly on Kubernetes nodes (no traditional container isolation)
- ✅ Provides shared overlay filesystem to protect host from workload modifications
- ✅ Supports Kubernetes volumes (ConfigMap, Secret, hostPath, emptyDir) via OCI bind mounts
- ✅ Integrates with Kubernetes API (Pods, kubectl logs, kubectl exec)
- ✅ Supports interactive containers with PTY (kubectl run -it, kubectl exec -it)
- ✅ Captures real exit codes and process lifecycle events

### What Reaper Does NOT Do
- ❌ Container isolation (namespaces, cgroups)
- ❌ Resource limits (CPU, memory)
- ❌ Network isolation (uses host networking)
- ❌ Container image pulling

### Use Cases
- Privileged system utilities requiring direct hardware access
- Cluster maintenance tasks across host filesystem
- Legacy applications requiring host-level access
- Development and debugging workflows

## Architecture

### Three-Tier System

```
Kubernetes/containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2  (long-lived shim, implements Task trait)
        ↓ (exec: create/start/state/delete/kill)
reaper-runtime  (short-lived OCI runtime CLI)
        ↓ (fork FIRST, then spawn)
monitoring daemon → spawns workload → wait() → captures exit code
```

**Key Design Decisions:**
- **Fork-first architecture**: Runtime forks FIRST, then spawned workload becomes daemon's child. This allows daemon to call `wait()` and capture real exit codes (only parent can wait on child).
- **Overlay namespace**: All workloads share ONE mount namespace with overlayfs. Created lazily by first workload, persisted via bind-mount of `/proc/<pid>/ns/mnt`.
- **Inner fork for namespace persistence**: Bind-mounting a namespace file to host path MUST be done from HOST mount namespace. After `unshare(CLONE_NEWNS)`, bind-mounts don't propagate. Solution: inner fork where child creates namespace, parent (host ns) bind-mounts it.

### Critical Implementation Details

See [MEMORY.md](.claude/projects/-Users-miguelgi-Documents-CODE-Explorations-reaper/memory/MEMORY.md) for detailed architecture decisions and common pitfalls.

**Fork-First Architecture (CRITICAL):**
- Runtime forks → creates daemon → parent exits
- Daemon calls `setsid()` to detach
- Daemon spawns workload (daemon is parent!)
- Daemon calls `child.wait()` to capture exit code
- Daemon updates state file, then exits

**Why this works:**
- `std::process::Child` handle is valid (created by daemon, not transferred across fork)
- Daemon is workload's parent → can call `wait()`
- Proper zombie reaping
- Real exit codes captured

### Overlay Filesystem

All Reaper workloads share a single overlay namespace:

```
Host Root (/) ─── read-only lower layer
                      │
              ┌───────┴────────┐
              │   OverlayFS    │
              │  merged view   │
              └───────┬────────┘
                      │
    /run/reaper/overlay/upper ─── shared writable layer
```

- Reads fall through to host root
- Writes go to shared upper layer
- Host filesystem never modified (mandatory isolation)
- Uses `pivot_root` to preserve `/proc`, `/sys`, `/dev`
- `/tmp` is NOT bind-mounted (protected by overlay)

**Configuration:**
- `REAPER_OVERLAY_BASE`: Default `/run/reaper/overlay`
- Overlay is mandatory on Linux (no fail-open)
- Not available on macOS (code gated with `#[cfg(target_os = "linux")]`)

### Volume Mounts

Kubernetes volumes (ConfigMap, Secret, hostPath, emptyDir, etc.) are supported via OCI bind mounts. Kubelet prepares volume content as host directories, and containerd writes bind-mount directives to the OCI `config.json` `mounts` array. Reaper reads this array and performs bind mounts inside the overlay namespace.

**How it works:**
1. OCI `config.json` mounts are parsed into `OciMount` structs
2. Non-bind mounts (proc, sysfs, tmpfs, etc.) are filtered out (already handled by overlay)
3. Kubernetes-internal mounts (`/etc/hosts`, `/etc/hostname`, `/etc/resolv.conf`, `/dev/termination-log`) are skipped
4. Remaining bind mounts are applied inside the shared overlay namespace after `enter_overlay()`

**Key details:**
- Volume mounts are shared across all workloads (same shared namespace, no per-container isolation)
- Mount failures are fatal — workload refuses to start (same pattern as overlay failure)
- Read-only mounts (`"ro"` in options) are remounted with `MS_RDONLY`
- `do_exec()` does NOT need to re-apply volume mounts — they persist in the shared namespace

## Project Structure

```
reaper/
├── src/bin/
│   ├── containerd-shim-reaper-v2/
│   │   └── main.rs              # Shim implementation (ttrpc server, Task trait)
│   └── reaper-runtime/
│       ├── main.rs              # OCI runtime CLI (fork-first architecture)
│       ├── state.rs             # State persistence (/run/reaper/<id>/)
│       └── overlay.rs           # Overlay filesystem (Linux-only)
├── tests/                       # Integration tests
│   ├── integration_basic_binary.rs
│   ├── integration_io.rs        # FIFO stdout/stderr
│   ├── integration_exec.rs      # Exec support
│   ├── integration_overlay.rs   # Overlay filesystem
│   ├── integration_shim.rs      # Shim protocol
│   └── integration_user_management.rs
├── examples/                    # Runnable Kind-based demos
│   ├── 01-scheduling/           # DaemonSet on all/subset of nodes
│   ├── 02-client-server/        # TCP server + clients across nodes
│   ├── 03-client-server-runas/  # Same as above, running as non-root user
│   └── 04-volumes/              # Kubernetes volume mounts with overlay
├── scripts/
│   ├── run-integration-tests.sh # Full integration test suite
│   └── install-reaper.sh        # Installation script (Ansible wrapper)
├── ansible/
│   └── install-reaper.yml       # Deployment playbook
├── kubernetes/
│   └── runtimeclass.yaml        # RuntimeClass definition
└── docs/
    ├── SHIMV2_DESIGN.md         # Shim v2 protocol implementation
    ├── SHIM_ARCHITECTURE.md     # Architecture deep-dive
    ├── OVERLAY_DESIGN.md        # Overlay filesystem design
    ├── DEVELOPMENT.md           # Development guide
    └── CURRENT_STATE.md         # ⚠️ OUTDATED - refer to SHIMV2_DESIGN.md
```

## Key Files by Task

**For runtime changes (fork, exec, lifecycle):**
- [src/bin/reaper-runtime/main.rs](src/bin/reaper-runtime/main.rs) - especially `do_start()`, `do_kill()`, `do_exec()`
- [src/bin/reaper-runtime/state.rs](src/bin/reaper-runtime/state.rs) - state file management
- [src/bin/reaper-runtime/overlay.rs](src/bin/reaper-runtime/overlay.rs) - overlay filesystem (Linux)

**For shim changes (containerd integration):**
- [src/bin/containerd-shim-reaper-v2/main.rs](src/bin/containerd-shim-reaper-v2/main.rs) - Task trait implementation

**For testing:**
- [scripts/run-integration-tests.sh](scripts/run-integration-tests.sh) - full test suite
- [TESTING.md](TESTING.md) - comprehensive testing guide

## CI/CD and Integration Testing

### Permission Issues in GitHub Actions

**Problem**: In GitHub Actions CI, the `target/` directory is often cached and owned by a different user than the current workflow step. This causes "Permission denied" errors when trying to copy binaries to `target/release/`.

**Solution**: The integration test scripts detect CI mode via the `CI` environment variable and use binaries directly from `target/<target-triple>/release/` without copying them. This is controlled by the `REAPER_BINARY_DIR` environment variable.

- **CI mode** (`CI=true`): Uses binaries from `target/<target-triple>/release/` directly
- **Local mode**: Copies binaries to `target/release/` for convenience

Key environment variables:
- `CI`: Set by GitHub Actions automatically. Enables CI-specific behavior.
- `REAPER_BINARY_DIR`: Override the binary directory location for Ansible installer.

Files involved:
- [scripts/run-integration-tests.sh](scripts/run-integration-tests.sh): Detects CI mode and sets `REAPER_BINARY_DIR`
- [scripts/install-reaper.sh](scripts/install-reaper.sh): Accepts `REAPER_BINARY_DIR` and passes it to Ansible
- [ansible/install-reaper.yml](ansible/install-reaper.yml): Uses `local_binary_dir` variable (set from `REAPER_BINARY_DIR`)

### Building Binaries for Integration Tests

The integration tests build static musl binaries using Docker to ensure compatibility with Kind nodes:

```bash
# Detects node architecture (x86_64 or aarch64)
docker run --rm \
  -v "$(pwd)":/work \
  -w /work \
  messense/rust-musl-cross:<arch>-musl \
  cargo build --release --target <target-triple>
```

This produces binaries at `target/<target-triple>/release/` that work in Kind's container environment.

## Architecture Notes

See [MEMORY.md](.claude/projects/-Users-miguelgi-Documents-CODE-Explorations-reaper/memory/MEMORY.md) for key architecture decisions and common pitfalls.

## Integration Test Structure

The integration test suite ([scripts/run-integration-tests.sh](scripts/run-integration-tests.sh)) has four phases:

1. **Phase 1**: Rust cargo tests (unit and integration tests)
2. **Phase 2**: Infrastructure setup (Kind cluster, build binaries, install Reaper via Ansible)
3. **Phase 3**: Kubernetes readiness checks (API server, RuntimeClass, ServiceAccount)
4. **Phase 4**: Integration tests (DNS, overlay, process cleanup, exec support, etc.)

All tests must pass for the suite to succeed.

## Development Workflow

### Quick Iteration
```bash
cargo test              # Unit tests (fast)
cargo clippy            # Linting
cargo fmt --all         # Format code
```

### Integration Testing
```bash
# Full test (creates Kind cluster, builds, tests, cleans up)
./scripts/run-integration-tests.sh

# Iterative development (keep cluster alive)
./scripts/run-integration-tests.sh --no-cleanup
# Make changes...
cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime
./scripts/run-integration-tests.sh --skip-cargo --no-cleanup
# Final run with cleanup
./scripts/run-integration-tests.sh --skip-cargo
```

### Linux-specific Code on macOS
```bash
# Check Linux-only code compiles (overlay.rs is Linux-only)
rustup target add x86_64-unknown-linux-gnu
cargo clippy --target x86_64-unknown-linux-gnu --all-targets
```

## Implementation Status (February 2026)

### ✅ Core Features Complete
- Full OCI runtime (create, start, state, kill, delete)
- Containerd shim v2 protocol (all Task methods)
- Fork-first architecture with real exit code capture
- Zombie process reaping
- FIFO-based I/O capture (kubectl logs)
- PTY support (kubectl run -it, kubectl exec -it)
- Overlay filesystem namespace with persistent helper
- Volume mounts (ConfigMap, Secret, hostPath, emptyDir) via OCI bind mounts
- UID/GID switching with privilege dropping (setgroups → setgid → setuid → umask)
- Sensitive host file filtering in overlay
- State persistence and lifecycle management
- Kubernetes integration via RuntimeClass
- End-to-end validation with Kind cluster

### 🔄 Known Limitations
- Multi-container pods not fully tested
- ResizePty returns OK but is no-op (no dynamic PTY resize)
- No cgroup resource limits (by design)
- No namespace isolation (by design)
- Volume mounts are shared across all workloads (no per-container isolation)

### ⏳ Future Work
See [docs/TODO.md](docs/TODO.md) for planned enhancements:
- Real Kubernetes cluster testing (GKE, EKS)

## Documentation Map

- **[README.md](README.md)** - Project overview, quick start, features
- **[examples/README.md](examples/README.md)** - Runnable Kind-based demos (scheduling, client-server, runAs, volumes)
- **[kubernetes/README.md](kubernetes/README.md)** - Installation and Kubernetes integration guide
- **[TESTING.md](TESTING.md)** - Testing guide (unit, integration, coverage)
- **[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)** - Development setup, tooling, contributing
- **[docs/SHIMV2_DESIGN.md](docs/SHIMV2_DESIGN.md)** - Shim v2 protocol implementation (authoritative)
- **[docs/SHIM_ARCHITECTURE.md](docs/SHIM_ARCHITECTURE.md)** - Architecture deep-dive
- **[docs/OVERLAY_DESIGN.md](docs/OVERLAY_DESIGN.md)** - Overlay filesystem design
- **[docs/CURRENT_STATE.md](docs/CURRENT_STATE.md)** - ⚠️ **OUTDATED** - refer to SHIMV2_DESIGN.md instead
- **[docs/TODO.md](docs/TODO.md)** - Future work and enhancements

## Important Notes

- **CURRENT_STATE.md is outdated** - Use SHIMV2_DESIGN.md for current implementation status
- **macOS compatibility** - All Linux-specific code must be gated with `#[cfg(target_os = "linux")]`
- **Overlay is mandatory** - No fail-open to host-direct execution on Linux
- **Fork-first is critical** - Do not change fork order; see MEMORY.md for why
- **500ms timing delay** - Required for fast processes; see SHIM_ARCHITECTURE.md for details
