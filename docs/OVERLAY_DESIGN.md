# Overlay Filesystem Design

## Overview

Reaper uses a shared mount namespace with an overlayfs to protect the host
filesystem while allowing cross-deployment file sharing. All workloads on a
node share a single writable overlay layer; the host root is the read-only
lower layer.

## How It Works

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

- **Reads** fall through to the host root (lower layer)
- **Writes** go to the upper layer (`/run/reaper/overlay/upper`)
- All Reaper workloads see the same upper layer
- The host filesystem is never modified

## Architecture

### Namespace Creation (First Workload)

The first workload to start creates the shared namespace:

```
reaper-runtime do_start()
  └─ fork() (daemon child)
       └─ setsid()
       └─ enter_overlay()
            └─ acquire_lock(/run/reaper/overlay.lock)
            └─ create_namespace()
                 └─ fork() (inner child - helper)
                 │    ├─ unshare(CLONE_NEWNS)
                 │    ├─ mount("", "/", MS_PRIVATE | MS_REC)
                 │    ├─ mount overlay on /run/reaper/merged
                 │    ├─ bind-mount /proc, /sys, /dev, /run
                 │    ├─ bind-mount /etc → /run/reaper/merged/etc
                 │    ├─ pivot_root(/run/reaper/merged, .../old_root)
                 │    ├─ umount(/old_root, MNT_DETACH)
                 │    └─ signal parent "ready", sleep forever (kept alive)
                 │
                 └─ inner parent (host ns):
                      ├─ wait for "ready"
                      ├─ bind-mount /proc/<child>/ns/mnt → /run/reaper/shared-mnt-ns
                      ├─ keep child alive (helper persists namespace)
                      └─ setns(shared-mnt-ns)  # join the namespace
```

### Namespace Joining (Subsequent Workloads)

```
reaper-runtime do_start()
  └─ enter_overlay()
       └─ acquire_lock()
       └─ namespace_exists(/run/reaper/shared-mnt-ns) → true
       └─ join_namespace()
            └─ setns(fd, CLONE_NEWNS)
```

### Why Inner Fork?

The bind-mount of `/proc/<pid>/ns/mnt` to a host path must be done from
the HOST mount namespace. After `unshare(CLONE_NEWNS)`, the process is in
the new namespace and bind-mounts don't propagate to the host. The inner
parent stays in the host namespace to perform this operation.

### Why Keep Helper Alive?

The helper process (inner child) is kept alive to persist the namespace.
While the bind-mount of `/proc/<pid>/ns/mnt` keeps the namespace reference,
keeping the helper alive ensures /etc files and other bind-mounts remain
accessible. The helper sleeps indefinitely until explicitly terminated.

### Why pivot_root?

Mounting overlay directly on `/` hides all existing submounts (`/proc`,
`/sys`, `/dev`). With `pivot_root`, we mount overlay on a new point,
bind-mount special filesystems into it, then switch root. This preserves
real host `/proc`, `/sys`, and `/dev`.

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `REAPER_OVERLAY_BASE` | `/run/reaper/overlay` | Base dir for upper/work layers |

Overlay is always enabled on Linux. There is no option to disable it —
workloads must not modify the host filesystem.

### Bind-Mounted Directories

Only kernel-backed special filesystems and `/run` are bind-mounted from
the host into the overlay:

- `/proc` — process information (kernel-backed)
- `/sys` — kernel/device information (kernel-backed)
- `/dev` — device nodes (kernel-backed)
- `/run` — runtime state (needed for daemon↔shim communication via state files)

**`/tmp` is NOT bind-mounted** — writes to `/tmp` go through the overlay
upper layer, protecting the host's `/tmp` from modification.

### Directory Structure

```
/run/reaper/
├── overlay/
│   ├── upper/        # shared writable layer
│   └── work/         # overlayfs internal
├── merged/           # pivot_root target (temporary during setup)
├── shared-mnt-ns     # bind-mounted namespace reference
├── overlay.lock      # file lock for namespace creation
└── <container-id>/   # per-container state (existing)
```

## Lifecycle

1. **Boot**: `/run` is tmpfs, starts empty (ephemeral by design)
2. **First workload**: Creates overlay dirs, namespace, and overlay mount
3. **Subsequent workloads**: Join existing namespace via `setns()`
4. **Reboot**: Everything under `/run` is cleared; fresh start

## Mandatory Isolation

Overlay is mandatory on Linux. If overlay setup fails (e.g., not running
as root, kernel lacks overlay support), the workload is **refused** — it
will not run on the host filesystem. The daemon exits with code 1 and
updates the container state to `stopped`.

## Requirements

- Linux kernel with overlayfs support (standard since 3.18)
- `CAP_SYS_ADMIN` (required for `unshare`, `setns`, `mount`, `pivot_root`)
- Reaper runtime runs as root on the node (standard for container runtimes)
- Not available on macOS (code gated with `#[cfg(target_os = "linux")]`)

## Sensitive File Filtering

Reaper automatically filters sensitive host files to prevent workloads from
accessing credentials, SSH keys, and other sensitive data. Filtering is
implemented by bind-mounting empty placeholders over sensitive paths after
pivot_root.

### Default Filtered Paths

- `/root/.ssh` - root user SSH keys
- `/etc/shadow`, `/etc/gshadow` - password hashes
- `/etc/ssh/ssh_host_*_key` - SSH host private keys
- `/etc/ssl/private` - SSL/TLS private keys
- `/etc/sudoers`, `/etc/sudoers.d` - sudo configuration
- `/var/lib/docker` - Docker internal state
- `/run/secrets` - container secrets

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `REAPER_FILTER_ENABLED` | `true` | Enable/disable filtering |
| `REAPER_FILTER_PATHS` | `""` | Colon-separated custom paths |
| `REAPER_FILTER_MODE` | `append` | `append` or `replace` |
| `REAPER_FILTER_ALLOWLIST` | `""` | Paths to exclude from filtering |
| `REAPER_FILTER_DIR` | `/run/reaper/overlay-filters` | Placeholder directory |

**Example**: Add custom paths while keeping defaults:
```bash
REAPER_FILTER_PATHS="/custom/secret:/home/user/.aws/credentials"
```

**Example**: Replace default list entirely:
```bash
REAPER_FILTER_MODE=replace
REAPER_FILTER_PATHS="/etc/shadow:/etc/gshadow"
```

**Example**: Disable a specific default filter:
```bash
REAPER_FILTER_ALLOWLIST="/etc/shadow"
```

### Security Guarantees

- Filters are immutable (workloads cannot unmount them)
- Applied once during namespace creation
- Inherited by all workloads joining the namespace
- Non-existent paths are silently skipped
- Individual filter failures are logged but non-fatal

### How It Works

After `pivot_root` completes in the shared namespace:

1. Read filter configuration from environment variables
2. Build filter list (defaults + custom, minus allowlist)
3. Create empty placeholder files/directories in `/run/reaper/overlay-filters/`
4. For each sensitive path:
   - If path exists, create matching placeholder (file or directory)
   - Bind-mount placeholder over the sensitive path
   - Log success/failure

This makes sensitive files appear empty or missing to workloads, while the
actual host files remain untouched.

## Namespace Isolation

By default (`REAPER_OVERLAY_ISOLATION=namespace`), each Kubernetes namespace
gets its own isolated overlay. This means workloads in `production` cannot
see writes from workloads in `dev`, matching Kubernetes' namespace-as-trust-boundary
expectation.

### How It Works

1. The containerd shim reads `io.kubernetes.pod.namespace` from OCI annotations
2. It passes `--namespace <ns>` to `reaper-runtime create`
3. The runtime stores the namespace in `ContainerState` (state.json)
4. On `start` and `exec`, the runtime reads the namespace from state and
   computes per-namespace paths for overlay dirs, mount namespace, and lock

### Per-Namespace Path Layout

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

### Legacy Node-Wide Mode

Set `REAPER_OVERLAY_ISOLATION=node` to use the old flat layout where all
workloads share a single overlay regardless of their K8s namespace. This
is useful for cross-deployment file sharing or backward compatibility.

### Upgrade Path

Existing containers created before the upgrade have `namespace: None` in their
state files. With the default namespace isolation mode, their `start` will fail.
Drain nodes before upgrading to ensure no in-flight containers are affected.

## Limitations

- `/run` is typically a small tmpfs; for write-heavy workloads, configure
  `REAPER_OVERLAY_BASE` to point to a larger filesystem
- Within a single K8s namespace, workloads still share the same overlay
  (no per-pod isolation)
- Overlay does not protect against processes that directly modify kernel
  state via `/proc` or `/sys` writes
- Sensitive file filtering does not support glob patterns (use explicit paths)
