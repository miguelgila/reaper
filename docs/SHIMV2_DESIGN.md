# Shim v2 Implementation Design

## Overview

This document outlines the implementation plan for containerd Runtime v2 API (shim protocol) support in Reaper, enabling Kubernetes integration.

## Background

### What is the Shim v2 Protocol?

The containerd Runtime v2 API is the interface between:
- **containerd** (Kubernetes container runtime)
- **Container runtime shim** (our code)
- **Low-level runtime** (our reaper-runtime binary)

```
Kubernetes → CRI → containerd → [Shim v2 API] → reaper-shim → reaper-runtime
```

### Why Do We Need It?

Without shim v2:
- ❌ Kubernetes can't communicate with reaper
- ❌ No pod lifecycle management
- ❌ No container status reporting

With shim v2:
- ✅ Kubernetes can create/start/stop containers
- ✅ Stream logs and exec into containers
- ✅ Monitor container health
- ✅ Full pod lifecycle support

## Architecture Options

### Option 1: Separate Shim Binary (Recommended)

```
containerd-shim-reaper-v2    ← New binary
    ↓
reaper-runtime               ← Existing binary
```

**Pros:**
- Clean separation of concerns
- Follows containerd conventions
- Easier to debug
- Standard approach (runc, kata, etc.)

**Cons:**
- Need to manage two binaries

### Option 2: Integrated Shim

Combine shim and runtime in one binary with different subcommands.

**Pros:**
- Single binary deployment
- Simpler distribution

**Cons:**
- Mixes responsibilities
- Non-standard approach

**Decision: Option 1** - Separate shim binary following containerd best practices

## Shim v2 API Overview

The shim must implement these TTRPC services:

### Task Service (Required)

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

### Key Methods for MVP

**Phase 1 (Essential):**
- `Create` - Create container from OCI bundle
- `Start` - Start the container process
- `Delete` - Remove container
- `Kill` - Send signal to container
- `Wait` - Wait for container exit

**Phase 2 (Important):**
- `Exec` - Execute command in running container
- `Stats` - Get resource usage statistics
- `Pids` - List processes in container
- `Connect` - Connect to existing shim

**Phase 3 (Optional):**
- `Pause/Resume` - Suspend/resume container
- `Checkpoint` - CRIU checkpoint/restore
- `Update` - Update resource constraints
- `ResizePty` - Resize terminal

## Implementation Plan

### Milestone 1: Project Setup

**Tasks:**
- [ ] Add dependencies: `ttrpc`, `protobuf`, `tokio`
- [ ] Generate protobuf code from containerd definitions
- [ ] Create `containerd-shim-reaper-v2` binary crate
- [ ] Set up basic TTRPC server

**Deliverable:** Shim binary that starts and accepts connections

### Milestone 2: Core Task API

**Tasks:**
- [ ] Implement `Create` - parse bundle, call reaper-runtime create
- [ ] Implement `Start` - call reaper-runtime start
- [ ] Implement `Delete` - call reaper-runtime delete
- [ ] Implement `Kill` - call reaper-runtime kill
- [ ] Implement `Wait` - monitor process, return exit code

**Deliverable:** Basic container lifecycle working

### Milestone 3: Process Management

**Tasks:**
- [ ] Implement `Pids` - list container processes
- [ ] Implement `Connect` - reconnect to existing shim
- [ ] Add event publishing (container start/stop/exit)
- [ ] Handle stdout/stderr streaming

**Deliverable:** Full process monitoring

### Milestone 4: Advanced Features

**Tasks:**
- [ ] Implement `Exec` - execute commands in container
- [ ] Implement `Stats` - resource usage metrics
- [ ] Implement `ResizePty` - terminal resizing
- [ ] Add proper error handling and logging

**Deliverable:** Feature-complete shim

### Milestone 5: Kubernetes Integration

**Tasks:**
- [ ] Create RuntimeClass configuration
- [ ] Test with real Kubernetes cluster (minikube/kind)
- [ ] End-to-end pod lifecycle testing
- [ ] Documentation and examples

**Deliverable:** Working Kubernetes integration

## Technical Details

### TTRPC Server Setup

```rust
use ttrpc::Server;
use containerd_shim_protos::shim::TaskService;

fn main() -> Result<()> {
    let task_service = Box::new(TaskServiceImpl::new());
    
    let mut server = Server::new()
        .bind("unix:///run/containerd/reaper.sock")?
        .register_service(task_service);
    
    server.start()?;
    
    // Handle signals, wait for shutdown
    Ok(())
}
```

### Calling reaper-runtime

The shim will spawn reaper-runtime as a child process:

```rust
use std::process::Command;

fn create_container(id: &str, bundle: &Path) -> Result<()> {
    let output = Command::new("/usr/local/bin/reaper-runtime")
        .env("REAPER_RUNTIME_ROOT", "/run/containerd/reaper")
        .arg("create")
        .arg(id)
        .arg("--bundle")
        .arg(bundle)
        .output()?;
    
    if !output.status.success() {
        bail!("create failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    Ok(())
}
```

### State Management

The shim needs to track:
- Container ID → PID mapping
- Process state (created, running, stopped)
- Exit codes
- stdout/stderr pipes

```rust
struct ShimState {
    containers: HashMap<String, ContainerInfo>,
    event_tx: mpsc::Sender<Event>,
}

struct ContainerInfo {
    id: String,
    pid: Option<i32>,
    bundle: PathBuf,
    status: ContainerStatus,
    exit_code: Option<i32>,
    stdout: Option<File>,
    stderr: Option<File>,
}
```

## Dependencies

### New Cargo Dependencies

```toml
[dependencies]
ttrpc = "0.8"
protobuf = "3.3"
containerd-shim-protos = "0.5"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
```

### System Dependencies

- containerd (for testing)
- protobuf compiler (for code generation)

## Testing Strategy

### Unit Tests

- Mock TTRPC client calling shim methods
- Verify correct reaper-runtime invocations
- Test state transitions

### Integration Tests

- Real TTRPC server/client
- End-to-end container lifecycle
- Test with actual OCI bundles

### Kubernetes Tests

- Deploy to minikube/kind
- Create pods with reaper runtime
- Verify pod lifecycle
- Test exec, logs, port-forwarding

## Open Questions

1. **Stdio Handling:** How to stream stdout/stderr from reaper-runtime to containerd?
   - Option A: Use named pipes
   - Option B: Unix domain sockets
   - Option C: Network sockets

2. **Reaper-runtime Changes:** Do we need to modify reaper-runtime for shim compatibility?
   - Probably need to support `--console-socket` for terminal handling
   - May need additional state fields

3. **Event Publishing:** How to notify containerd of container events?
   - Need to implement containerd event format
   - Publish to containerd event stream

4. **Namespace Isolation:** With no kernel namespaces, how do we handle:
   - Network isolation? (Not supported in phase 1)
   - PID conflicts? (Host PID namespace OK for now)

## Resources

- [containerd shim v2 spec](https://github.com/containerd/containerd/blob/main/runtime/v2/README.md)
- [containerd protobuf definitions](https://github.com/containerd/containerd/tree/main/api/runtime/task)
- [runc shim implementation](https://github.com/containerd/containerd/tree/main/runtime/v2/runc)
- [TTRPC protocol](https://github.com/containerd/ttrpc)

## Next Steps

1. Review this design document
2. Set up protobuf code generation
3. Create `containerd-shim-reaper-v2` crate
4. Implement Milestone 1 (Project Setup)
