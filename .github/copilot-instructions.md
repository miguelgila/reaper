# Reaper - OCI Runtime Context

## Project Overview
Reaper is a minimal OCI-compatible container runtime written in Rust. It focuses on running host binaries using OCI container semantics without full containerization (no namespaces/cgroups initially).

## Architecture

### Core Components
- **reaper-runtime**: OCI-compatible CLI (`src/bin/reaper-runtime/main.rs`)
  - Commands: create, start, state, kill, delete
  - State persistence in `/run/reaper/<container-id>/`
  - JSON-based container state management

### Key Files
- `src/bin/reaper-runtime/main.rs` - Main runtime CLI and process execution
- `src/bin/reaper-runtime/state.rs` - Container state persistence
- `tests/integration_basic_binary.rs` - Basic binary execution tests
- `tests/integration_user_management.rs` - uid/gid management tests

## Implementation Details

### OCI Configuration Support
The runtime reads `config.json` from bundle directories with the following structure:
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

### User/Group Management (OCI Compliance)
- **OciUser struct**: uid, gid, additional_gids, umask
- **Privilege dropping sequence**: setgroups → setgid → setuid
- **Implementation**: Uses `Command::pre_exec` hook in child process
- **Security**: Processes inherit runtime UID if no user field specified
- **Root support**: uid=0 allowed per OCI spec (requires runtime to run as root)

### Process Execution
- Uses `std::process::Command` with `CommandExt::pre_exec`
- Stdio handling: stdin=null, stdout/stderr=inherit (passes through to parent)
- State transitions: created → running → stopped

### Dependencies
- `nix` crate with features: `["signal", "process", "user"]`
- Uses `nix::libc` for setgroups (not available in nix::unistd)
- Uses `nix::unistd::{setuid, setgid, Uid, Gid}` for user switching

## Testing Strategy

### Unit Tests (15 total)
- **State management** (10 tests in state.rs): ContainerState, save/load, paths
- **Config parsing** (5 tests in main.rs): OciUser deserialization, backward compatibility

### Integration Tests (8 total)
- **Basic execution** (3 tests): echo, shell scripts, error handling
- **User management** (5 tests): uid/gid switching, umask, additional groups, root user

### Running Tests
```bash
cargo test                    # All tests
cargo test --test integration_basic_binary
cargo test --test integration_user_management
```

## Important Constraints

### Current Scope
- ✅ Binary execution with OCI config syntax
- ✅ Process uid/gid/groups management
- ✅ State persistence and lifecycle
- ✅ Shim v2 integration with containerd
- 🔄 Process monitoring and state transitions (IN PROGRESS)
- ❌ Namespaces (not implemented - use host namespaces)
- ❌ Cgroups (not implemented)

### Process Lifecycle Architecture
**CRITICAL UNDERSTANDING**: The runtime is a short-lived CLI tool, NOT a daemon.

#### Lifecycle Flow
1. **containerd-shim-reaper-v2** (long-lived per-container process)
   - Started by containerd for each container
   - Calls `reaper-runtime create|start|delete` as needed
   - Monitors container process lifecycle
   - Reports state back to containerd/kubelet

2. **reaper-runtime** (short-lived command invocations)
   - `create`: Validates bundle, saves initial state
   - `start`: Spawns host process, updates state to "running", **returns immediately**
   - `state`: Reads and returns current state JSON
   - `delete`: Cleans up state directory
   - `kill`: Sends signal to process

3. **Process Monitoring Challenge**
   - After `reaper-runtime start` returns, the spawned process is orphaned
   - The runtime process exits, so it cannot wait() on the child
   - Threads spawned in `do_start()` die when the runtime process exits
   - **Solution**: The SHIM must monitor the process, not the runtime

#### Current Implementation Status (Jan 2026)
- ✅ Runtime spawns processes with fork-based monitoring daemon
- ✅ Monitoring daemon stays alive as parent of workload process
- ✅ Daemon waits for workload completion and updates state
- ✅ State includes exit_code field
- ✅ Shim polls state file for status changes
- ✅ Proper parent-child relationship for process reaping

### Key Learnings from Investigation

#### Why Threads in Runtime Don't Work
1. **Thread Timing Issue**: Threads spawned at the beginning of `do_start()` execute, but threads spawned near the end don't
2. **Process Exit**: When `do_start()` returns Ok(()), the runtime process exits
3. **Thread Termination**: All spawned threads are killed when the parent process exits
4. **Test Evidence**:
   - `TEST1-SIMPLE.txt` (spawned early) appears
   - `THREAD-STARTED.txt` (spawned late) never appears
   - This is NOT a threading problem - it's a process lifecycle problem

#### Why Moving Child Fails
1. **Observation**: Simple threads with `move ||` work fine
2. **Observation**: Threads trying to move `std::process::Child` never execute
3. **Evidence**: Thread-started file is 0 bytes, or doesn't exist
4. **Root Cause**: Still unknown, but irrelevant since threads won't work anyway

#### Correct Architecture (Implemented)
The runtime forks a monitoring daemon:
```rust
// In reaper-runtime start command
let child = spawn_workload_process();
let workload_pid = child.id();

match unsafe { fork() } {
    Ok(ForkResult::Parent { .. }) => {
        // Parent returns immediately (start command completes)
        println!("started pid={}", workload_pid);
        std::process::exit(0);
    }
    Ok(ForkResult::Child) => {
        // Child becomes monitoring daemon
        setsid(); // Detach from terminal

        // Wait for workload to complete
        match child.wait() {
            Ok(status) => {
                let exit_code = status.code().unwrap_or(1);
                update_state_to_stopped(exit_code);
            }
        }
        std::process::exit(0);
    }
}
```

Process tree:
```
containerd-shim-reaper-v2 (long-lived)
  └─ reaper-runtime start (exits immediately after fork)
       ├─ workload process (child of daemon)
       └─ monitoring daemon (stays alive, waits for workload)
```

### Security Considerations
1. Without user field in config.json, processes run as runtime's effective UID
2. Setting uid=0 requires runtime to have CAP_SETUID or run as root
3. Setting supplementary groups requires appropriate permissions

## Development Workflow

### Git Commits
- User wants control over commits - **do not auto-commit**
- Wait for explicit commit requests

### Code Style
- Rust stable toolchain (pinned via rust-toolchain.toml)
- Run `cargo fmt` and `cargo clippy` before committing
- CI checks: build, test, coverage (75% threshold), audit

### State Management
- State root: `$REAPER_RUNTIME_ROOT` or default `/run/reaper`
- Container directory: `<state_root>/<container_id>/`
- Files: `state.json`, `pid`

## Related Documentation
- OCI Runtime Spec: https://github.com/opencontainers/runtime-spec
- Focus on `config.json` process section and user fields
- Reference: process.user.{uid,gid,additionalGids,umask}

## Next Steps / TODO
- [ ] Add more comprehensive error handling
- [ ] Consider adding namespace support (future)
- [ ] Consider adding cgroup support (future)
- [ ] Implement shim v2 protocol for Kubernetes (blocked - needs research)
- [ ] Add more integration tests for edge cases
