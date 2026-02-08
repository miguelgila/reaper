# Exec Implementation Plan

## Goal

Enable `kubectl exec -it <pod> -- /bin/sh` for containers running with the reaper runtime.
This requires implementing the containerd shim v2 `exec` lifecycle across three components.

## Background

The exec lifecycle in containerd shim v2 works as follows:

1. **exec()** - containerd sends `ExecProcessRequest` with container_id, exec_id, process spec (args/env/cwd), stdin/stdout/stderr FIFO paths, and a `terminal` flag
2. **start(exec_id)** - containerd tells the shim to actually start the exec process
3. **wait(exec_id)** - containerd blocks until the exec process exits
4. **kill(exec_id)** - containerd sends a signal to the exec process
5. **delete(exec_id)** - containerd cleans up exec state
6. **resize_pty(exec_id)** - containerd resizes the terminal (for interactive sessions)

For interactive exec (`-it`), the `terminal` flag is true and a PTY must be allocated.

## Architecture

```
kubectl exec -it pod -- /bin/sh
        ↓
containerd (creates FIFOs for stdin/stdout/stderr)
        ↓ (ttrpc)
containerd-shim-reaper-v2
   exec(): writes exec state file to /run/reaper/<cid>/exec-<eid>.json
   start(): calls reaper-runtime exec <cid> --exec-id <eid>
        ↓
reaper-runtime exec
   1. Reads exec state file (args, env, cwd, terminal, FIFO paths)
   2. Forks daemon
   3. Parent: polls for PID, exits
   4. Daemon: setsid(), joins overlay namespace
   5. If terminal=true: creates PTY (openpty), spawns process with PTY slave
      - Relay thread: stdin FIFO → PTY master
      - Relay thread: PTY master → stdout FIFO
   6. If terminal=false: spawns process with FIFOs as stdin/stdout/stderr
   7. Waits for process exit
   8. Updates exec state file with exit_code, status="stopped"
        ↓
shim wait(exec_id): polls exec state file until status="stopped"
shim kill(exec_id): reads PID from exec state, sends signal
shim delete(exec_id): removes exec state file
```

## Exec State File Format

Path: `/run/reaper/<container-id>/exec-<exec-id>.json`

```json
{
  "container_id": "abc123",
  "exec_id": "exec1",
  "status": "created",
  "pid": null,
  "exit_code": null,
  "args": ["/bin/sh"],
  "env": ["PATH=/usr/bin:/bin", "TERM=xterm"],
  "cwd": "/",
  "terminal": true,
  "stdin": "/path/to/stdin-fifo",
  "stdout": "/path/to/stdout-fifo",
  "stderr": "/path/to/stderr-fifo"
}
```

Status values: `"created"` → `"running"` → `"stopped"`

## Implementation Steps

### Step 1: Fix Cargo.toml (DONE)

Changed nix feature from `"pty"` (doesn't exist) to `"term"` (correct feature for PTY support).

### Step 2: Add ExecState to `src/bin/reaper-runtime/state.rs`

Add after the existing `delete()` function, before `#[cfg(test)]`:

```rust
/// State for an exec process within a container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecState {
    pub container_id: String,
    pub exec_id: String,
    pub status: String, // created | running | stopped
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub terminal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

pub fn exec_state_path(container_id: &str, exec_id: &str) -> PathBuf {
    container_dir(container_id).join(format!("exec-{}.json", exec_id))
}

pub fn save_exec_state(state: &ExecState) -> anyhow::Result<()> {
    let dir = container_dir(&state.container_id);
    fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(&state)?;
    fs::write(exec_state_path(&state.container_id, &state.exec_id), json)?;
    Ok(())
}

pub fn load_exec_state(container_id: &str, exec_id: &str) -> anyhow::Result<ExecState> {
    let path = exec_state_path(container_id, exec_id);
    let data = fs::read(&path)?;
    let state: ExecState = serde_json::from_slice(&data)?;
    Ok(state)
}

pub fn delete_exec_state(container_id: &str, exec_id: &str) -> anyhow::Result<()> {
    let path = exec_state_path(container_id, exec_id);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}
```

Also add unit tests for exec state save/load/delete.

### Step 3: Add Exec subcommand to `src/bin/reaper-runtime/main.rs`

#### 3a. Add to Commands enum:

```rust
/// Execute a process inside a running container
Exec {
    /// Container ID
    id: String,
    /// Exec process ID
    #[arg(long)]
    exec_id: String,
},
```

#### 3b. Add match arm in main():

```rust
Commands::Exec { ref id, ref exec_id } => do_exec(id, exec_id),
```

#### 3c. Update imports:

Add to the `use state::` import line:
```rust
use state::{..., ExecState, load_exec_state, save_exec_state};
```

#### 3d. Implement `do_exec()`:

```rust
fn do_exec(container_id: &str, exec_id: &str) -> Result<()> {
    info!("do_exec() called - container_id={}, exec_id={}", container_id, exec_id);

    let exec_state = load_exec_state(container_id, exec_id)?;

    let args = exec_state.args.clone();
    if args.is_empty() {
        bail!("exec process args must not be empty");
    }

    let program = args[0].clone();
    let argv: Vec<String> = args[1..].to_vec();
    let cwd = exec_state.cwd.clone();
    let env_vars = exec_state.env.clone();
    let terminal = exec_state.terminal;
    let stdin_path = exec_state.stdin.clone();
    let stdout_path = exec_state.stdout.clone();
    let stderr_path = exec_state.stderr.clone();

    let container_id = container_id.to_string();
    let exec_id = exec_id.to_string();

    use nix::unistd::{fork, ForkResult};

    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => {
            // Poll exec state for PID (same pattern as do_start)
            let mut exec_pid = None;
            for _ in 0..20 {
                if let Ok(state) = load_exec_state(&container_id, &exec_id) {
                    if state.pid.is_some() {
                        exec_pid = state.pid;
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if let Some(pid) = exec_pid {
                println!("exec started pid={}", pid);
            } else {
                println!("exec started pid=0");
            }
            Ok(())
        }
        Ok(ForkResult::Child) => {
            if let Err(e) = nix::unistd::setsid() {
                eprintln!("exec daemon: setsid failed: {}", e);
            }

            // Join overlay namespace (Linux only) - same as do_start
            #[cfg(target_os = "linux")]
            {
                let overlay_config = overlay::read_config();
                if let Err(e) = overlay::enter_overlay(&overlay_config) {
                    tracing::error!("do_exec() - overlay failed: {:#}", e);
                    if let Ok(mut state) = load_exec_state(&container_id, &exec_id) {
                        state.status = "stopped".into();
                        state.exit_code = Some(1);
                        let _ = save_exec_state(&state);
                    }
                    std::process::exit(1);
                }
            }

            let exit_code = if terminal {
                exec_with_pty(&program, &argv, cwd, env_vars, stdin_path, stdout_path, &container_id, &exec_id)
            } else {
                exec_without_pty(&program, &argv, cwd, env_vars, stdin_path, stdout_path, stderr_path, &container_id, &exec_id)
            };

            // Update exec state to stopped
            if let Ok(mut state) = load_exec_state(&container_id, &exec_id) {
                state.status = "stopped".into();
                state.exit_code = Some(exit_code);
                let _ = save_exec_state(&state);
            }

            std::process::exit(0);
        }
        Err(e) => bail!("Fork failed: {}", e),
    }
}
```

#### 3e. Implement `exec_with_pty()`:

```rust
fn exec_with_pty(
    program: &str,
    argv: &[String],
    cwd: Option<String>,
    env_vars: Option<Vec<String>>,
    stdin_path: Option<String>,
    stdout_path: Option<String>,
    container_id: &str,
    exec_id: &str,
) -> i32 {
    use nix::pty::openpty;
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;

    // Create PTY
    let pty = match openpty(None, None) {
        Ok(pty) => pty,
        Err(e) => {
            tracing::error!("openpty failed: {}", e);
            return 1;
        }
    };

    let slave_raw_fd = pty.slave.as_raw_fd();

    let mut cmd = Command::new(program);
    cmd.args(argv);
    if let Some(ref cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if let Some(ref envs) = env_vars {
        for kv in envs {
            if let Some((k, v)) = kv.split_once('=') {
                cmd.env(k, v);
            }
        }
    }

    // PTY slave becomes stdin/stdout/stderr via pre_exec
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    unsafe {
        cmd.pre_exec(move || {
            // New session so we can set controlling terminal
            if nix::libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Set controlling terminal
            if nix::libc::ioctl(slave_raw_fd, nix::libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Dup slave to stdin/stdout/stderr
            nix::libc::dup2(slave_raw_fd, 0);
            nix::libc::dup2(slave_raw_fd, 1);
            nix::libc::dup2(slave_raw_fd, 2);
            if slave_raw_fd > 2 {
                nix::libc::close(slave_raw_fd);
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("exec spawn failed: {}", e);
            return 1;
        }
    };

    let exec_pid = child.id() as i32;

    // Update exec state with PID
    if let Ok(mut state) = load_exec_state(container_id, exec_id) {
        state.status = "running".into();
        state.pid = Some(exec_pid);
        let _ = save_exec_state(&state);
    }

    // Close slave in parent - child has it via dup2
    drop(pty.slave);

    // Convert PTY master OwnedFd to File for I/O
    let master_file: std::fs::File = pty.master.into();
    let master_clone = master_file.try_clone().unwrap_or_else(|e| {
        tracing::error!("failed to clone master fd: {}", e);
        std::process::exit(1);
    });

    // Start relay threads
    // stdin FIFO → PTY master (user input to process)
    if let Some(ref stdin_p) = stdin_path {
        if !stdin_p.is_empty() {
            let stdin_path = stdin_p.clone();
            let mut master_w = master_clone; // master_clone for writing
            std::thread::spawn(move || {
                if let Ok(mut stdin_file) = std::fs::File::open(&stdin_path) {
                    let mut buf = [0u8; 4096];
                    loop {
                        match stdin_file.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if master_w.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            });
        }
    }

    // PTY master → stdout FIFO (process output to user)
    if let Some(ref stdout_p) = stdout_path {
        if !stdout_p.is_empty() {
            let stdout_path = stdout_p.clone();
            let mut master_r = master_file; // master_file for reading
            std::thread::spawn(move || {
                if let Ok(mut stdout_file) = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&stdout_path)
                {
                    let mut buf = [0u8; 4096];
                    loop {
                        match master_r.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if stdout_file.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break, // EIO when slave closes
                        }
                    }
                }
            });
        }
    }

    // Wait for child
    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    }
}
```

#### 3f. Implement `exec_without_pty()`:

```rust
fn exec_without_pty(
    program: &str,
    argv: &[String],
    cwd: Option<String>,
    env_vars: Option<Vec<String>>,
    stdin_path: Option<String>,
    stdout_path: Option<String>,
    stderr_path: Option<String>,
    container_id: &str,
    exec_id: &str,
) -> i32 {
    let mut cmd = Command::new(program);
    cmd.args(argv);
    if let Some(ref cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if let Some(ref envs) = env_vars {
        for kv in envs {
            if let Some((k, v)) = kv.split_once('=') {
                cmd.env(k, v);
            }
        }
    }

    // Connect FIFOs directly
    if let Some(ref p) = stdin_path {
        if !p.is_empty() {
            if let Ok(f) = std::fs::File::open(p) {
                cmd.stdin(Stdio::from(f));
            } else {
                cmd.stdin(Stdio::null());
            }
        } else {
            cmd.stdin(Stdio::null());
        }
    } else {
        cmd.stdin(Stdio::null());
    }

    if let Some(ref p) = stdout_path {
        if !p.is_empty() {
            match open_log_file(p) {
                Ok(f) => cmd.stdout(Stdio::from(f)),
                Err(_) => cmd.stdout(Stdio::inherit()),
            };
        } else {
            cmd.stdout(Stdio::inherit());
        }
    } else {
        cmd.stdout(Stdio::inherit());
    }

    if let Some(ref p) = stderr_path {
        if !p.is_empty() {
            match open_log_file(p) {
                Ok(f) => cmd.stderr(Stdio::from(f)),
                Err(_) => cmd.stderr(Stdio::inherit()),
            };
        } else {
            cmd.stderr(Stdio::inherit());
        }
    } else {
        cmd.stderr(Stdio::inherit());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("exec spawn failed: {}", e);
            return 1;
        }
    };

    let exec_pid = child.id() as i32;

    // Update exec state
    if let Ok(mut state) = load_exec_state(container_id, exec_id) {
        state.status = "running".into();
        state.pid = Some(exec_pid);
        let _ = save_exec_state(&state);
    }

    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    }
}
```

### Step 4: Update containerd shim (`src/bin/containerd-shim-reaper-v2/main.rs`)

#### 4a. Add helper function:

```rust
fn runtime_state_dir() -> String {
    std::env::var("REAPER_RUNTIME_ROOT").unwrap_or_else(|_| "/run/reaper".to_string())
}
```

#### 4b. Update `publish_exit_event()` to accept an exec_id parameter:

Change signature to:
```rust
async fn publish_exit_event(&self, container_id: &str, exec_id: &str, pid: u32, exit_code: u32)
```

Set `event.id` to `exec_id` if non-empty, otherwise `container_id`.

Update the existing call site in `wait()` to pass `""` as exec_id.

#### 4c. Implement `exec()`:

```rust
async fn exec(&self, _ctx: &TtrpcContext, req: api::ExecProcessRequest) -> TtrpcResult<api::Empty> {
    info!("exec() called - container_id={}, exec_id={}, terminal={}", req.id, req.exec_id, req.terminal);

    // Parse process spec from protobuf Any
    let spec = req.spec.as_ref().ok_or_else(|| {
        ttrpc::Error::RpcStatus(ttrpc::get_status(ttrpc::Code::INVALID_ARGUMENT, "missing spec"))
    })?;

    // The spec value is JSON-encoded OCI process spec
    let process: serde_json::Value = serde_json::from_slice(&spec.value).map_err(|e| {
        ttrpc::Error::RpcStatus(ttrpc::get_status(ttrpc::Code::INVALID_ARGUMENT, format!("invalid spec: {}", e)))
    })?;

    let args: Vec<String> = process["args"].as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let env: Option<Vec<String>> = process["env"].as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect());
    let cwd: Option<String> = process["cwd"].as_str().map(String::from);

    if args.is_empty() {
        return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
            ttrpc::Code::INVALID_ARGUMENT, "exec args must not be empty",
        )));
    }

    // Write exec state file for the runtime to read
    let exec_state = serde_json::json!({
        "container_id": req.id,
        "exec_id": req.exec_id,
        "status": "created",
        "pid": null,
        "exit_code": null,
        "args": args,
        "env": env,
        "cwd": cwd,
        "terminal": req.terminal,
        "stdin": if req.stdin.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(req.stdin.clone()) },
        "stdout": if req.stdout.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(req.stdout.clone()) },
        "stderr": if req.stderr.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(req.stderr.clone()) },
    });

    let state_dir = runtime_state_dir();
    let exec_path = format!("{}/{}/exec-{}.json", state_dir, req.id, req.exec_id);

    std::fs::write(&exec_path, serde_json::to_vec_pretty(&exec_state).unwrap())
        .map_err(|e| {
            ttrpc::Error::RpcStatus(ttrpc::get_status(ttrpc::Code::INTERNAL, format!("write exec state: {}", e)))
        })?;

    info!("exec() succeeded - wrote exec state to {}", exec_path);
    Ok(api::Empty::new())
}
```

#### 4d. Update `start()` to handle exec_id:

Add at the beginning of `start()`, before the sandbox check:

```rust
// Handle exec start
if !req.exec_id.is_empty() {
    info!("start() - EXEC process, exec_id={}", req.exec_id);

    let runtime_path = self.runtime_path.clone();
    let container_id = req.id.clone();
    let exec_id = req.exec_id.clone();

    let output = tokio::task::spawn_blocking(move || {
        execute_and_reap_child(&runtime_path, vec!["exec", &container_id, "--exec-id", &exec_id])
    })
    .await
    .map_err(|e| ttrpc::Error::RpcStatus(ttrpc::get_status(ttrpc::Code::INTERNAL, format!("{}", e))))?
    .map_err(|e| ttrpc::Error::RpcStatus(ttrpc::get_status(ttrpc::Code::INTERNAL, format!("{}", e))))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(ttrpc::Code::INTERNAL, format!("exec failed: {}", stderr))));
    }

    // Poll exec state file for PID
    let state_dir = runtime_state_dir();
    let exec_path = format!("{}/{}/exec-{}.json", state_dir, req.id, req.exec_id);
    let exec_path_clone = exec_path.clone();

    let pid = tokio::task::spawn_blocking(move || {
        for _ in 0..20 {
            if let Ok(data) = std::fs::read_to_string(&exec_path_clone) {
                if let Ok(state) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(pid) = state["pid"].as_u64() {
                        if pid > 0 { return pid as u32; }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        0u32
    })
    .await
    .unwrap_or(0);

    info!("start() exec succeeded - exec_id={}, pid={}", req.exec_id, pid);
    return Ok(api::StartResponse { pid, ..Default::default() });
}
```

#### 4e. Update `wait()` to handle exec_id:

Add after the sandbox check, before the workload polling:

```rust
// Handle exec wait
if !req.exec_id.is_empty() {
    info!("wait() - EXEC process, exec_id={}", req.exec_id);

    let state_dir = runtime_state_dir();
    let exec_path = format!("{}/{}/exec-{}.json", state_dir, req.id, req.exec_id);
    let container_id = req.id.clone();
    let exec_id_clone = req.exec_id.clone();

    let (exit_code, pid) = tokio::task::spawn_blocking(move || {
        let timeout = std::time::Duration::from_secs(3600); // 1 hour for interactive
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > timeout {
                return (1i32, 0u32);
            }
            if let Ok(data) = std::fs::read_to_string(&exec_path) {
                if let Ok(state) = serde_json::from_str::<serde_json::Value>(&data) {
                    if state["status"].as_str() == Some("stopped") {
                        let code = state["exit_code"].as_i64().unwrap_or(0) as i32;
                        let pid = state["pid"].as_u64().unwrap_or(0) as u32;
                        return (code, pid);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    })
    .await
    .unwrap_or((1, 0));

    self.publish_exit_event(&container_id, &exec_id_clone, pid, exit_code as u32).await;

    let mut resp = api::WaitResponse::new();
    resp.set_exit_status(exit_code as u32);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let mut timestamp = ::protobuf::well_known_types::timestamp::Timestamp::new();
    timestamp.seconds = now.as_secs() as i64;
    timestamp.nanos = now.subsec_nanos() as i32;
    resp.exited_at = ::protobuf::MessageField::some(timestamp);

    return Ok(resp);
}
```

#### 4f. Update `kill()` to handle exec_id:

Add after the sandbox check, before the workload kill:

```rust
// Handle exec kill
if !req.exec_id.is_empty() {
    info!("kill() - EXEC process, exec_id={}", req.exec_id);

    let state_dir = runtime_state_dir();
    let exec_path = format!("{}/{}/exec-{}.json", state_dir, req.id, req.exec_id);

    if let Ok(data) = std::fs::read_to_string(&exec_path) {
        if let Ok(state) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(pid) = state["pid"].as_i64() {
                if pid > 0 {
                    let sig = nix::sys::signal::Signal::try_from(req.signal as i32)
                        .unwrap_or(nix::sys::signal::Signal::SIGTERM);
                    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), sig) {
                        Ok(()) => info!("kill() exec - signal sent to pid {}", pid),
                        Err(nix::errno::Errno::ESRCH) => info!("kill() exec - pid {} already exited", pid),
                        Err(e) => tracing::error!("kill() exec failed: {}", e),
                    }
                }
            }
        }
    }

    return Ok(api::Empty::new());
}
```

#### 4g. Update `delete()` to handle exec_id:

Add after the sandbox check:

```rust
// Handle exec delete
if !req.exec_id.is_empty() {
    info!("delete() - EXEC process, exec_id={}", req.exec_id);

    let state_dir = runtime_state_dir();
    let exec_path = format!("{}/{}/exec-{}.json", state_dir, req.id, req.exec_id);
    let _ = std::fs::remove_file(&exec_path);

    return Ok(api::DeleteResponse { pid: 0, exit_status: 0, ..Default::default() });
}
```

#### 4h. Update `state()` to handle exec_id:

Add after the sandbox check:

```rust
// Handle exec state
if !req.exec_id.is_empty() {
    info!("state() - EXEC process, exec_id={}", req.exec_id);

    let state_dir = runtime_state_dir();
    let exec_path = format!("{}/{}/exec-{}.json", state_dir, req.id, req.exec_id);

    if let Ok(data) = std::fs::read_to_string(&exec_path) {
        if let Ok(state) = serde_json::from_str::<serde_json::Value>(&data) {
            let mut resp = api::StateResponse::new();
            resp.id = req.exec_id.clone();
            resp.pid = state["pid"].as_u64().unwrap_or(0) as u32;
            let status_str = state["status"].as_str().unwrap_or("unknown");
            resp.status = match status_str {
                "created" => ::protobuf::EnumOrUnknown::new(api::Status::CREATED),
                "running" => ::protobuf::EnumOrUnknown::new(api::Status::RUNNING),
                "stopped" => ::protobuf::EnumOrUnknown::new(api::Status::STOPPED),
                _ => ::protobuf::EnumOrUnknown::new(api::Status::UNKNOWN),
            };
            if status_str == "stopped" {
                resp.exit_status = state["exit_code"].as_u64().unwrap_or(0) as u32;
            }
            return Ok(resp);
        }
    }

    return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
        ttrpc::Code::NOT_FOUND, format!("exec {} not found", req.exec_id),
    )));
}
```

#### 4i. Update `resize_pty()` to return Ok instead of error:

```rust
async fn resize_pty(&self, _ctx: &TtrpcContext, req: api::ResizePtyRequest) -> TtrpcResult<api::Empty> {
    info!("resize_pty() called - container_id={}, exec_id={}, width={}, height={}",
        req.id, req.exec_id, req.width, req.height);
    // TODO: Propagate window size to PTY master (requires IPC with runtime daemon)
    // For now, return success - terminal works but won't resize dynamically
    Ok(api::Empty::new())
}
```

### Step 5: Add Integration Tests

Create tests in `tests/integration_exec.rs`:

1. **test_exec_state_lifecycle** - Test ExecState save/load/delete via runtime binary
2. **test_exec_non_terminal** - Run a container with `sleep 60`, exec `echo hello` (non-terminal), verify output and exit code
3. **test_exec_terminal** - Run a container with `sleep 60`, exec with terminal mode, verify PTY is allocated
4. **test_exec_kill** - Exec a long-running process, kill it, verify it stops

Key test pattern:
```rust
// 1. Create container with "sleep 60" as the main process
// 2. Start container (forks daemon running sleep)
// 3. Write exec state file manually
// 4. Call reaper-runtime exec <id> --exec-id <eid>
// 5. Verify exec process ran and exec state updated
// 6. Clean up
```

Note: Full exec integration tests require the shim. Unit tests can test the runtime exec command directly.

### Step 6: Update kind-integration.sh

Add an exec test section:

```bash
echo "--- Testing exec support ---"
# Create a long-running pod
kubectl apply -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: reaper-exec-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["sleep", "60"]
EOF

# Wait for pod to be running
kubectl wait --for=condition=Ready pod/reaper-exec-test --timeout=30s

# Exec into the pod
EXEC_OUTPUT=$(kubectl exec reaper-exec-test -- echo "exec works")
if [ "$EXEC_OUTPUT" = "exec works" ]; then
  echo "PASS: kubectl exec works"
else
  echo "FAIL: kubectl exec output: $EXEC_OUTPUT"
  exit 1
fi

# Clean up
kubectl delete pod reaper-exec-test --force --grace-period=0
```

### Step 7: Build, Test, Commit

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test
# Push to overlay-namespace branch
```

## Key Design Decisions

1. **Shim writes exec state file directly** rather than calling a runtime command. This is simpler than adding an `exec-prepare` subcommand.

2. **Runtime reads exec state file and does the heavy lifting** (fork, overlay join, PTY creation, process spawn, I/O relay).

3. **PTY relay uses std::thread** rather than async. The relay runs in the forked daemon process, which is synchronous.

4. **resize_pty is a no-op** for the initial implementation. The terminal works with its default size (80x24). Dynamic resize would require IPC between shim and runtime daemon.

5. **Same overlay namespace** - exec processes join the same shared overlay namespace as the main container, so they see the same filesystem.

6. **1-hour timeout for exec wait** - Interactive sessions can run for a long time. The shim polls the exec state file with a 1-hour timeout.

## Potential Issues

1. **FIFO open blocking** - If containerd hasn't opened its end of the FIFOs when the runtime daemon tries to open them, the open() call will block. This should resolve quickly as containerd opens FIFOs before/around the time it calls start().

2. **PTY master File ownership** - Converting `OwnedFd` from `openpty()` to `File` requires careful ownership management. Use `File::from(pty.master.into())` to transfer ownership cleanly.

3. **macOS vs Linux ioctl** - `TIOCSCTTY` has different values on macOS vs Linux but `libc::TIOCSCTTY` handles this. The second argument (0) works on both.

4. **nix 0.28 "term" feature** - The feature is called "term" not "pty". Already fixed in Cargo.toml.
