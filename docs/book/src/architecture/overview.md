# Architecture Overview

Reaper consists of three components arranged in a three-tier system:

```
Kubernetes/containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2  (long-lived shim, implements Task trait)
        ↓ (exec: create/start/state/delete/kill)
reaper-runtime  (short-lived OCI runtime CLI)
        ↓ (fork FIRST, then spawn)
monitoring daemon → spawns workload → wait() → captures exit code
```

## Components

### containerd-shim-reaper-v2

The **shim** is a long-lived process (one per container) that communicates with containerd via ttrpc. It implements the containerd Task service interface and delegates OCI operations to the runtime binary.

### reaper-runtime

The **runtime** is a short-lived CLI tool called by the shim for OCI operations (create, start, state, kill, delete). It implements the fork-first architecture for process monitoring.

### Monitoring Daemon

The **daemon** is forked by the runtime during `start`. It spawns the workload as its child, calls `wait()` to capture the real exit code, and updates the state file.

## Fork-First Architecture

This is the most critical design decision in Reaper:

1. Runtime forks → creates monitoring daemon
2. Parent (CLI) exits immediately (OCI spec requires this)
3. Daemon calls `setsid()` to detach
4. Daemon spawns workload (daemon becomes parent)
5. Daemon calls `wait()` on workload → captures real exit code
6. Daemon updates state file, then exits

**Why fork-first?** Only a process's parent can call `wait()` on it. The daemon must be the workload's parent to capture exit codes. Spawning first, then forking, would leave the `Child` handle invalid in the forked process.

## State Management

Process lifecycle state is stored in `/run/reaper/<container-id>/state.json`:

```json
{
  "id": "abc123...",
  "bundle": "/run/containerd/io.containerd.runtime.v2.task/k8s.io/abc123...",
  "status": "stopped",
  "pid": 12345,
  "exit_code": 0
}
```

The shim polls this file to detect state changes and publishes containerd events (e.g., `TaskExit`).

## Further Reading

- [Shim v2 Protocol](shim-v2.md) — Full protocol implementation details
- [Overlay Filesystem](overlay.md) — How host filesystem protection works
