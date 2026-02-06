# Reaper - OCI Runtime Context (Claude Edition)

## Project Overview
Reaper is a minimal OCI-compatible container runtime written in Rust. It focuses on running host binaries using OCI container semantics without full containerization (no namespaces/cgroups initially).

## Architecture

### Core Components
- **containerd-shim-reaper-v2**: Long-lived shim process (one per container)
  - Implements containerd shim v2 API via ttrpc
  - Location: `src/bin/containerd-shim-reaper-v2/main.rs`
  - Lifecycle: Started by containerd, persists for container lifetime
  - Responsibilities: Process monitoring, state reporting, signal handling

- **reaper-runtime**: Short-lived OCI-compatible CLI
  - Location: `src/bin/reaper-runtime/main.rs`
  - Commands: create, start, state, kill, delete
  - Lifecycle: Invoked by shim, exits immediately after completing command
  - State persistence in `/run/reaper/<container-id>/`

### Key Files
- `src/bin/containerd-shim-reaper-v2/main.rs` - Shim implementation (containerd interface)
- `src/bin/reaper-runtime/main.rs` - Runtime CLI (OCI spec compliance)
- `src/bin/reaper-runtime/state.rs` - Container state persistence
- `tests/integration_basic_binary.rs` - Basic binary execution tests
- `tests/integration_user_management.rs` - uid/gid management tests
- `tests/integration_shim.rs` - Shim v2 protocol tests

## Critical Architecture Understanding

### Process Lifecycle (Fork-First Architecture)

#### The Three-Layer Model
```
Kubernetes/Containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2 (LONG-LIVED, one per container)
        ↓ (exec: create/start/state/delete)
reaper-runtime (SHORT-LIVED, exits after each command)
        ↓ (fork FIRST!)
monitoring daemon → spawns workload → wait() → update state
```

#### Lifecycle Timeline
1. **Container Create**
   - Containerd starts `containerd-shim-reaper-v2` process
   - Shim's `create()` calls `reaper-runtime create <bundle>`
   - Runtime validates bundle, creates `/run/reaper/<id>/state.json`
   - Runtime exits with status=created
   - Shim returns to containerd

2. **Container Start** (CRITICAL FLOW - Fork-First!)
   - Containerd calls shim's `start()`
   - Shim calls `reaper-runtime start <id>`
   - Runtime **forks FIRST** (creates monitoring daemon)
   - Parent (CLI) waits 100ms then exits immediately
   - Child (daemon) calls `setsid()` to detach
   - Daemon **spawns workload** (now daemon is workload's parent!)
   - Daemon updates state to "running" with PID
   - Daemon sleeps 500ms (allows containerd to observe "running" state)
   - Daemon calls `child.wait()` (blocks until workload exits)
   - Daemon updates state to "stopped" with exit code
   - Daemon exits

3. **Container Wait** (SOLVED!)
   - Containerd calls shim's `wait()`
   - Shim polls state file via `reaper-runtime state`
   - When state becomes "stopped", shim publishes TaskExit event
   - Shim returns exit code to containerd with `exited_at` timestamp

#### Why Fork-First Works
**CRITICAL**: The daemon must be the PARENT of the workload to call `wait()`.

Previous bug (FIXED January 2026):
- We were spawning workload first, then forking
- After `fork()`, the `std::process::Child` handle was invalid in the forked child
- The handle was created by the parent process and didn't transfer correctly

Solution:
1. Fork FIRST (creates monitoring daemon)
2. Daemon spawns workload (daemon becomes parent)
3. Daemon can now `wait()` on workload (parent-child relationship)
4. Real exit codes captured, zombies properly reaped

### Current Implementation Status (Jan 2026)

#### ✅ Working (Core Complete!)
- Process spawning with OCI config
- Fork-first monitoring daemon architecture
- Proper parent-child relationship for process waiting
- State persistence with exit codes (JSON files)
- User/group ID management (uid, gid, additional_gids, umask)
- Shim v2 protocol complete (all Task methods)
- Sandbox faking (pause containers)
- Process reaping via `child.wait()` in monitoring daemon
- State transitions: created → running → stopped
- Exit code reporting to Kubernetes
- **Pods correctly show "Completed" status** (validated!)
- TaskExit event publishing with timestamps
- Kill handling (ESRCH for already-dead processes)
- Timing delay for fast processes (500ms)

#### ❌ Not Implemented (By Design)
- Namespaces (intentionally - use host namespaces)
- Cgroups (intentionally)
- Resource limits

## Implementation Details

### OCI Configuration Support
The runtime reads `config.json` from bundle directories:
```json
{
  "process": {
    "args": ["/bin/echo", "hello"],
    "env": ["KEY=value"],
    "cwd": "/tmp",
    "user": {
      "uid": 1000,
      "gid": 1000,
      "additionalGids": [10, 20],
      "umask": 22
    }
  }
}
```

### Container State
Location: `/run/reaper/<container-id>/state.json`
```json
{
  "id": "abc123...",
  "bundle": "/run/containerd/io.containerd.runtime.v2.task/k8s.io/abc123...",
  "status": "running",
  "pid": 12345,
  "exit_code": null
}
```

Status values: `"created"`, `"running"`, `"stopped"`

### User/Group Management (OCI Compliance)
- **OciUser struct**: uid, gid, additional_gids, umask
- **Privilege dropping sequence**: setgroups → setgid → setuid
- **Implementation**: Uses `Command::pre_exec` hook (currently disabled)
- **Security**: Processes inherit runtime UID if no user field specified

### Process Execution
- Uses `std::process::Command`
- Stdio: stdin=null, stdout/stderr=inherit
- Process becomes orphan when runtime exits
- Monitored via PID by shim

## Problem We Solved

### Original Symptom
Kubernetes pods showed status="Running" forever, even though the process (e.g., echo) completed immediately.

### Root Cause Identified
1. Runtime spawned process and exited immediately
2. Process became orphan (adopted by init)
3. Nobody could wait() on the orphan (not a child)
4. State never transitioned to "stopped"
5. Shim polled state, saw "running" forever
6. Containerd reported "Running" to Kubernetes

### Solution Implemented: Fork-First Monitoring Daemon
**The runtime forks FIRST, then the daemon spawns the workload (making daemon the parent).**

```rust
// In reaper-runtime start command - FORK FIRST!
match unsafe { fork() } {
    Ok(ForkResult::Parent { child: daemon_pid }) => {
        // Original runtime process (CLI)
        std::thread::sleep(Duration::from_millis(100)); // Let daemon start
        // Read PID from state (daemon will have updated it)
        println!("started pid={}", workload_pid_from_state);
        std::process::exit(0);
    }
    Ok(ForkResult::Child) => {
        // Monitoring daemon
        setsid(); // Detach from terminal

        // NOW spawn workload - WE are the parent!
        let child = Command::new(program).spawn()?;
        update_state("running", child.id());

        // CRITICAL: Give containerd time to observe "running" state
        std::thread::sleep(Duration::from_millis(500));

        // Wait for workload (we're the parent, so this works!)
        let exit_status = child.wait()?;
        update_state("stopped", exit_status.code());
        std::process::exit(0);
    }
}
```

**Key Benefits:**
1. Monitoring daemon spawns workload → daemon IS the parent
2. `child.wait()` works correctly (parent-child relationship)
3. Real exit codes captured
4. Proper zombie reaping
5. Daemon updates state file when workload exits
6. Shim polls state file and publishes TaskExit event
7. No orphan or zombie processes

## Development Workflow

### Building
```bash
# Local (macOS)
cargo build --release

# Cross-compile for Linux (minikube)
./scripts/minikube-setup-runtime.sh
```

### Testing
```bash
# Unit + integration tests
cargo test

# Specific test suites
cargo test --test integration_basic_binary
cargo test --test integration_user_management
cargo test --test integration_shim

# Deploy to minikube and test
./scripts/minikube-setup-runtime.sh
kubectl apply -f examples/k8s/pod-reaper.yaml
kubectl get pod reaper-example
```

### Debugging

#### State Files
```bash
minikube ssh -- 'sudo find /run/reaper -name state.json -exec cat {} \;'
```

#### Process Status
```bash
minikube ssh -- 'ps aux | grep echo'
# Zombies show as: [echo] <defunct>
```

#### Shim Logs
```bash
minikube ssh -- 'cat /var/log/reaper-shim.log'
```

#### Runtime Logs
```bash
minikube ssh -- 'cat /var/log/reaper-runtime.log'
```

### Git Commits
- User wants control over commits - **do not auto-commit**
- Wait for explicit commit requests

### Code Style
- Rust stable toolchain (pinned via rust-toolchain.toml)
- Run `cargo fmt` and `cargo clippy` before committing
- CI checks: build, test, coverage (75% threshold), audit

## Dependencies
- `nix` crate: POSIX APIs (signals, process, user management)
- `containerd-shim`: Shim v2 protocol implementation
- `containerd-shim-protos`: Protobuf definitions for shim v2
- `ttrpc`: Transport protocol for shim communication
- `anyhow`: Error handling
- `serde`: State serialization

## Bug Fixes (January 2026)

All core issues have been resolved. Here's what was fixed:

### 1. Fork Order Bug
**Problem:** `std::process::Child` handle invalid after fork
**Fix:** Fork FIRST, then spawn workload in the forked child
**File:** `src/bin/reaper-runtime/main.rs:198-311`

### 2. Fast Process Timing
**Problem:** Fast commands (echo) completed before containerd observed "running" state
**Fix:** Added 500ms delay after setting "running" state, before calling `wait()`
**File:** `src/bin/reaper-runtime/main.rs:264-270`

### 3. Kill ESRCH Error
**Problem:** containerd's `kill()` failed with ESRCH for already-dead processes
**Fix:** Treat ESRCH as success (process not running = goal achieved)
**File:** `src/bin/reaper-runtime/main.rs:347-365`

### 4. TaskExit Event Publishing
**Problem:** containerd wasn't recognizing container exits
**Fix:** Publish TaskExit event with proper `exited_at` timestamp
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:162-199`

### 5. Response Timestamps
**Problem:** Missing `exited_at` timestamps in WaitResponse and StateResponse
**Fix:** Include timestamp in all responses for stopped containers
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:545-552, 615-625`

## Debugging Commands

```bash
# Check container state
minikube ssh -- 'cat /run/reaper/$(ls /run/reaper | head -1)/state.json'

# Check for zombies (should be none!)
minikube ssh -- 'ps aux | grep defunct'

# View shim logs
minikube ssh -- 'tail -50 /var/log/reaper-shim.log'

# View runtime logs
minikube ssh -- 'tail -50 /var/log/reaper-runtime.log'

# List running shim processes
minikube ssh -- 'ps aux | grep containerd-shim-reaper'
```

## Next Steps (Future Enhancements)

### Short Term
- Re-enable user/group switching (currently disabled for debugging)
- Add comprehensive error handling
- Reduce startup delay if possible (optimize timing)
- Clean up debug logging

### Medium Term
- Implement exec support (exec into running containers)
- Add resource monitoring (stats)
- Enhanced signal handling

### Long Term
- Optional namespace support
- Optional cgroup integration
- Production deployment guides

## Related Documentation
- OCI Runtime Spec: https://github.com/opencontainers/runtime-spec
- Containerd Shim v2: https://github.com/containerd/containerd/tree/main/runtime/v2
- Kubernetes RuntimeClass: https://kubernetes.io/docs/concepts/containers/runtime-class/
- Focus on `config.json` process section and user fields
