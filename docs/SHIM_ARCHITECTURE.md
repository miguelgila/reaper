# Containerd Shim v2 Architecture

## Overview

Reaper implements a **3-tier OCI runtime architecture**:

```
containerd → containerd-shim-reaper-v2 → reaper-runtime
```

This follows the standard OCI runtime shim pattern where:
1. **containerd** (container manager) calls the shim
2. **shim** (process lifecycle manager) calls the OCI runtime binary
3. **runtime** (container executor) manages actual container lifecycle

## Binary Components

### 1. containerd-shim-reaper-v2
- **Location**: `/usr/local/bin/containerd-shim-reaper-v2`
- **Purpose**: TTRPC-based shim that implements containerd's Shim v2 protocol
- **Responsibilities**:
  - Handle TTRPC requests from containerd
  - Translate containerd API calls to OCI runtime commands
  - Manage communication between containerd and reaper-runtime
  - Report container state back to containerd

### 2. reaper-runtime
- **Location**: `/usr/local/bin/reaper-runtime`
- **Purpose**: OCI-compliant runtime that executes containers
- **Responsibilities**:
  - Parse OCI bundle's `config.json`
  - Execute container processes
  - Manage container state (`created`, `running`, `stopped`)
  - Handle signals and process lifecycle
  - Persist state to `/run/reaper/<container-id>/`

## Architecture Flow

### Container Creation
```
containerd (TTRPC CreateTaskRequest)
  ↓
containerd-shim-reaper-v2 (Task::create)
  ↓ executes
reaper-runtime create <id> --bundle <path>
  ↓ creates
/run/reaper/<id>/state.json (status: created)
```

### Container Start
```
containerd (TTRPC StartRequest)
  ↓
containerd-shim-reaper-v2 (Task::start)
  ↓ executes
reaper-runtime start <id>
  ↓ spawns process, updates state
/run/reaper/<id>/state.json (status: running, pid: 1234)
  ↓ queries for PID
reaper-runtime state <id>
  ↓ returns
{id, bundle, pid, status}
```

### Container State Query
```
containerd (TTRPC StateRequest)
  ↓
containerd-shim-reaper-v2 (Task::state)
  ↓ executes
reaper-runtime state <id>
  ↓ returns JSON
{
  "id": "container-123",
  "bundle": "/var/lib/containerd/.../bundle",
  "pid": 1234,
  "status": "running"
}
```

### Container Kill
```
containerd (TTRPC KillRequest)
  ↓
containerd-shim-reaper-v2 (Task::kill)
  ↓ executes
reaper-runtime kill <id> <signal>
  ↓ sends signal to PID
kill(pid, SIGTERM)
```

### Container Delete
```
containerd (TTRPC DeleteRequest)
  ↓
containerd-shim-reaper-v2 (Task::delete)
  ↓ executes
reaper-runtime delete <id>
  ↓ removes state
rm -rf /run/reaper/<id>/
```

## Implementation Details

### Shim Implementation (main.rs)

#### ReaperShim Struct
```rust
#[derive(Clone)]
struct ReaperShim {
    exit: Arc<ExitSignal>,
    runtime_path: String,  // Path to reaper-runtime binary
}
```

The shim discovers the runtime binary via:
1. `REAPER_RUNTIME_PATH` environment variable (if set)
2. Default path: `/usr/local/bin/reaper-runtime`

#### ReaperTask Struct
```rust
#[derive(Clone)]
struct ReaperTask {
    runtime_path: String,  // Inherited from ReaperShim
}
```

Each task operation invokes the runtime binary:

```rust
// Example: create operation
Command::new(&self.runtime_path)
    .arg("create")
    .arg(&req.id)
    .arg("--bundle")
    .arg(&req.bundle)
    .output()
    .await
```

### Runtime CLI (reaper-runtime)

OCI runtime commands:
- `create <id> --bundle <path>` - Create container from bundle
- `start <id>` - Start the container process
- `state <id>` - Query container state (JSON output)
- `kill <id> <signal>` - Send signal to container
- `delete <id>` - Remove container and cleanup

### State Management

The shim does **NOT** maintain in-memory state. All state queries are delegated to the runtime, which persists state in `/run/reaper/<container-id>/state.json`.

This design ensures:
- State survives shim restarts
- Single source of truth (runtime owns state)
- Simplified shim implementation (stateless bridge)

## Deployment Requirements

### Both Binaries Required

The system requires **BOTH** binaries to be deployed:

1. **containerd-shim-reaper-v2** at `/usr/local/bin/`
   - Discovered by containerd via `runtime_type = "io.containerd.reaper.v2"`
   - Must be executable
   - Logs to file only if `REAPER_SHIM_LOG` env var is set (prevents stdout/stderr pollution)

2. **reaper-runtime** at `/usr/local/bin/`
   - Invoked by shim for all container operations
   - Must be executable
   - Must be in shim's PATH or at default location

### Logging Configuration

**IMPORTANT**: The shim does NOT log to stdout/stderr by default. Containerd communicates with shims via stdout/stderr using the TTRPC binary protocol, so any text output would corrupt the communication.

To enable shim logging:
```bash
export REAPER_SHIM_LOG=/var/log/reaper-shim.log
```

When `REAPER_SHIM_LOG` is set, the shim will:
- Write logs to the specified file
- Disable ANSI color codes (plain text only)
- Append to the file (not overwrite)

Without `REAPER_SHIM_LOG`, the shim runs silently.

### Containerd Configuration

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
  runtime_type = "io.containerd.reaper.v2"
  sandbox_mode = "podsandbox"
  # NO options section - causes cgroup errors
```

### Kubernetes RuntimeClass

```yaml
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper-v2
handler: reaper-v2
```

## Troubleshooting

### Logs Show ANSI Color Codes / "Invalid Argument" Error
**Cause**: Shim was logging to stdout/stderr, polluting TTRPC communication
**Fix**: Shim now only logs when `REAPER_SHIM_LOG` env var is set. Set it to a file path for debugging:
```bash
export REAPER_SHIM_LOG=/var/log/reaper-shim.log
```

### "Env(NotPresent)" Error
**Cause**: Shim was trying to execute directly instead of calling runtime binary
**Fix**: Refactored shim to invoke reaper-runtime for all operations

### "runtime binary not found"
**Cause**: reaper-runtime not deployed or not in PATH
**Fix**: Ensure both binaries are deployed and executable

### Cgroup Errors
**Cause**: `options` section in containerd config
**Fix**: Remove options section from containerd runtime config

### TTRPC Socket Creation Failure
**Cause**: Shim not properly implementing protocol or logging to stdout
**Fix**: Use `containerd_shim::asynchronous::run()` and disable stdout logging

### Local Testing
```bash
# Build both binaries
cargo build --bin reaper-runtime --bin containerd-shim-reaper-v2

# Test runtime CLI directly
./target/debug/reaper-runtime --help
./target/debug/reaper-runtime create test1 --bundle /path/to/bundle
./target/debug/reaper-runtime state test1
./target/debug/reaper-runtime start test1
./target/debug/reaper-runtime kill test1 15
./target/debug/reaper-runtime delete test1
```

### Minikube Testing
```bash
# Deploy both binaries and configure containerd
./scripts/minikube-setup-runtime.sh

# Create test pod
kubectl apply -f kubernetes/pod-example.yaml
kubectl logs reaper-example
```

### Kind Testing
```bash
# Deploy and run integration test
./scripts/kind-integration.sh
```

## Future Enhancements

### Planned Features
- [ ] Namespace support (user, PID, mount, network)
- [ ] Cgroup support (resource limits)
- [ ] Multi-process exec support
- [ ] TTY and stdin/stdout handling
- [ ] Checkpoint/restore support

### Out of Scope (Current Milestone)
- Full containerization (namespaces/cgroups)
- Interactive terminals (resize_pty)
- Advanced exec functionality
- Resource statistics beyond basic placeholder

## References

- [OCI Runtime Specification](https://github.com/opencontainers/runtime-spec)
- [Containerd Shim v2 Protocol](https://github.com/containerd/containerd/tree/main/runtime/v2)
- [TTRPC Protocol](https://github.com/containerd/ttrpc)
