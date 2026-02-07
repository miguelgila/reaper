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
                 └─ fork() (inner child)
                 │    ├─ unshare(CLONE_NEWNS)
                 │    ├─ mount("", "/", MS_PRIVATE | MS_REC)
                 │    ├─ mount overlay on /run/reaper/merged
                 │    ├─ bind-mount /proc, /sys, /dev, /run
                 │    ├─ pivot_root(/run/reaper/merged, .../old_root)
                 │    ├─ umount(/old_root, MNT_DETACH)
                 │    └─ signal parent "ready", sleep forever
                 │
                 └─ inner parent (host ns):
                      ├─ wait for "ready"
                      ├─ bind-mount /proc/<child>/ns/mnt → /run/reaper/shared-mnt-ns
                      ├─ kill(child)  # namespace persists via bind-mount
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

## Limitations

- `/run` is typically a small tmpfs; for write-heavy workloads, configure
  `REAPER_OVERLAY_BASE` to point to a larger filesystem
- All workloads share the same overlay — no per-namespace isolation
  (by design, for cross-deployment file sharing)
- Overlay does not protect against processes that directly modify kernel
  state via `/proc` or `/sys` writes
