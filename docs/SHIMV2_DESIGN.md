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
| Exec | ⚠️ | Not implemented |
| ResizePty | ⚠️ | Not implemented |
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

### Deploy to Minikube
```bash
./scripts/minikube-setup-runtime.sh
```

### Verify Pod Completion
```bash
kubectl get pod reaper-example
# Should show: Completed (0 restarts)
```

### Check Logs
```bash
minikube ssh -- 'tail -50 /var/log/reaper-shim.log'
minikube ssh -- 'tail -50 /var/log/reaper-runtime.log'
```

## Resources

- [containerd shim v2 spec](https://github.com/containerd/containerd/blob/main/runtime/v2/README.md)
- [containerd protobuf definitions](https://github.com/containerd/containerd/tree/main/api/runtime/task)
- [TTRPC protocol](https://github.com/containerd/ttrpc)
- [OCI Runtime Spec](https://github.com/opencontainers/runtime-spec)

---

**Document Version:** 2.0
**Last Updated:** January 2026
**Status:** Core Implementation Complete
