# Current State - Reaper OCI Runtime (January 2026)

## Quick Summary

**Status:** ✅ Core functionality complete and validated

**What Works:**
- Full OCI runtime implementation (create, start, state, kill, delete)
- Containerd shim v2 protocol complete
- Fork-based process monitoring with real exit codes
- Proper zombie process reaping
- State persistence and lifecycle management
- Kubernetes integration via RuntimeClass
- Pods correctly transition to "Completed" status

**Validated:** Pods running `/bin/echo` correctly show `Completed` status with `exitCode: 0`

## Architecture Overview

### Three-Tier System

```
Kubernetes/Containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2 (long-lived per-container)
        ↓ (exec: create/start/state/delete)
reaper-runtime (short-lived CLI)
        ↓ (fork)
monitoring daemon → spawns workload → wait() → update state
```

### Process Lifecycle

1. **Container Create**
   - Shim calls `reaper-runtime create <bundle>`
   - Runtime validates bundle, creates state file
   - State: `status="created"`

2. **Container Start** (CRITICAL FLOW)
   - Shim calls `reaper-runtime start <id>`
   - Runtime **forks first** (creates monitoring daemon)
   - Parent (CLI) waits 100ms then exits
   - Child (daemon) calls `setsid()` to detach
   - Daemon **spawns workload** (now daemon is workload's parent)
   - Daemon updates state: `status="running", pid=<workload_pid>`
   - Daemon sleeps 500ms (allows containerd to observe "running" state)
   - Daemon calls `child.wait()` (blocks until workload exits)

3. **Process Monitoring**
   - Daemon's `wait()` returns when workload exits
   - Daemon captures real exit code from `ExitStatus`
   - Daemon updates state: `status="stopped", exit_code=<code>`
   - Daemon exits cleanly (no lingering processes)

4. **Container Completion**
   - Shim polls state file via `reaper-runtime state`
   - Detects `status="stopped"`
   - Publishes `TaskExit` event to containerd
   - Returns exit code via `WaitResponse` with `exited_at` timestamp
   - Kubernetes marks pod as `Completed`

### Key Innovation: Fork-First Architecture

**Problem:**
- OCI spec requires runtime CLI to exit immediately
- Someone needs to `wait()` on the workload to get exit code
- Only the **parent process** can call `wait()` on a child

**Previous Bug (FIXED):**
We were spawning the workload first, then forking. After `fork()`, the `std::process::Child` handle was invalid in the forked child because it was created by the parent process.

**Solution:** Fork FIRST, then spawn
```rust
match unsafe { fork() }? {
    ForkResult::Parent { child: daemon_pid } => {
        // CLI waits briefly for daemon to start, then exits
        sleep(100ms);
        println!("started pid={}", workload_pid);
        exit(0);
    }
    ForkResult::Child => {
        // Daemon: detach, spawn workload, wait, update state
        setsid();  // Become session leader

        let child = Command::new(program).spawn()?;  // WE are the parent!
        update_state_to_running(child.id());

        sleep(500ms);  // Let containerd observe "running" state

        let exit_status = child.wait()?;  // This works because we're the parent!
        update_state_to_stopped(exit_status.code());
        exit(0);
    }
}
```

**Why This Works:**
- Monitoring daemon spawns the workload, making daemon the **parent**
- Parent can call `wait()` to get real exit code
- Properly reaps zombie process
- No orphan processes
- Clean lifecycle: daemon exits after workload completes

## Critical Bug Fixes (January 2026)

### 1. Fork Order Bug
**Problem:** `std::process::Child` handle invalid after fork
**Fix:** Fork first, then spawn workload in the forked child
**File:** `src/bin/reaper-runtime/main.rs:188-311`

### 2. Fast Process Timing
**Problem:** Very fast commands (like `echo`) completed before containerd observed "running" state
**Fix:** Added 500ms delay after setting "running" state, before calling `wait()`
**File:** `src/bin/reaper-runtime/main.rs:264-270`

### 3. Kill ESRCH Error
**Problem:** When containerd received TaskExit event, it called `kill()` which failed with ESRCH (process already dead), causing containerd to fail the exit handling
**Fix:** Treat ESRCH as success in `do_kill()` - the goal (process not running) is achieved
**File:** `src/bin/reaper-runtime/main.rs:347-365`

### 4. TaskExit Event Publishing
**Problem:** Containerd wasn't recognizing container exits
**Fix:** Publish `TaskExit` event with proper timestamp when container stops
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:162-199`

### 5. WaitResponse Timestamps
**Problem:** Missing `exited_at` timestamp in WaitResponse
**Fix:** Include proper timestamp in all WaitResponse and StateResponse messages
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:545-552`

## File Structure

### Binaries
- `src/bin/containerd-shim-reaper-v2/main.rs` - Shim implementation (ttrpc server)
- `src/bin/reaper-runtime/main.rs` - Runtime CLI (OCI operations + forking)
- `src/bin/reaper-runtime/state.rs` - State persistence

### Tests
- `tests/integration_basic_binary.rs` - Basic process execution
- `tests/integration_user_management.rs` - uid/gid handling
- `tests/integration_shim.rs` - Shim protocol tests

### Documentation
- `.github/copilot-instructions.md` - GitHub Copilot context
- `.github/claude-instructions.md` - Claude AI context
- `docs/SHIM_ARCHITECTURE.md` - Shim v2 protocol details
- `docs/SHIMV2_DESIGN.md` - Implementation milestones
- `docs/NOTES_FUTURE.md` - Future enhancements
- `docs/CURRENT_STATE.md` - This file

### Deployment
- `scripts/minikube-setup-runtime.sh` - Build and deploy to minikube
- `kubernetes/runtimeclass.yaml` - RuntimeClass and example pod

## State Management

### State File Location
`/run/reaper/<container-id>/state.json`

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

### Status Values
- `"created"` - Container created, not started
- `"running"` - Process executing
- `"stopped"` - Process exited

### Lifecycle
1. **create**: Creates state with `status="created", pid=null, exit_code=null`
2. **start**: Daemon updates to `status="running", pid=<workload_pid>`
3. **daemon**: Updates to `status="stopped", exit_code=<code>` when workload exits
4. **delete**: Removes state file and directory

## Building and Testing

### Build for Local Testing
```bash
cargo build --release
cargo test
```

### Build and Deploy to Minikube
```bash
./scripts/minikube-setup-runtime.sh
```

This script:
1. Starts/restarts minikube with containerd
2. Builds both binaries for Linux (musl, cross-compiled)
3. Copies binaries to minikube node
4. Configures containerd with reaper-v2 runtime
5. Creates RuntimeClass and example pod
6. Sets up logging environment variables

### Test Pod Deployment
```bash
kubectl apply -f kubernetes/runtimeclass.yaml
kubectl get pod reaper-example
# Should show: Completed (0 restarts)
```

### Expected Output
```
NAME             READY   STATUS      RESTARTS   AGE
reaper-example   0/1     Completed   0          5s
```

### Check Container State
```bash
minikube ssh -- 'sudo cat /run/reaper/<container-id>/state.json'
```

### View Logs
```bash
# Shim logs
minikube ssh -- 'tail -50 /var/log/reaper-shim.log'

# Runtime logs
minikube ssh -- 'tail -50 /var/log/reaper-runtime.log'
```

## Testing Checklist

### ✅ Completed
- [x] Binary execution (echo, sh -c)
- [x] Process spawning with fork-first architecture
- [x] State file creation and updates
- [x] Fork-based monitoring daemon
- [x] Exit code capture (validated: exitCode=0)
- [x] Zombie reaping (no defunct processes)
- [x] Shim v2 protocol (all Task methods)
- [x] Sandbox container faking (pause containers)
- [x] TaskExit event publishing
- [x] Proper timestamps in responses
- [x] Kill handling for already-exited processes
- [x] Pod status transitions to "Completed"
- [x] restartPolicy: Never for one-shot tasks

### 🔄 In Progress
- [ ] Multi-container pods
- [ ] Long-running processes
- [ ] Error handling edge cases
- [ ] Resource cleanup verification

### ⏳ Not Started
- [ ] User/group ID management (currently disabled)
- [ ] Signal handling robustness
- [ ] Exec into running containers
- [ ] Resource monitoring (stats)
- [ ] Performance optimization

## Known Limitations

### By Design
- **No namespaces:** Processes run in host namespace
- **No cgroups:** No resource limits enforced
- **No isolation:** Full host access (intended use case)

### Current Implementation
- **User switching disabled:** Temporarily disabled for debugging
  - Code exists in `do_start()` but commented out
  - Uses `Command::pre_exec()` hook
  - Will re-enable after core functionality validated

- **Basic error handling:** Some edge cases not covered
  - Daemon failure scenarios
  - State file corruption
  - Concurrent access to state

- **No exec support:** Can't execute commands in running containers
  - Would require daemon to accept commands
  - Not critical for current use case

- **500ms startup delay:** Added for timing correctness
  - Required for containerd to observe "running" state
  - May be reducible with better synchronization

## Next Steps

### Short Term
1. Re-enable user/group switching
2. Add comprehensive error handling
3. Test with various workload types
4. Reduce startup delay if possible
5. Clean up debug logging

### Medium Term
1. Implement exec support
2. Add resource monitoring
3. Enhanced signal handling
4. Documentation polish
5. Example use cases

### Long Term
1. Namespace support (optional)
2. Cgroup integration (optional)
3. Security hardening
4. Production deployment guides
5. Community feedback integration

## How to Continue Development

### Starting a New Session

1. **Read context files:**
   - `.github/claude-instructions.md` (for Claude)
   - `docs/CURRENT_STATE.md` (this file)

2. **Check recent changes:**
   ```bash
   git log --oneline -10
   git status
   ```

3. **Deploy and test:**
   ```bash
   ./scripts/minikube-setup-runtime.sh
   kubectl get pod reaper-example  # Should show Completed
   ```

### Key Files to Understand

**For runtime changes:**
- `src/bin/reaper-runtime/main.rs` (especially `do_start()` and `do_kill()`)
- `src/bin/reaper-runtime/state.rs`

**For shim changes:**
- `src/bin/containerd-shim-reaper-v2/main.rs` (especially `Task` trait impl)

**For deployment:**
- `kubernetes/runtimeclass.yaml`
- `scripts/minikube-setup-runtime.sh`

## References

- **OCI Runtime Spec:** https://github.com/opencontainers/runtime-spec
- **Containerd Shim v2:** https://github.com/containerd/containerd/tree/main/runtime/v2
- **Kubernetes RuntimeClass:** https://kubernetes.io/docs/concepts/containers/runtime-class/
- **Rust nix crate:** https://docs.rs/nix/latest/nix/

---

**Document Version:** 2.0
**Last Updated:** January 2026
**Status:** Core Functionality Complete
