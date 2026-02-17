# Shim v2 Implementation Design

## Overview

This document outlines the implementation plan for containerd Runtime v2 API (shim protocol) support in Reaper, enabling Kubernetes integration for **command execution on the host system**.

**Important Clarification:** Reaper does not create traditional containers. Instead, it executes commands directly on the Kubernetes cluster nodes, providing a lightweight alternative to full containerization for specific use cases.

## Background

### What is the Shim v2 Protocol?

The containerd Runtime v2 API is the interface between:
- **containerd** (Kubernetes container runtime)
- **Container runtime shim** (our code)
- **Command executor** (reaper-runtime running commands on host)

```
Kubernetes → CRI → containerd → [Shim v2 API] → reaper-shim → host command execution
```

### Why Do We Need It?

Without shim v2:
- ❌ Kubernetes can't execute commands via reaper
- ❌ No process lifecycle management
- ❌ No command output streaming

With shim v2:
- ✅ Kubernetes can run/start/stop commands
- ✅ Stream command output and exec into running processes
- ✅ Monitor command execution status
- ✅ Full process lifecycle support

## Architecture

### Three-Tier Design (Implemented)

```
containerd-shim-reaper-v2    ← Shim binary (ttrpc server, long-lived)
    ↓ (subprocess calls)
reaper-runtime               ← OCI runtime CLI (create/start/state/kill/delete)
    ↓ (fork)
monitoring daemon            ← Spawns and monitors workload
    ↓ (spawn)
workload process             ← The actual command being run
```

**Key Points:**
- Shim is long-lived (one per container, communicates with containerd via ttrpc)
- Runtime is short-lived CLI (called by shim for OCI operations)
- Monitoring daemon is forked by runtime to watch workload
- Workload is spawned BY the daemon (daemon is parent)

### Why Fork-First Architecture?

**The Problem:**
- OCI spec requires runtime CLI to exit immediately after `start`
- Someone needs to `wait()` on the workload to capture exit code
- Only a process's **parent** can call `wait()` on it

**Previous Bug (FIXED):**
We originally spawned the workload first, then forked. After `fork()`, the `std::process::Child` handle was invalid in the forked child because it was created by the parent process.

**Solution:** Fork FIRST, then spawn
1. Runtime forks → creates monitoring daemon
2. Parent (CLI) exits immediately
3. Daemon spawns workload (daemon becomes parent)
4. Daemon can now `wait()` on workload

## Shim v2 API Implementation

### Task Service Methods

```protobuf
service Task {
    rpc Create(CreateTaskRequest) returns (CreateTaskResponse);
    rpc Start(StartTaskRequest) returns (StartTaskResponse);
    rpc Delete(DeleteTaskRequest) returns (DeleteTaskResponse);
    rpc Pids(PidsRequest) returns (PidsResponse);
    rpc Pause(PauseRequest) returns (google.protobuf.Empty);
    rpc Resume(ResumeRequest) returns (google.protobuf.Empty);
    rpc Checkpoint(CheckpointTaskRequest) returns (google.protobuf.Empty);
    rpc Kill(KillRequest) returns (google.protobuf.Empty);
    rpc Exec(ExecProcessRequest) returns (google.protobuf.Empty);
    rpc ResizePty(ResizePtyRequest) returns (google.protobuf.Empty);
    rpc CloseIO(CloseIORequest) returns (google.protobuf.Empty);
    rpc Update(UpdateTaskRequest) returns (google.protobuf.Empty);
    rpc Wait(WaitRequest) returns (WaitResponse);
    rpc Stats(StatsRequest) returns (StatsResponse);
    rpc Connect(ConnectRequest) returns (ConnectResponse);
    rpc Shutdown(ShutdownRequest) returns (google.protobuf.Empty);
}
```

### Implementation Status

| Method | Status | Notes |
|--------|--------|-------|
| Create | ✅ | Calls `reaper-runtime create`, handles sandbox detection |
| Start | ✅ | Calls `reaper-runtime start`, fork-first architecture |
| Delete | ✅ | Calls `reaper-runtime delete`, cleans up state |
| Kill | ✅ | Calls `reaper-runtime kill`, handles ESRCH gracefully |
| Wait | ✅ | Polls state file, publishes TaskExit event |
| State | ✅ | Calls `reaper-runtime state`, returns proper protobuf status |
| Pids | ✅ | Returns workload PID from state |
| Stats | ✅ | Basic implementation (no cgroup metrics) |
| Connect | ✅ | Returns shim and workload PIDs |
| Shutdown | ✅ | Triggers shim exit |
| Pause/Resume | ⚠️ | Returns OK but no-op (no cgroup freezer) |
| Checkpoint | ⚠️ | Not implemented (no CRIU) |
| Exec | ✅ | Implemented with PTY support |
| ResizePty | ⚠️ | Returns OK but no-op (no dynamic resize) |
| CloseIO | ⚠️ | Not implemented |
| Update | ⚠️ | Not implemented (no cgroups) |

## Implementation Milestones

### ✅ Milestone 1: Project Setup - COMPLETED

- [x] Add dependencies: `containerd-shim`, `containerd-shim-protos`, `tokio`, `async-trait`
- [x] Generate protobuf code from containerd definitions (via containerd-shim-protos)
- [x] Create `containerd-shim-reaper-v2` binary crate
- [x] Set up basic TTRPC server with Shim and Task traits

### ✅ Milestone 2: Core Task API - COMPLETED

- [x] Implement Create - parse bundle, call reaper-runtime create
- [x] Implement Start - call reaper-runtime start, capture PID
- [x] Implement Delete - call reaper-runtime delete, cleanup state
- [x] Implement Kill - call reaper-runtime kill with signal
- [x] Implement Wait - poll state file for completion
- [x] Implement State - return container status with proper protobuf enums
- [x] Implement Pids - list container processes

### ✅ Milestone 3: Process Monitoring - COMPLETED

- [x] Fork-first architecture in reaper-runtime
- [x] Monitoring daemon as parent of workload
- [x] Real exit code capture via `child.wait()`
- [x] State file updates from monitoring daemon
- [x] Zombie process prevention (proper reaping)
- [x] Shim polling of state file for completion detection

### ✅ Milestone 4: Containerd Integration - COMPLETED

- [x] TaskExit event publishing with timestamps
- [x] Proper `exited_at` timestamps in WaitResponse
- [x] Proper `exited_at` timestamps in StateResponse
- [x] ESRCH handling in kill (already-exited processes)
- [x] Sandbox container detection and faking
- [x] Timing delay for fast processes

### ✅ Milestone 5: Kubernetes Integration - COMPLETED

- [x] RuntimeClass configuration
- [x] End-to-end pod lifecycle testing
- [x] Pod status transitions to "Completed"
- [x] Exit code capture and reporting
- [x] No zombie processes
- [x] PTY support for interactive containers
- [x] Exec implementation with PTY support
- [x] File descriptor leak fix
- [x] Overlay namespace improvements

## Critical Bug Fixes (January 2026)

### 1. Fork Order Bug
**File:** `src/bin/reaper-runtime/main.rs:188-311`

**Problem:** `std::process::Child` handle invalid after fork

**Fix:** Fork first, then spawn workload in the forked child
```rust
match unsafe { fork() }? {
    ForkResult::Parent { .. } => {
        // CLI exits, daemon will update state
        sleep(100ms);
        exit(0);
    }
    ForkResult::Child => {
        setsid();  // Detach
        let child = Command::new(program).spawn()?;  // We're the parent!
        update_state("running", child.id());
        sleep(500ms);  // Let containerd observe "running"
        child.wait()?;  // This works!
        update_state("stopped", exit_code);
        exit(0);
    }
}
```

### 2. Fast Process Timing
**File:** `src/bin/reaper-runtime/main.rs:264-270`

**Problem:** Fast commands (echo) completed before containerd observed "running" state

**Fix:** Added 500ms delay after setting "running" state

### 3. Kill ESRCH Error
**File:** `src/bin/reaper-runtime/main.rs:347-365`

**Problem:** containerd's `kill()` failed with ESRCH for already-dead processes

**Fix:** Treat ESRCH as success (process not running = goal achieved)

### 4. TaskExit Event Publishing
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:162-199`

**Problem:** containerd wasn't recognizing container exits

**Fix:** Publish `TaskExit` event with proper `exited_at` timestamp

### 5. Response Timestamps
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:545-552, 615-625`

**Problem:** Missing timestamps in WaitResponse and StateResponse

**Fix:** Include `exited_at` timestamp in all responses for stopped containers

## Technical Details

### ReaperShim Structure
```rust
#[derive(Clone)]
struct ReaperShim {
    exit: Arc<ExitSignal>,
    runtime_path: String,
    namespace: String,
}
```

### ReaperTask Structure
```rust
#[derive(Clone)]
struct ReaperTask {
    runtime_path: String,
    sandbox_state: Arc<Mutex<HashMap<String, (bool, u32)>>>,
    publisher: Arc<RemotePublisher>,
    namespace: String,
}
```

### State File Format
```json
{
  "id": "abc123...",
  "bundle": "/run/containerd/io.containerd.runtime.v2.task/k8s.io/abc123...",
  "status": "stopped",
  "pid": 12345,
  "exit_code": 0
}
```

### Sandbox Container Detection

Sandbox (pause) containers are detected by checking:
1. Image name contains "pause"
2. Command is `/pause`
3. Process args contain "pause"

Sandboxes return fake responses immediately (no actual process).

## Dependencies

### Cargo Dependencies
```toml
[dependencies]
containerd-shim = { version = "0.10", features = ["async", "tracing"] }
containerd-shim-protos = { version = "0.10", features = ["async"] }
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

## Testing

### Run Integration Tests
```bash
./scripts/run-integration-tests.sh
```

This orchestrates all testing including Rust unit tests, Kubernetes infrastructure setup, and comprehensive integration tests (DNS, overlay, host protection, UID/GID switching, privilege dropping, zombies, exec, etc.).

For options and troubleshooting, see [TESTING.md](TESTING.md).

## Security Features

### UID/GID Switching and Privilege Dropping

**Implemented:** February 2026

The runtime supports OCI user specification for credential switching, allowing workloads to run as non-root users. This integrates with Kubernetes `securityContext`:

```yaml
spec:
  securityContext:
    runAsUser: 1000
    runAsGroup: 1000
    fsGroup: 1000
  containers:
  - name: app
    securityContext:
      runAsUser: 1001
```

#### Implementation

**File:** `src/bin/reaper-runtime/main.rs`

Privilege dropping follows the standard Unix sequence in `pre_exec` hooks:

```rust
// 1. Set supplementary groups (requires CAP_SETGID)
if !user.additional_gids.is_empty() {
    let gids: Vec<gid_t> = user.additional_gids.iter().map(|&g| g).collect();
    safe_setgroups(&gids)?;
}

// 2. Set GID (requires CAP_SETGID)
if setgid(user.gid) != 0 {
    return Err(std::io::Error::last_os_error());
}

// 3. Set UID (irreversible privilege drop)
if setuid(user.uid) != 0 {
    return Err(std::io::Error::last_os_error());
}

// 4. Apply umask (if specified)
if let Some(mask) = user.umask {
    umask(mask as mode_t);
}
```

**Platform Compatibility:** The `setgroups()` syscall signature differs across platforms. We provide a platform-specific wrapper:
- **Linux**: `size_t` (usize) for length parameter
- **macOS/BSD**: `c_int` (i32) for length parameter

#### Execution Paths

User switching is implemented in all four execution paths:
1. **PTY mode** (interactive containers): `do_start()` with terminal=true
2. **Non-PTY mode** (batch containers): `do_start()` with terminal=false
3. **Exec with PTY** (kubectl exec -it): `do_exec()` with terminal=true
4. **Exec without PTY** (kubectl exec): `do_exec()` with terminal=false

#### Integration Tests

**Unit Tests** (`tests/integration_user_management.rs`):
- `test_run_with_current_user` - Validates UID/GID from config
- `test_privilege_drop_root_to_user` - Tests root → non-root transition
- `test_non_root_cannot_switch_user` - Permission denial for non-root
- `test_supplementary_groups_validation` - additionalGids support
- `test_umask_affects_file_permissions` - umask application

**Kubernetes Integration Tests** (`scripts/run-integration-tests.sh`):
- `test_uid_gid_switching` - securityContext UID/GID (runAsUser: 1000)
- `test_privilege_drop` - Unprivileged execution (runAsUser: 1001)

All tests validate actual runtime credentials (not just config parsing) via `id -u` and `id -g` commands in the container.

## Resources

- [containerd shim v2 spec](https://github.com/containerd/containerd/blob/main/runtime/v2/README.md)
- [containerd protobuf definitions](https://github.com/containerd/containerd/tree/main/api/runtime/task)
- [TTRPC protocol](https://github.com/containerd/ttrpc)
- [OCI Runtime Spec](https://github.com/opencontainers/runtime-spec)

---

**Document Version:** 2.1
**Last Updated:** February 2026
**Status:** Core Implementation Complete with Exec and PTY Support
