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

### Process Lifecycle (IMPORTANT!)

#### The Three-Layer Model
```
Kubernetes/Containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2 (LONG-LIVED, one per container)
        ↓ (exec: create/start/state/delete)
reaper-runtime (SHORT-LIVED, exits after each command)
        ↓ (spawn)
Host Process (echo, sh, etc.)
```

#### Lifecycle Timeline
1. **Container Create**
   - Containerd starts `containerd-shim-reaper-v2` process
   - Shim's `create()` calls `reaper-runtime create <bundle>`
   - Runtime validates bundle, creates `/run/reaper/<id>/state.json`
   - Runtime exits with status=created
   - Shim returns to containerd

2. **Container Start**
   - Containerd calls shim's `start()`
   - Shim calls `reaper-runtime start <id>`
   - Runtime spawns host process (e.g., /bin/echo)
   - Runtime updates state to "running" with PID
   - **Runtime exits immediately** (process continues as orphan)
   - Shim returns to containerd

3. **Container Wait** (THE PROBLEM WE'RE SOLVING)
   - Containerd calls shim's `wait()`
   - Shim must detect when process exits and report exit code
   - Options:
     - Poll state file (current, inefficient)
     - Use waitpid() on PID (proposed, efficient)

#### Why Threads in Runtime Don't Work
**CRITICAL**: The runtime is NOT a daemon. It's a CLI command that exits.

1. When `reaper-runtime start` runs:
   - Spawns host process (e.g., echo)
   - Updates state to "running"
   - Returns from main()
   - **Process terminates**

2. Any threads spawned in runtime:
   - Are children of the runtime process
   - Die when runtime process exits
   - Never complete their work

3. Evidence from testing:
   - Threads spawned early in function sometimes write files
   - Threads spawned late never execute
   - Moving `std::process::Child` into threads fails mysteriously
   - Root cause: Process exits before threads run

### Current Implementation Status (Jan 2026)

#### ✅ Working
- Process spawning with OCI config
- Fork-based monitoring daemon (runtime forks to stay as parent)
- Proper parent-child relationship for process waiting
- State persistence with exit codes (JSON files)
- User/group ID management (uid, gid, additional_gids, umask)
- Shim v2 protocol complete
- Sandbox faking (pause containers)
- Process reaping via waitpid in monitoring daemon
- State transitions: created → running → stopped
- Exit code reporting to Kubernetes

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

### Solution Implemented: Fork-Based Monitoring Daemon
**The runtime forks a monitoring daemon that stays as the parent of the workload.**

```rust
// In reaper-runtime start command
let child = spawn_workload();
let workload_pid = child.id();

match unsafe { fork() } {
    Ok(ForkResult::Parent { .. }) => {
        // Original runtime process exits immediately
        println!("started pid={}", workload_pid);
        std::process::exit(0);
    }
    Ok(ForkResult::Child) => {
        // Forked child becomes monitoring daemon
        setsid(); // Detach from terminal

        // Wait for workload (we're the parent!)
        match child.wait() {
            Ok(status) => {
                update_state_to_stopped(status.code());
            }
        }
        std::process::exit(0);
    }
}
```

**Key Benefits:**
1. Monitoring daemon is parent of workload → can waitpid()
2. Proper zombie reaping (parent waits on child)
3. Real exit codes captured
4. Daemon updates state file when workload exits
5. Shim polls state file (simple, reliable)
6. No orphan processes

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

## Next Steps / Active Issues

### Current Investigation
**Why is process monitoring failing?**

1. ✅ Confirmed: Runtime exits immediately after start
2. ✅ Confirmed: Threads in runtime don't complete
3. ✅ Confirmed: Processes become zombies
4. 🔄 Testing: Shim-based waitpid implementation
5. ❓ Unknown: Why aren't shim logs showing wait() calls?

### Debugging Commands
```bash
# Check if shim is calling wait()
minikube ssh -- 'grep "wait()" /var/log/reaper-shim.log'

# Check container state
minikube ssh -- 'cat /run/reaper/$(ls /run/reaper | head -1)/state.json'

# Check for zombies
minikube ssh -- 'ps aux | grep defunct'

# List running shim processes
minikube ssh -- 'ps aux | grep containerd-shim-reaper'
```

## Related Documentation
- OCI Runtime Spec: https://github.com/opencontainers/runtime-spec
- Containerd Shim v2: https://github.com/containerd/containerd/tree/main/runtime/v2
- Focus on `config.json` process section and user fields
