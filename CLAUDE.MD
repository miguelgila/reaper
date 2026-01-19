# CLAUDE.MD - Reaper OCI Runtime

## Quick Start Context

**What is Reaper?** A minimal OCI-compatible container runtime written in Rust that executes commands directly on Kubernetes host nodes without traditional containerization (no namespaces/cgroups by design).

**Status:** ✅ Core functionality complete and validated (January 2026)
- Full containerd shim v2 protocol implementation
- Kubernetes integration via RuntimeClass
- Pods correctly transition to "Completed" status
- Real exit code capture and reporting

## Architecture Overview

### Three-Tier System

```
Kubernetes/Containerd
        ↓ (ttrpc)
containerd-shim-reaper-v2 (long-lived per-container)
        ↓ (exec: create/start/state/delete)
reaper-runtime (short-lived CLI, exits after command)
        ↓ (fork FIRST!)
monitoring daemon → spawns workload → wait() → update state
```

### Critical Innovation: Fork-First Architecture

**The Problem:** OCI requires runtime CLI to exit immediately, but someone needs to `wait()` on the workload to capture exit codes. Only a **parent process** can `wait()` on its children.

**Previous Bug (FIXED Jan 2026):** We spawned workload first, then forked. After `fork()`, the `std::process::Child` handle was invalid in the forked child.

**Solution:** Fork FIRST, then spawn workload in the daemon.

```rust
// In reaper-runtime start command - FORK FIRST!
match unsafe { fork() }? {
    ForkResult::Parent { .. } => {
        sleep(100ms);  // Let daemon start
        exit(0);       // CLI exits immediately (OCI requirement)
    }
    ForkResult::Child => {
        // Monitoring daemon
        setsid();  // Detach from terminal

        // NOW spawn - WE are the parent!
        let child = Command::new(program).spawn()?;
        update_state("running", child.id());

        // CRITICAL: 500ms delay lets containerd observe "running" state
        sleep(500ms);

        // This works because we're the parent!
        let status = child.wait()?;
        update_state("stopped", status.code());
        exit(0);
    }
}
```

**Why This Works:**
1. Daemon spawns workload → daemon is the parent
2. `child.wait()` works (parent-child relationship)
3. Real exit codes captured
4. Proper zombie reaping
5. No orphan processes

## Project Structure

### Binaries
- `src/bin/containerd-shim-reaper-v2/main.rs` - Shim (ttrpc server, long-lived)
- `src/bin/reaper-runtime/main.rs` - Runtime CLI (OCI operations + forking)
- `src/bin/reaper-runtime/state.rs` - State persistence
- `src/main.rs` - Placeholder main binary

### Tests
- `tests/integration_basic_binary.rs` - Basic process execution
- `tests/integration_user_management.rs` - uid/gid handling
- `tests/integration_shim.rs` - Shim v2 protocol tests

### Documentation
- `.github/claude-instructions.md` - This file's predecessor (detailed context)
- `docs/CURRENT_STATE.md` - Implementation status and testing
- `docs/SHIMV2_DESIGN.md` - Shim v2 implementation milestones
- `docs/SHIM_ARCHITECTURE.md` - Architecture and process flow
- `docs/NOTES_FUTURE.md` - Kubernetes security and access control
- `README.md` - Public-facing documentation
- `CONTRIBUTING.md` - Contribution guidelines

### Deployment & Scripts
- `scripts/minikube-setup-runtime.sh` - Build and deploy to minikube
- `scripts/test-k8s-integration.sh` - End-to-end integration test
- `scripts/kind-integration.sh` - Kind cluster integration
- `scripts/docker-coverage.sh` - Coverage in Docker
- `scripts/install-hooks.sh` - Git hooks installer
- `kubernetes/` - RuntimeClass and example pod configs

### Configuration
- `Cargo.toml` - Project dependencies and binaries
- `rust-toolchain.toml` - Rust stable toolchain pin
- `.vscode/` - VS Code settings and launch configs
- `.github/workflows/` - CI/CD (test, build, coverage, security)

## Key Dependencies

```toml
# OCI & Containerd
containerd-shim = "0.10"
containerd-shim-protos = "0.10"
ttrpc = "0.8"

# Async runtime
tokio = "1"
async-trait = "0.1"

# Process management
nix = "0.28"  # POSIX APIs (fork, signals, setuid/setgid)

# Error handling & serialization
anyhow = "1"
serde = "1"
serde_json = "1"

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"
```

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
1. **create**: Creates state with `status="created"`
2. **start**: Daemon updates to `status="running", pid=<workload_pid>`
3. **daemon waits**: Updates to `status="stopped", exit_code=<code>` when workload exits
4. **delete**: Removes state file and directory

## Critical Bug Fixes (January 2026)

All core issues have been resolved:

### 1. Fork Order Bug
**File:** `src/bin/reaper-runtime/main.rs:188-311`
**Problem:** `std::process::Child` handle invalid after fork
**Fix:** Fork first, then spawn workload in the forked child

### 2. Fast Process Timing
**File:** `src/bin/reaper-runtime/main.rs:264-270`
**Problem:** Fast commands (echo) completed before containerd observed "running" state
**Fix:** Added 500ms delay after setting "running" state, before calling `wait()`

### 3. Kill ESRCH Error
**File:** `src/bin/reaper-runtime/main.rs:347-365`
**Problem:** `kill()` failed with ESRCH for already-dead processes
**Fix:** Treat ESRCH as success (process not running = goal achieved)

### 4. TaskExit Event Publishing
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:162-199`
**Problem:** containerd wasn't recognizing container exits
**Fix:** Publish TaskExit event with proper `exited_at` timestamp

### 5. Response Timestamps
**File:** `src/bin/containerd-shim-reaper-v2/main.rs:545-552, 615-625`
**Problem:** Missing `exited_at` timestamps in WaitResponse and StateResponse
**Fix:** Include proper timestamps in all responses for stopped containers

## Development Workflow

### Local Development (macOS/Linux)
```bash
# Build all binaries
cargo build --release

# Run all tests
cargo test

# Run specific test suites
cargo test --test integration_basic_binary
cargo test --test integration_user_management
cargo test --test integration_shim

# Format and lint
cargo fmt --all
cargo clippy --all-targets --all-features
```

### Git Hooks
```bash
# Install pre-commit hook (auto-formats on commit)
chmod +x .githooks/pre-commit
./scripts/install-hooks.sh
```

### Docker-Based Testing (Linux parity)
```bash
# Run tests in Docker (optional)
./scripts/docker-test.sh

# Run coverage with tarpaulin
./scripts/docker-coverage.sh
```

### Kubernetes Integration Testing
```bash
# Full minikube setup and deployment
./scripts/minikube-setup-runtime.sh

# Or kind cluster setup
./scripts/kind-integration.sh

# Deploy test pod
kubectl apply -f kubernetes/runtimeclass.yaml
kubectl get pod reaper-example
# Expected: Completed (0 restarts)

# Check logs
kubectl logs reaper-example
```

### Debugging in Kubernetes

```bash
# Check container state
minikube ssh -- 'sudo cat /run/reaper/<container-id>/state.json'

# View shim logs
minikube ssh -- 'tail -50 /var/log/reaper-shim.log'

# View runtime logs
minikube ssh -- 'tail -50 /var/log/reaper-runtime.log'

# Check for zombies (should be none!)
minikube ssh -- 'ps aux | grep defunct'

# List running shim processes
minikube ssh -- 'ps aux | grep containerd-shim-reaper'
```

## Implementation Status

### ✅ Completed
- [x] OCI runtime CLI (create/start/state/kill/delete)
- [x] Fork-first monitoring daemon architecture
- [x] State persistence with exit codes
- [x] User/group ID management (uid, gid, additional_gids, umask)
- [x] Shim v2 protocol (all Task methods)
- [x] Sandbox container faking (pause containers)
- [x] Process reaping via `child.wait()`
- [x] State transitions: created → running → stopped
- [x] Exit code reporting to Kubernetes
- [x] TaskExit event publishing with timestamps
- [x] Kill handling for already-exited processes (ESRCH)
- [x] Timing delay for fast processes (500ms)
- [x] Pods correctly show "Completed" status
- [x] Container stdout/stderr capture via FIFOs (kubectl logs integration)
- [x] Integration tests (basic, user management, shim)
- [x] CI/CD (test, build, coverage, security audit)

### 🔄 In Progress
- [ ] Multi-container pods testing
- [ ] Long-running processes validation
- [ ] Error handling edge cases
- [ ] Resource cleanup verification

### ⏳ Not Started
- [ ] Exec into running containers (requires daemon protocol)
- [ ] Resource monitoring (stats without cgroups)
- [ ] Performance optimization (reduce startup delay)
- [ ] Optional namespace support
- [ ] Optional cgroup integration
- [ ] Production deployment guides

### ❌ Not Implemented (By Design)
- Namespaces (intentionally - use host namespaces)
- Cgroups (intentionally - no resource limits)
- Full container isolation (not the goal)

## Current Limitations

### By Design
- **No namespaces:** Processes run in host namespace (intended behavior)
- **No cgroups:** No resource limits enforced
- **No isolation:** Full host access (use case specific)

### Current Implementation
- **User switching disabled:** Temporarily disabled for debugging
  - Code exists in `do_start()` but commented out
  - Uses `Command::pre_exec()` hook
  - Will re-enable after further validation
- **500ms startup delay:** Required for containerd to observe "running" state
  - May be reducible with better synchronization
- **Non-blocking FIFO opens:** May fail if containerd isn't ready
  - Falls back to inherited stdio if FIFO open fails (graceful degradation)

## Configuration & Deployment

### Logging Configuration
Both binaries stay silent by default (required for TTRPC protocol).

Enable logging:
```bash
export REAPER_SHIM_LOG=/var/log/reaper-shim.log
export REAPER_RUNTIME_LOG=/var/log/reaper-runtime.log
```

### Containerd Configuration
```toml
# /etc/containerd/config.toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
  runtime_type = "io.containerd.reaper.v2"
  sandbox_mode = "podsandbox"
```

### Kubernetes RuntimeClass
```yaml
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper-v2
handler: reaper-v2
```

### Example Pod
```yaml
apiVersion: v1
kind: Pod
metadata:
  name: reaper-example
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never  # Important for one-shot tasks!
  containers:
    - name: test
      image: busybox
      command: ["/bin/echo", "Hello from Reaper!"]
```

## Coding Conventions

### Rust Style
- Format: `cargo fmt --all` (enforced by git hooks)
- Linting: `cargo clippy --all-targets --all-features` (enforced by CI)
- Toolchain: Stable (pinned via `rust-toolchain.toml`)

### Error Handling
- Use `anyhow::Result` for most functions
- Use `thiserror` for custom error types
- Propagate errors with `?` operator

### Testing
- Integration tests in `tests/` directory
- Use `tempfile` for isolated test directories
- Use `serial_test` when tests need sequential execution
- Mock containerd interactions where possible

### Logging
- Use `tracing` macros: `error!`, `warn!`, `info!`, `debug!`, `trace!`
- Only log when env vars are set (REAPER_SHIM_LOG, REAPER_RUNTIME_LOG)
- Keep stdout/stderr clean for TTRPC protocol compliance

## CI/CD

GitHub Actions workflows:
- **test.yml** - Runs `cargo test` on push/PR
- **build.yml** - Multi-OS/toolchain matrix build, clippy, formatting
- **coverage.yml** - Runs cargo-tarpaulin and uploads to Codecov (75% threshold)
- **security.yml** - Runs `cargo-audit` for dependency vulnerabilities

All workflows must pass before merge to main.

## Next Steps for Development

### Short Term
1. Re-enable user/group switching (currently disabled)
2. Add comprehensive error handling
3. Test with various workload types
4. Reduce startup delay if possible
5. Clean up debug logging

### Medium Term
1. Implement exec support (requires daemon protocol changes)
2. Add resource monitoring (stats without cgroups)
3. Enhanced signal handling
4. Documentation improvements
5. Example use cases

### Long Term
1. Optional namespace support (configurable)
2. Optional cgroup integration (configurable)
3. Security hardening (see docs/NOTES_FUTURE.md)
4. Production deployment guides
5. Community feedback integration

## Contributing

1. Read `CONTRIBUTING.md`
2. Install git hooks: `./scripts/install-hooks.sh`
3. Run tests before committing: `cargo test`
4. Format code: `cargo fmt --all`
5. Check lints: `cargo clippy --all-targets --all-features`
6. Ensure CI passes on your PR

**Important:** Do not auto-commit unless explicitly requested by the user. Wait for explicit commit instructions.

## Resources

### Official Documentation
- [OCI Runtime Spec](https://github.com/opencontainers/runtime-spec)
- [Containerd Shim v2 Protocol](https://github.com/containerd/containerd/tree/main/runtime/v2)
- [TTRPC Protocol](https://github.com/containerd/ttrpc)
- [Kubernetes RuntimeClass](https://kubernetes.io/docs/concepts/containers/runtime-class/)

### Crate Documentation
- [nix crate](https://docs.rs/nix/latest/nix/)
- [containerd-shim](https://docs.rs/containerd-shim/)
- [tokio](https://docs.rs/tokio/)

### Internal Documentation
- `.github/claude-instructions.md` - Detailed Claude context
- `docs/CURRENT_STATE.md` - Implementation status
- `docs/SHIMV2_DESIGN.md` - Shim v2 milestones
- `docs/SHIM_ARCHITECTURE.md` - Architecture deep dive
- `docs/NOTES_FUTURE.md` - Security and access control

## License

MIT License - see LICENSE file

---

**Document Version:** 1.0
**Last Updated:** January 2026
**Maintained for:** Claude Code AI Assistant
