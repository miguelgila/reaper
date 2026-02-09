# Containerd Shim v2 Architecture

## Overview

Reaper implements a **3-tier OCI runtime architecture**:

```
containerd → containerd-shim-reaper-v2 → reaper-runtime → monitoring daemon → workload
```

This follows the standard OCI runtime shim pattern where:
1. **containerd** (container manager) calls the shim
2. **shim** (process lifecycle manager) calls the OCI runtime binary
3. **runtime** (container executor) forks a monitoring daemon
4. **monitoring daemon** spawns and monitors the workload

## Binary Components

### 1. containerd-shim-reaper-v2
- **Location**: `/usr/local/bin/containerd-shim-reaper-v2`
- **Purpose**: TTRPC-based shim that implements containerd's Shim v2 protocol
- **Lifetime**: Long-lived (one per container)
- **Responsibilities**:
  - Handle TTRPC requests from containerd
  - Translate containerd API calls to OCI runtime commands
  - Poll state file for status changes
  - Publish TaskExit events when containers stop
  - Report container state back to containerd

### 2. reaper-runtime
- **Location**: `/usr/local/bin/reaper-runtime`
- **Purpose**: OCI-compliant runtime CLI
- **Lifetime**: Short-lived (exits after each command)
- **Responsibilities**:
  - Parse OCI bundle's `config.json`
  - Fork monitoring daemon (on `start`)
  - Manage container state (`created`, `running`, `stopped`)
  - Handle signals and process lifecycle
  - Persist state to `/run/reaper/<container-id>/`

### 3. Monitoring Daemon
- **Location**: Forked by reaper-runtime
- **Purpose**: Monitor workload process lifecycle
- **Lifetime**: Lives until workload completes
- **Responsibilities**:
  - Spawn the workload process (becomes its parent)
  - Call `wait()` to capture exit code
  - Update state file when workload exits
  - Exit cleanly (no lingering processes)

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

### Container Start (Fork-First Architecture)
```
containerd (TTRPC StartRequest)
  ↓
containerd-shim-reaper-v2 (Task::start)
  ↓ executes
reaper-runtime start <id>
  ↓ FORK FIRST (CRITICAL!)
       ├─ Parent process (CLI): waits 100ms, then exits
       └─ Child (monitoring daemon):
            ↓ setsid() - detach from terminal
            ↓ spawn workload
            echo "hello" (PID 1234)
            ↓ update state to "running"
            ↓ sleep 500ms (let containerd observe running state)
            ↓ child.wait() - blocks until workload exits
            ↓ update state to "stopped" with exit_code
            ↓ exit
```

**Process Tree During Execution:**
```
containerd-shim-reaper-v2 (PID 100, long-lived)
  └─ [calls reaper-runtime start]

After fork:
init (PID 1)
  └─ monitoring daemon (PID 201, session leader)
       └─ workload process (PID 1234, child of daemon)
```

**Key Point:** The monitoring daemon spawns the workload, making itself the workload's parent. This allows it to call `wait()` and capture the real exit code.

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
  "status": "stopped",
  "exit_code": 0
}
```

### Container Kill
```
containerd (TTRPC KillRequest)
  ↓
containerd-shim-reaper-v2 (Task::kill)
  ↓ executes
reaper-runtime kill <id> <signal>
  ↓ sends signal (or returns OK if ESRCH - process already dead)
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

## Fork-First Architecture (CRITICAL)

### The Problem

Container runtimes face a fundamental challenge:
1. OCI spec requires runtime CLI to exit immediately after `start`
2. Someone needs to `wait()` on the workload to get exit code
3. Only a process's **parent** can call `wait()` on it

### Previous Bug (FIXED January 2026)

We originally implemented: spawn workload first, then fork.

```rust
// WRONG - DO NOT DO THIS
let child = Command::new(program).spawn()?;  // Spawn first
match unsafe { fork() }? {
    ForkResult::Child => {
        child.wait();  // FAILS! child handle invalid after fork
    }
}
```

**Why it failed:** After `fork()`, the `std::process::Child` handle was invalid in the forked child because it was created by the parent process. The internal file descriptors and state don't transfer correctly across fork.

### Correct Implementation: Fork FIRST

```rust
// CORRECT - Fork first, then spawn in daemon
match unsafe { fork() }? {
    ForkResult::Parent { child: daemon_pid } => {
        // CLI process
        sleep(100ms);  // Let daemon start
        println!("started pid={}", workload_pid);  // Read from state
        exit(0);  // Exit immediately
    }
    ForkResult::Child => {
        // Monitoring daemon
        setsid()?;  // Become session leader, detach from terminal

        // NOW spawn - we will be the parent!
        let child = Command::new(program).spawn()?;
        update_state("running", child.id());

        sleep(500ms);  // Let containerd observe "running" state

        let status = child.wait()?;  // THIS WORKS! We're the parent!
        update_state("stopped", status.code());

        exit(0);
    }
}
```

### Why Fork-First Works

1. **Daemon is the parent of workload**
   - Daemon spawns workload → daemon is parent
   - `wait()` only works on children → daemon can wait ✅

2. **Proper zombie reaping**
   - When workload exits, it becomes zombie
   - Daemon (parent) calls `wait()` → zombie reaped ✅

3. **Real exit codes captured**
   - `wait()` returns actual `ExitStatus`
   - Written to state file
   - Shim reads and reports to containerd

4. **Clean process lifecycle**
   - Daemon exits after updating state
   - No orphan processes
   - No zombie processes

5. **File descriptor isolation**
   - Daemon redirects stdout/stderr to `/dev/null` via `dup2()`
   - Prevents inherited pipes from blocking parent process
   - Fixes ContainerCreating bug from leaked file descriptors

### Timing Considerations

**500ms delay is critical for fast processes:**

Without delay:
```
1. Container starts at T=0
2. Echo completes at T=1ms
3. State becomes "stopped"
4. containerd's wait() sees "stopped" immediately
5. But containerd never saw "running" state!
6. State machine confused → pod stuck in "Running"
```

With 500ms delay:
```
1. Container starts at T=0
2. State becomes "running" at T=1ms
3. Daemon sleeps for 500ms
4. containerd observes "running" state
5. Echo completes (already done, wait() returns)
6. State becomes "stopped" at T=501ms
7. containerd sees proper transition → pod becomes "Completed"
```

## TaskExit Event Publishing

When the shim's `wait()` detects that a container has stopped, it publishes a `TaskExit` event:

```rust
let event = TaskExit {
    container_id: container_id.to_string(),
    id: container_id.to_string(),
    pid,
    exit_status: exit_code,
    exited_at: timestamp,  // REQUIRED!
    ..Default::default()
};

self.publisher.publish(
    Context::default(),
    "/tasks/exit",
    &self.namespace,
    Box::new(event),
).await?;
```

**Critical:** The `exited_at` timestamp must be set! Without it, containerd may not properly handle the exit.

## Kill Handling (ESRCH)

When containerd receives a TaskExit event, it often tries to `kill()` the container as part of cleanup. For already-exited processes, this returns `ESRCH` (no such process).

**Previous bug:** We returned an error on ESRCH, causing containerd to fail exit handling.

**Fix:** Treat ESRCH as success (the goal is achieved - process is not running).

```rust
match nix::sys::signal::kill(pid, sig) {
    Ok(()) => { /* Success */ }
    Err(nix::errno::Errno::ESRCH) => {
        // Process already dead - goal achieved!
        info!("Process {} already exited (ESRCH), treating as success", pid);
    }
    Err(e) => bail!("failed to send signal: {}", e),
}
```

## Implementation Details

### ReaperShim Struct
```rust
#[derive(Clone)]
struct ReaperShim {
    exit: Arc<ExitSignal>,
    runtime_path: String,
    namespace: String,  // For event publishing
}
```

### ReaperTask Struct
```rust
#[derive(Clone)]
struct ReaperTask {
    runtime_path: String,
    sandbox_state: Arc<Mutex<HashMap<String, (bool, u32)>>>,
    publisher: Arc<RemotePublisher>,  // For TaskExit events
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

### Sandbox Container Handling

Kubernetes uses "pause" containers for pod networking. These are detected and handled specially:

```rust
fn is_sandbox_container(bundle: &str) -> bool {
    // Check image name, command, or args for "pause"
}
```

Sandboxes return fake responses immediately (PID 1, exit code 0) without spawning real processes.

### PTY and Exec Support

Reaper supports interactive containers and exec sessions:

**PTY for Interactive Containers:**
- `kubectl run -it` sets `terminal: true` in ContainerState
- Runtime allocates PTY via `openpty()` during `do_start()`
- Relay threads connect stdin FIFO → PTY master and PTY master → stdout FIFO
- Child process becomes session leader and PTY slave becomes controlling terminal via `TIOCSCTTY`

**Exec into Running Containers:**
- Shim's `exec()` writes exec state file with process spec, FIFO paths, and terminal flag
- Runtime's `do_exec()` forks daemon, joins overlay namespace, spawns exec process
- Exec with PTY (`kubectl exec -it`) uses same PTY allocation pattern as interactive containers
- Exec without PTY connects FIFOs directly to stdin/stdout/stderr
- Wait timeout increased to 1 hour to support long-running interactive sessions

## Deployment Requirements

### Both Binaries Required

1. **containerd-shim-reaper-v2** at `/usr/local/bin/`
   - Named exactly `containerd-shim-reaper-v2` (containerd naming convention)
   - Discoverable via `runtime_type = "io.containerd.reaper.v2"`

2. **reaper-runtime** at `/usr/local/bin/`
   - Invoked by shim for OCI operations
   - Configurable via `REAPER_RUNTIME_PATH` env var

### Logging Configuration

Both binaries stay silent by default (required for TTRPC protocol).

Enable logging:
```bash
export REAPER_SHIM_LOG=/var/log/reaper-shim.log
export REAPER_RUNTIME_LOG=/var/log/reaper-runtime.log
```

For systemd:
```bash
sudo mkdir -p /etc/systemd/system/containerd.service.d
sudo tee /etc/systemd/system/containerd.service.d/reaper-logging.conf <<EOF
[Service]
Environment="REAPER_SHIM_LOG=/var/log/reaper-shim.log"
Environment="REAPER_RUNTIME_LOG=/var/log/reaper-runtime.log"
EOF
sudo systemctl daemon-reload
sudo systemctl restart containerd
```

### Containerd Configuration

```toml
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

## Testing

### Run Integration Tests
```bash
./scripts/run-integration-tests.sh
```

This creates a kind cluster, builds the runtime, configures containerd, and runs comprehensive tests (DNS, overlay, host protection, zombie processes, etc.).

For options and troubleshooting, see [TESTING.md](../TESTING.md).

## Troubleshooting

### Pod Stuck in "Running"
**Possible causes:**
1. Timing issue - process completed before containerd saw "running"
2. TaskExit event missing timestamp
3. kill() returning error for dead process

**Solution:** All fixed in January 2026 - ensure you have latest code.

### Zombie Processes
**Cause:** Monitoring daemon not properly reaping workload
**Solution:** Fork-first architecture ensures daemon is parent of workload

### "Process already exited (ESRCH)"
**Status:** This is now handled gracefully and logged as success

### No Logs
**Cause:** Logging env vars not set
**Solution:** Set `REAPER_SHIM_LOG` and `REAPER_RUNTIME_LOG`

## References

- [OCI Runtime Specification](https://github.com/opencontainers/runtime-spec)
- [Containerd Shim v2 Protocol](https://github.com/containerd/containerd/tree/main/runtime/v2)
- [TTRPC Protocol](https://github.com/containerd/ttrpc)

---

**Document Version:** 2.1
**Last Updated:** February 2026
**Status:** Core Implementation Complete with Exec and PTY Support
