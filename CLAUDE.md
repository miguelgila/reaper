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

By default, Reaper isolates overlays per Kubernetes namespace. Each K8s namespace
gets its own overlay upper/work dirs, mount namespace, and lock file:

```
/run/reaper/
  overlay/
    default/upper/          # K8s "default" namespace
    default/work/
    kube-system/upper/      # K8s "kube-system" namespace
    kube-system/work/
  merged/
    default/                # pivot_root target per namespace
    kube-system/
  ns/
    default                 # persisted mount namespace bind-mount
    kube-system
  overlay-default.lock      # per-namespace flock
  overlay-kube-system.lock
```

The shim extracts `io.kubernetes.pod.namespace` from OCI annotations and
passes `--namespace <ns>` to the runtime. The runtime stores it in state
so `start` and `exec` join the correct namespace's overlay.

When `reaper.runtime/overlay-name` is set (e.g., `pippo`), an additional
sub-group level is added within the namespace:

```
/run/reaper/
  overlay/
    production/pippo/upper/   # Named overlay group "pippo" in "production"
    production/pippo/work/
  merged/
    production/pippo/
  ns/
    production--pippo         # double-dash separator (DNS labels use single hyphens)
  overlay-production--pippo.lock
```

Set `REAPER_OVERLAY_ISOLATION=node` to opt out and use the legacy flat layout
where all workloads share a single overlay. `overlay-name` is ignored in node mode.

- Reads fall through to host root
- Writes go to namespace-scoped upper layer
- Host filesystem never modified (mandatory isolation)
- Uses `pivot_root` to preserve `/proc`, `/sys`, `/dev`
- `/tmp` is NOT bind-mounted (protected by overlay)

**Configuration:**
- Config file: `/etc/reaper/reaper.conf` (cross-distro, `KEY=VALUE` format)
- Config load order: config file defaults → env vars override
- `REAPER_CONFIG`: Override config file path (default: `/etc/reaper/reaper.conf`)
- `REAPER_OVERLAY_ISOLATION`: Isolation mode — `namespace` (default) isolates overlays per K8s namespace, `node` uses legacy shared overlay
- `REAPER_OVERLAY_BASE`: Default `/run/reaper/overlay` (in node mode) or `/run/reaper/overlay/<ns>` (in namespace mode)
- `REAPER_DNS_MODE`: DNS resolution mode — `host` (default) uses node's resolv.conf, `kubernetes`/`k8s` writes kubelet-prepared resolv.conf (pointing to CoreDNS) into the overlay
- `REAPER_ANNOTATIONS_ENABLED`: Master switch for pod annotation overrides (default: `true`). Set to `false` to disable all annotation processing.
- Overlay is mandatory on Linux (no fail-open)
- Not available on macOS (code gated with `#[cfg(target_os = "linux")]`)

### Pod Annotations

Users can influence Reaper behavior per-pod via Kubernetes annotations with the `reaper.runtime/` prefix. Annotations are extracted from OCI `config.json` by the shim and passed to the runtime via `--annotation key=value` CLI args.

**Supported annotations:**

| Annotation | Maps To | Valid Values | Default |
|---|---|---|---|
| `reaper.runtime/dns-mode` | `REAPER_DNS_MODE` | `host`, `kubernetes`, `k8s` | `host` |
| `reaper.runtime/overlay-name` | Named overlay group | DNS label (`[a-z0-9-]`, max 63) | *(none — uses namespace overlay)* |

**Example pod spec:**
```yaml
apiVersion: v1
kind: Pod
metadata:
  annotations:
    reaper.runtime/dns-mode: "kubernetes"
    reaper.runtime/overlay-name: "my-group"
spec:
  runtimeClassName: reaper
  containers:
    - name: my-app
      image: my-app:latest
```

**Security model:**
- Only annotations in the user-overridable allowlist are honored (currently: `dns-mode`, `overlay-name`)
- Admin-only parameters (overlay paths, filter settings, runtime paths) can NEVER be overridden via annotations
- Admin can disable all annotations: `REAPER_ANNOTATIONS_ENABLED=false`
- Unknown annotation keys are silently ignored; invalid values are logged and ignored

**Data flow:**
1. User sets `reaper.runtime/*` annotations in pod spec
2. Containerd writes annotations to OCI `config.json`
3. Shim extracts `reaper.runtime/*` annotations, passes `--annotation key=value` to runtime
4. Runtime stores annotations in `ContainerState` (state.json)
5. On `start`, runtime applies allowed annotation overrides (e.g., DNS mode, overlay name)

**Key files:**
- [src/annotations.rs](src/annotations.rs) — Shared annotation parsing, validation, allowlist
- Shim: `extract_reaper_annotations_from_bundle()` in main.rs
- Runtime: `do_create()` stores annotations, `do_start()` applies overrides

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
├── src/
│   ├── config.rs                    # Shared config file loader (/etc/reaper/reaper.conf)
│   ├── annotations.rs               # Shared pod annotation parsing and validation
│   ├── crds/                        # ReaperPod CRD types (feature-gated: controller)
│   └── bin/
│       ├── containerd-shim-reaper-v2/
│       │   └── main.rs              # Shim implementation (ttrpc server, Task trait)
│       ├── reaper-controller/       # CRD controller binary
│       │   ├── main.rs              # Entry point, CRD watcher setup
│       │   ├── reconciler.rs        # ReaperPod → Pod reconciliation
│       │   └── pod_builder.rs       # ReaperPod spec → Pod spec translation
│       └── reaper-runtime/
│           ├── main.rs              # OCI runtime CLI (fork-first architecture)
│           ├── state.rs             # State persistence (/run/reaper/<id>/)
│           └── overlay.rs           # Overlay filesystem (Linux-only)
├── tests/                       # Integration tests
│   ├── integration_basic_binary.rs
│   ├── integration_io.rs        # FIFO stdout/stderr
│   ├── integration_exec.rs      # Exec support
│   ├── integration_annotations.rs # Pod annotation overrides
│   ├── integration_overlay.rs   # Overlay filesystem
│   ├── integration_shim.rs      # Shim protocol
│   └── integration_user_management.rs
├── examples/                    # Runnable Kind-based demos
│   ├── 01-scheduling/           # DaemonSet on all/subset of nodes
│   ├── 02-client-server/        # TCP server + clients across nodes
│   ├── 03-client-server-runas/  # Same as above, running as non-root user
│   ├── 04-volumes/              # Kubernetes volume mounts with overlay
│   ├── 05-kubemix/              # Jobs, DaemonSets, and Deployments on 10-node cluster
│   ├── 06-ansible-jobs/         # Sequential Jobs: install Ansible, then run playbook
│   ├── 07-ansible-complex/     # DaemonSet bootstrap + role-based Ansible playbooks
│   ├── 08-mix-container-runtime-engines/  # Mixed runtimes: OpenLDAP (default) + SSSD (Reaper)
│   └── 09-reaperpod/                     # ReaperPod CRD: simplified Reaper-native workloads
├── scripts/
│   ├── run-integration-tests.sh   # Full integration test suite
│   ├── install-reaper.sh          # Installation script (Ansible, DEPRECATED)
│   ├── build-node-image.sh        # Build node installer image for Kind
│   ├── build-controller-image.sh  # Build controller Docker image for Kind
│   ├── install-node.sh            # Init container script for node DaemonSet
│   └── generate-crds.sh           # Generate CRD YAML from Rust types
├── deploy/
│   ├── helm/reaper/               # Helm chart (recommended installation)
│   │   ├── Chart.yaml
│   │   ├── values.yaml
│   │   ├── crds/                  # CRD definitions
│   │   └── templates/             # DaemonSet, Controller, RBAC, RuntimeClass
│   ├── ansible/                   # DEPRECATED — use Helm chart instead
│   │   └── install-reaper.yml
│   └── kubernetes/
│       ├── runtimeclass.yaml
│       ├── reaper-controller.yaml
│       └── crds/
│           └── reaperpods.reaper.giar.dev.yaml
└── docs/
    ├── SHIMV2_DESIGN.md         # Shim v2 protocol implementation
    ├── SHIM_ARCHITECTURE.md     # Architecture deep-dive
    ├── OVERLAY_DESIGN.md        # Overlay filesystem design
    ├── DEVELOPMENT.md           # Development guide
    ├── TESTING.md               # Testing guide
    ├── CONTRIBUTING.md          # Contributing guide
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
- [docs/TESTING.md](docs/TESTING.md) - comprehensive testing guide

## CI/CD and Integration Testing

### Permission Issues in GitHub Actions

**Problem**: In GitHub Actions CI, the `target/` directory is often cached and owned by a different user than the current workflow step. This causes "Permission denied" errors when trying to copy binaries to `target/release/`.

**Solution**: The integration test scripts detect CI mode via the `CI` environment variable and use binaries directly from `target/<target-triple>/release/` without copying them. This is controlled by the `REAPER_BINARY_DIR` environment variable.

- **CI mode** (`CI=true`): Uses binaries from `target/<target-triple>/release/` directly
- **Local mode**: Copies binaries to `target/release/` for convenience

Key environment variables:
- `CI`: Set by GitHub Actions automatically. Enables CI-specific behavior.
- `REAPER_BINARY_DIR`: Override the binary directory location (legacy Ansible installer).

Files involved:
- [scripts/setup-playground.sh](scripts/setup-playground.sh): Creates Kind cluster and installs via Helm
- [scripts/build-node-image.sh](scripts/build-node-image.sh): Builds reaper-node installer image for Kind
- [scripts/build-controller-image.sh](scripts/build-controller-image.sh): Builds reaper-controller image for Kind
- [deploy/helm/reaper/](deploy/helm/reaper/): Helm chart (DaemonSet, Controller, CRD, RuntimeClass)

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

The integration test suite ([scripts/run-integration-tests.sh](scripts/run-integration-tests.sh)) has five phases:

1. **Phase 1**: Rust cargo tests (unit and integration tests)
2. **Phase 2**: Infrastructure setup (Kind cluster, build images, install Reaper via Helm)
3. **Phase 3**: Kubernetes readiness checks (API server, RuntimeClass, ServiceAccount)
4. **Phase 4**: Integration tests (DNS, overlay, process cleanup, exec support, etc.)
5. **Phase 4b**: Controller tests (ReaperPod CRD lifecycle, status mirroring, exit codes, annotations, GC)

Use `--crd-only` or `--agent-only` to run subsets. All tests must pass for the suite to succeed.

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
- Per-pod annotation-based configuration (DNS mode override via `reaper.runtime/dns-mode`)

### 🔄 Known Limitations
- Multi-container pods not fully tested
- ResizePty polling interval is 100ms (resize may not feel instant)
- No cgroup resource limits (by design)
- No namespace isolation (by design)
- Volume mounts are shared across all workloads (no per-container isolation)

### ⏳ Future Work
See [docs/TODO.md](docs/TODO.md) for planned enhancements:
- Real Kubernetes cluster testing (GKE, EKS)
- ReaperDaemonJob CRD (see below)

## ReaperDaemonJob CRD (Planned)

A new CRD to replace Nomad exec jobs for node configuration tasks (vServices). Fills a
gap Kubernetes lacks natively — a "DaemonJob" that runs to completion on every matching
node and re-triggers on node events (join, reboot).

**Use case:** Ansible playbooks that configure compute nodes (mount filesystems, install
packages, start system daemons). Currently deployed via Nomad exec at CSCS.

**Design:**
- Controller watches `Node` events (Ready condition changes, new node joining)
- For each matching node, creates a `ReaperPod` (reusing existing CRD) via `nodeName`
- Tracks per-node completion in `ReaperDaemonJob.status.nodeStatuses`
- Supports dependency ordering between jobs (`after: [job-name]`)
- On spec change, re-triggers on all matching nodes

**Key spec fields:**
- `command` / `args` — the Ansible playbook invocation
- `overlayName` — shared overlay so composable vServices see each other's mounts and
  installed packages (uses existing named overlay support)
- `nodeSelector` — target specific node groups
- `triggerOn` — NodeReady, NodeReboot, Manual, Schedule
- `after` — dependency ordering within the shared overlay
- `retryPolicy` — per-node retry on failure
- `concurrencyPolicy` — skip if already running on that node

**Why shared overlay matters:** vServices compose — one mounts a filesystem, another
installs packages on it. Reaper's named overlays (`overlayName`) let multiple jobs share
an overlay upper layer and mount namespace via `setns()`. See `examples/06-ansible-jobs/`.

**Why tmpfs overlay is a feature:** Overlays live on `/run` (tmpfs) and vanish on reboot.
When a node reboots and rejoins K8s, the controller re-triggers all jobs in dependency
order, rebuilding node state from scratch. No configuration drift.

**Controller layering:** `ReaperDaemonJob → ReaperPod → Pod`. No changes to existing
runtime or reaper-controller — only a new controller/reconciler for the new CRD.

## ReaperOverlay CRD (In Progress)

A PVC-like CRD that decouples overlay lifecycle from pod lifecycle. See
[docs/REAPER_OVERLAY_PLAN.md](docs/REAPER_OVERLAY_PLAN.md) for the full implementation plan.

**Key design decisions:**
- PVC-like blocking: ReaperPods with `overlayName` stay Pending until a matching `ReaperOverlay` is Ready
- Reset via `spec.resetGeneration` counter (monotonic, no race conditions)
- Deletion triggers on-disk cleanup on all nodes via finalizer
- Controller-to-agent communication: direct HTTP (v1). **Future nice-to-have**: annotation-based
  communication (controller sets annotations on agent pods, agent watches and acts) for environments
  where network policies prevent direct controller→agent HTTP calls.

## Documentation Map

- **[README.md](README.md)** - Project overview, quick start, features
- **[examples/README.md](examples/README.md)** - Runnable Kind-based demos (scheduling, client-server, runAs, volumes)
- **[deploy/kubernetes/README.md](deploy/kubernetes/README.md)** - Installation and Kubernetes integration guide
- **[docs/TESTING.md](docs/TESTING.md)** - Testing guide (unit, integration, coverage)
- **[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)** - Development setup, tooling, contributing
- **[docs/SHIMV2_DESIGN.md](docs/SHIMV2_DESIGN.md)** - Shim v2 protocol implementation (authoritative)
- **[docs/SHIM_ARCHITECTURE.md](docs/SHIM_ARCHITECTURE.md)** - Architecture deep-dive
- **[docs/OVERLAY_DESIGN.md](docs/OVERLAY_DESIGN.md)** - Overlay filesystem design
- **[docs/CURRENT_STATE.md](docs/CURRENT_STATE.md)** - ⚠️ **OUTDATED** - refer to SHIMV2_DESIGN.md instead
- **[docs/TODO.md](docs/TODO.md)** - Future work and enhancements

## Release Pipeline: Container Image Publication (March 2026)

Priority list for completing the release pipeline:

- [x] **1. Add container image build/push job to `release.yml`** — `container-images` job builds and pushes all 3 images to GHCR with semver + latest tags, using docker buildx for multi-arch (linux/amd64 + linux/arm64)
- [x] **2. Port reaper-agent DaemonSet to Helm chart** — added `agent-daemonset.yaml`, `agent-rbac.yaml` templates with `agent.enabled` toggle, Prometheus metrics annotations, full RBAC
- [x] **3. Wire `Chart.yaml` appVersion into auto-release and manual-release** — both workflows now update `Chart.yaml` appVersion alongside `Cargo.toml` and include it in the release commit
- [x] **4. Switch default image tags from `:latest` to appVersion** — all 3 Helm templates use `{{ .Values.*.image.tag | default .Chart.AppVersion }}`, values.yaml defaults to `tag: ""`
- [x] **5. Add `cosign sign` for container images** — keyless cosign signing via GitHub OIDC after each image push in the `container-images` job
- [x] **6. Normalize Dockerfiles to buildx-friendly pattern** — all 3 Dockerfiles use `--platform=$BUILDPLATFORM` builder stages (cross-compile natively) with `TARGETARCH` selection in runtime stage

## Important Notes

- **CURRENT_STATE.md is outdated** - Use SHIMV2_DESIGN.md for current implementation status
- **macOS compatibility** - All Linux-specific code must be gated with `#[cfg(target_os = "linux")]`
- **Overlay is mandatory** - No fail-open to host-direct execution on Linux
- **Fork-first is critical** - Do not change fork order; see MEMORY.md for why
- **500ms timing delay** - Required for fast processes; see SHIM_ARCHITECTURE.md for details
- **Bug tracking** - All bugs are tracked as GitHub issues (no local BUGS.md file)
