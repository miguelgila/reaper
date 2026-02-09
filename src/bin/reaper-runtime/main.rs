use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod state;
use state::{
    delete as delete_state, load_exec_state, load_pid, load_state, save_exec_state, save_pid,
    save_state, ContainerState,
};

#[cfg(target_os = "linux")]
mod overlay;

#[derive(Parser, Debug)]
#[command(
    name = "reaper-runtime",
    version,
    about = "Minimal OCI-compatible runtime; runc-v2 shim compatible"
)]
struct Cli {
    /// Bundle directory containing config.json (runc: --bundle)
    #[arg(global = true, long, value_name = "PATH")]
    bundle: Option<PathBuf>,

    /// Root directory for runtime state (runc: --root)
    #[arg(global = true, long, value_name = "PATH")]
    root: Option<PathBuf>,

    /// Path to write runtime logs (runc: --log)
    #[arg(global = true, long, value_name = "PATH")]
    log: Option<PathBuf>,

    /// Log format (json|text) (runc: --log-format)
    #[arg(global = true, long = "log-format")]
    log_format: Option<String>,

    /// PID file path (runc: --pid-file)
    #[arg(global = true, long, value_name = "PATH")]
    pid_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a container (record metadata)
    Create {
        /// Container ID
        id: String,
        /// Allocate a PTY (interactive terminal)
        #[arg(long)]
        terminal: bool,
        /// stdin FIFO path (from containerd)
        #[arg(long, value_name = "PATH")]
        stdin: Option<String>,
        /// stdout FIFO path (from containerd)
        #[arg(long, value_name = "PATH")]
        stdout: Option<String>,
        /// stderr FIFO path (from containerd)
        #[arg(long, value_name = "PATH")]
        stderr: Option<String>,
    },
    /// Start the container process
    Start {
        /// Container ID
        id: String,
    },
    /// Print container state as JSON
    State {
        /// Container ID
        id: String,
    },
    /// Send a signal to the container process
    Kill {
        /// Container ID
        id: String,
        /// Signal number (default: 15 = SIGTERM)
        signal: Option<i32>,
    },
    /// Delete container state
    Delete {
        /// Container ID
        id: String,
        #[arg(short, long)]
        force: bool,
    },
    /// Execute a process inside a running container
    Exec {
        /// Container ID
        id: String,
        /// Exec process ID
        #[arg(long)]
        exec_id: String,
    },
}

#[derive(Debug, Deserialize)]
struct OciUser {
    uid: u32,
    gid: u32,
    #[serde(default, alias = "additionalGids")]
    additional_gids: Vec<u32>,
    umask: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OciProcess {
    args: Option<Vec<String>>, // command and args
    env: Option<Vec<String>>,  // key=value
    cwd: Option<String>,
    user: Option<OciUser>,
    // #[serde(default)]
    // terminal: bool,
}

#[derive(Debug, serde::Deserialize)]
struct OciConfig {
    process: Option<OciProcess>,
    // Note: We only need the process section; root and other fields are ignored
}

fn read_oci_config(bundle: &Path) -> Result<OciConfig> {
    info!("read_oci_config() called - bundle={}", bundle.display());
    let path = bundle.join("config.json");
    let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: OciConfig = serde_json::from_slice(&data).context("parsing config.json")?;
    info!(
        "read_oci_config() succeeded - found {} process args",
        cfg.process
            .as_ref()
            .and_then(|p| p.args.as_ref())
            .map(|a| a.len())
            .unwrap_or(0)
    );
    Ok(cfg)
}

/// Open a FIFO for writing. FIFOs are created by containerd and we open them for writing.
/// Uses O_RDWR so the open succeeds even if the reader (containerd) hasn't connected yet —
/// O_WRONLY|O_NONBLOCK returns ENXIO on Linux when no reader exists.
/// Also uses O_NONBLOCK during open to prevent blocking, then clears it so writes block normally.
fn open_log_file(path: &str) -> Result<std::fs::File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("Failed to open log FIFO at {}", path))?;

    // Clear O_NONBLOCK so writes block normally (backpressure from reader)
    use std::os::unix::io::AsRawFd;
    unsafe {
        let flags = nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFL);
        if flags >= 0 {
            nix::libc::fcntl(
                file.as_raw_fd(),
                nix::libc::F_SETFL,
                flags & !nix::libc::O_NONBLOCK,
            );
        }
    }

    Ok(file)
}

fn do_create(
    id: &str,
    bundle: &Path,
    terminal: bool,
    stdin: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
) -> Result<()> {
    info!(
        "do_create() called - id={}, bundle={}, terminal={}, stdin={:?}, stdout={:?}, stderr={:?}",
        id,
        bundle.display(),
        terminal,
        stdin,
        stdout,
        stderr
    );
    let mut state = ContainerState::new(id.to_string(), bundle.to_path_buf());
    state.terminal = terminal;
    state.stdin = stdin;
    state.stdout = stdout;
    state.stderr = stderr;
    save_state(&state)?;
    info!("do_create() succeeded - state saved for container={}", id);
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

fn do_start(id: &str, bundle: &Path) -> Result<()> {
    info!("do_start() called - id={}, bundle={}", id, bundle.display());

    // Load state to get the original bundle path (in case bundle arg is just ".")
    let state = load_state(id)?;
    let bundle = &state.bundle;
    info!("do_start() - using bundle from state: {}", bundle.display());

    let cfg = read_oci_config(bundle)?;
    let proc = cfg
        .process
        .context("config.json missing 'process' section")?;
    let args = proc.args.unwrap_or_default();
    if args.is_empty() {
        bail!("process.args must contain at least one element (program)");
    }
    let program = args[0].clone();
    let argv: Vec<String> = args[1..].to_vec();

    // Reaper runs HOST binaries, not container binaries
    // Both absolute and relative paths are used directly - the shell/OS resolves them
    let program_path = PathBuf::from(&program);

    info!(
        "do_start() - program={}, resolved_path={}, args={:?}, cwd={:?}",
        program,
        program_path.display(),
        argv,
        proc.cwd
    );

    // Handle user/group ID switching before exec
    let user_config = proc.user;
    if let Some(ref user) = user_config {
        info!(
            "do_start() - user config: uid={}, gid={}, additional_gids={:?}, umask={:?}",
            user.uid, user.gid, user.additional_gids, user.umask
        );
    } else {
        info!("do_start() - no user config, will run as current user");
    }

    // Clone data needed for the forked child
    let container_id = id.to_string();
    let cwd = proc.cwd.clone();
    let env_vars = proc.env.clone();

    use nix::unistd::{fork, ForkResult};

    // CRITICAL: Fork FIRST, then spawn the workload in the forked child.
    // This ensures the monitoring daemon is the actual parent of the workload,
    // allowing it to properly call wait() and reap the child.
    //
    // Previous bug: We were spawning the workload first, then forking.
    // After fork(), the std::process::Child handle is invalid in the forked child
    // because it was created by the parent process.

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child: daemon_pid }) => {
            // Parent process (original reaper-runtime start command)
            // We don't know the workload PID yet - the daemon will report it
            // For now, just report the daemon PID and let the daemon update state
            info!(
                "do_start() - forked monitoring daemon (pid={}), parent returning",
                daemon_pid
            );

            // Wait for daemon to spawn workload and update state
            // The daemon needs time to: setsid(), spawn workload, write state to disk
            // Poll the state file until we see the workload PID
            let mut workload_pid = None;
            for _attempt in 0..20 {
                // Try up to 20 times (2 seconds total)
                if let Ok(state) = load_state(&container_id) {
                    if let Some(pid) = state.pid {
                        workload_pid = Some(pid);
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            // Print the workload PID if we got it, otherwise fall back to daemon PID
            if let Some(pid) = workload_pid {
                println!("started pid={}", pid);
            } else {
                // Fallback: report daemon PID if workload PID not yet available after 2s
                info!(
                    "do_start() - timeout waiting for workload PID, reporting daemon PID instead"
                );
                println!("started pid={}", daemon_pid);
            }

            // Attempt to reap daemon if it has already exited (non-blocking).
            // WNOHANG means don't block if still running. This prevents zombie processes.
            use nix::sys::wait::{waitpid, WaitPidFlag};
            use nix::unistd::Pid;
            let daemon_pid_raw = Pid::from_raw(daemon_pid.as_raw());
            let _ = waitpid(daemon_pid_raw, Some(WaitPidFlag::WNOHANG));

            Ok(())
        }
        Ok(ForkResult::Child) => {
            // Child process (monitoring daemon)
            // This process will spawn and monitor the workload

            // CRITICAL: Close inherited stdout/stderr immediately after fork.
            // The shim calls reaper-runtime via cmd.output() which creates pipes.
            // If we keep those pipe fds open, cmd.output() blocks until WE exit,
            // which prevents the shim's start() from returning and leaves the pod
            // stuck in ContainerCreating. Redirecting to /dev/null lets the parent's
            // pipes close when the parent exits.
            {
                use std::os::unix::io::AsRawFd;
                if let Ok(devnull) = std::fs::File::open("/dev/null") {
                    let fd = devnull.as_raw_fd();
                    unsafe {
                        nix::libc::dup2(fd, 1); // stdout
                        nix::libc::dup2(fd, 2); // stderr
                    }
                }
            }

            // Detach from parent session to become a proper daemon
            if let Err(e) = nix::unistd::setsid() {
                // Can't use eprintln! here since stderr is now /dev/null
                let _ = e;
            }

            // Join shared overlay namespace (Linux only).
            // Overlay is mandatory — workloads must not run on the host filesystem.
            #[cfg(target_os = "linux")]
            {
                let overlay_config = overlay::read_config();
                if let Err(e) = overlay::enter_overlay(&overlay_config) {
                    tracing::error!(
                        "do_start() - overlay setup failed: {:#}, refusing to run without isolation",
                        e
                    );
                    if let Ok(mut state) = load_state(&container_id) {
                        state.status = "stopped".into();
                        state.exit_code = Some(1);
                        let _ = save_state(&state);
                    }
                    std::process::exit(1);
                }
                info!("do_start() - joined shared overlay namespace");
            }

            // Now spawn the workload - we are its parent!
            // Reload state to get I/O paths and terminal flag from create
            let io_state = load_state(&container_id).ok();
            let use_terminal = io_state.as_ref().map_or(false, |s| s.terminal);

            if use_terminal {
                // Terminal mode: allocate a PTY so the shell sees isatty()=true.
                // Relay between containerd FIFOs and the PTY master.
                use nix::pty::openpty;
                use std::io::{Read, Write};
                use std::os::unix::io::AsRawFd;

                let pty = match openpty(None, None) {
                    Ok(pty) => pty,
                    Err(e) => {
                        tracing::error!("do_start() - openpty failed: {}", e);
                        if let Ok(mut state) = load_state(&container_id) {
                            state.status = "stopped".into();
                            state.exit_code = Some(1);
                            let _ = save_state(&state);
                        }
                        std::process::exit(1);
                    }
                };

                let slave_raw_fd = pty.slave.as_raw_fd();

                let mut cmd = Command::new(&program_path);
                cmd.args(&argv);
                if let Some(cwd) = cwd.as_deref() {
                    cmd.current_dir(cwd);
                }
                if let Some(envs) = env_vars.as_ref() {
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
                        // TIOCSCTTY request type is u64 on both Linux and macOS
                        if nix::libc::ioctl(
                            slave_raw_fd,
                            nix::libc::TIOCSCTTY as nix::libc::c_ulong,
                            0 as nix::libc::c_int,
                        ) < 0
                        {
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

                match cmd.spawn() {
                    Ok(mut child) => {
                        let workload_pid = child.id() as i32;

                        if let Ok(mut state) = load_state(&container_id) {
                            state.status = "running".into();
                            state.pid = Some(workload_pid);
                            let _ = save_state(&state);
                            let _ = save_pid(&container_id, workload_pid);
                        }

                        // Close slave in parent - child has it via dup2
                        drop(pty.slave);

                        // Convert PTY master OwnedFd to File for I/O
                        let master_file: std::fs::File = pty.master.into();
                        let master_clone = master_file.try_clone().unwrap_or_else(|e| {
                            tracing::error!("failed to clone master fd: {}", e);
                            std::process::exit(1);
                        });

                        // Relay: stdin FIFO → PTY master (user input to process)
                        if let Some(ref state) = io_state {
                            if let Some(ref stdin_path) = state.stdin {
                                if !stdin_path.is_empty() {
                                    let stdin_path = stdin_path.clone();
                                    let mut master_w = master_clone;
                                    std::thread::spawn(move || {
                                        if let Ok(mut stdin_file) = std::fs::File::open(&stdin_path)
                                        {
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
                        }

                        // Relay: PTY master → stdout FIFO (process output to user)
                        if let Some(ref state) = io_state {
                            if let Some(ref stdout_path) = state.stdout {
                                if !stdout_path.is_empty() {
                                    let stdout_path = stdout_path.clone();
                                    let mut master_r = master_file;
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
                                                        if stdout_file.write_all(&buf[..n]).is_err()
                                                        {
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
                        }

                        std::thread::sleep(std::time::Duration::from_millis(500));

                        match child.wait() {
                            Ok(exit_status) => {
                                let exit_code = exit_status.code().unwrap_or(1);
                                if let Ok(mut state) = load_state(&container_id) {
                                    state.status = "stopped".into();
                                    state.exit_code = Some(exit_code);
                                    let _ = save_state(&state);
                                }
                            }
                            Err(_e) => {
                                if let Ok(mut state) = load_state(&container_id) {
                                    state.status = "stopped".into();
                                    state.exit_code = Some(1);
                                    let _ = save_state(&state);
                                }
                            }
                        }
                    }
                    Err(_e) => {
                        if let Ok(mut state) = load_state(&container_id) {
                            state.status = "stopped".into();
                            state.exit_code = Some(1);
                            let _ = save_state(&state);
                        }
                    }
                }
            } else {
                // Non-terminal mode: connect FIFOs directly to the process
                let mut cmd = Command::new(&program_path);
                cmd.args(&argv);
                if let Some(cwd) = cwd.as_deref() {
                    cmd.current_dir(cwd);
                }
                if let Some(envs) = env_vars.as_ref() {
                    for kv in envs {
                        if let Some((k, v)) = kv.split_once('=') {
                            cmd.env(k, v);
                        }
                    }
                }

                // Configure stdin: use FIFO if available, otherwise null.
                // Open with O_RDWR so the FIFO always has a writer — prevents EOF
                // when the real writer (containerd/kubectl attach) hasn't connected yet.
                // Without O_RDWR, the shell reads EOF immediately and exits.
                if let Some(ref state) = io_state {
                    if let Some(ref stdin_path) = state.stdin {
                        if !stdin_path.is_empty() {
                            use std::fs::OpenOptions;
                            use std::os::unix::fs::OpenOptionsExt;
                            match OpenOptions::new()
                                .read(true)
                                .write(true)
                                .custom_flags(nix::libc::O_NONBLOCK)
                                .open(stdin_path)
                            {
                                Ok(file) => {
                                    // Clear O_NONBLOCK after open so reads block normally
                                    use std::os::unix::io::AsRawFd;
                                    unsafe {
                                        let flags =
                                            nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFL);
                                        nix::libc::fcntl(
                                            file.as_raw_fd(),
                                            nix::libc::F_SETFL,
                                            flags & !nix::libc::O_NONBLOCK,
                                        );
                                    }
                                    cmd.stdin(Stdio::from(file));
                                    info!("do_start() - connected stdin from FIFO: {}", stdin_path);
                                }
                                Err(e) => {
                                    info!(
                                    "do_start() - failed to open stdin FIFO ({}), using null: {}",
                                    stdin_path, e
                                );
                                    cmd.stdin(Stdio::null());
                                }
                            }
                        } else {
                            cmd.stdin(Stdio::null());
                        }
                    } else {
                        cmd.stdin(Stdio::null());
                    }
                } else {
                    cmd.stdin(Stdio::null());
                }

                // Configure stdout: use FIFO if available, otherwise inherit
                if let Some(ref state) = io_state {
                    if let Some(ref stdout_path) = state.stdout {
                        if !stdout_path.is_empty() {
                            match open_log_file(stdout_path) {
                                Ok(file) => {
                                    cmd.stdout(Stdio::from(file));
                                    info!(
                                        "do_start() - redirected stdout to FIFO: {}",
                                        stdout_path
                                    );
                                }
                                Err(e) => {
                                    info!("do_start() - failed to open stdout FIFO ({}), falling back to inherit: {}", stdout_path, e);
                                    cmd.stdout(Stdio::inherit());
                                }
                            }
                        } else {
                            cmd.stdout(Stdio::inherit());
                        }
                    } else {
                        cmd.stdout(Stdio::inherit());
                    }
                } else {
                    cmd.stdout(Stdio::inherit());
                }

                // Configure stderr: use FIFO if available, otherwise inherit
                if let Some(ref state) = io_state {
                    if let Some(ref stderr_path) = state.stderr {
                        if !stderr_path.is_empty() {
                            match open_log_file(stderr_path) {
                                Ok(file) => {
                                    cmd.stderr(Stdio::from(file));
                                    info!(
                                        "do_start() - redirected stderr to FIFO: {}",
                                        stderr_path
                                    );
                                }
                                Err(e) => {
                                    info!("do_start() - failed to open stderr FIFO ({}), falling back to inherit: {}", stderr_path, e);
                                    cmd.stderr(Stdio::inherit());
                                }
                            }
                        } else {
                            cmd.stderr(Stdio::inherit());
                        }
                    } else {
                        cmd.stderr(Stdio::inherit());
                    }
                } else {
                    cmd.stderr(Stdio::inherit());
                }

                match cmd.spawn() {
                    Ok(mut child) => {
                        let workload_pid = child.id() as i32;

                        // Update state to running with the actual workload PID
                        if let Ok(mut state) = load_state(&container_id) {
                            state.status = "running".into();
                            state.pid = Some(workload_pid);
                            let _ = save_state(&state);
                            let _ = save_pid(&container_id, workload_pid);
                        }

                        // IMPORTANT: Give containerd/kubelet time to observe the "running"
                        // state before the container potentially exits.
                        std::thread::sleep(std::time::Duration::from_millis(500));

                        // Wait for the workload process to exit
                        // We are the parent, so this will work correctly!
                        match child.wait() {
                            Ok(exit_status) => {
                                let exit_code = exit_status.code().unwrap_or(1);
                                if let Ok(mut state) = load_state(&container_id) {
                                    state.status = "stopped".into();
                                    state.exit_code = Some(exit_code);
                                    let _ = save_state(&state);
                                }
                            }
                            Err(_e) => {
                                if let Ok(mut state) = load_state(&container_id) {
                                    state.status = "stopped".into();
                                    state.exit_code = Some(1);
                                    let _ = save_state(&state);
                                }
                            }
                        }
                    }
                    Err(_e) => {
                        if let Ok(mut state) = load_state(&container_id) {
                            state.status = "stopped".into();
                            state.exit_code = Some(1);
                            let _ = save_state(&state);
                        }
                    }
                }
            }

            // Daemon exits after workload completes
            std::process::exit(0);
        }
        Err(e) => {
            bail!("Failed to fork monitoring daemon: {}", e);
        }
    }
}

fn do_state(id: &str) -> Result<()> {
    info!("do_state() called - id={}", id);
    let state = load_state(id)?;
    info!(
        "do_state() succeeded - id={}, status={}, pid={:?}",
        id, state.status, state.pid
    );
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

fn do_kill(id: &str, signal: Option<i32>) -> Result<()> {
    let signal = signal.unwrap_or(15); // Default to SIGTERM
    info!("do_kill() called - id={}, signal={}", id, signal);
    let pid = load_pid(id)?;
    info!("do_kill() - sending signal {} to pid {}", signal, pid);
    // Use nix to send signal
    let sig = nix::sys::signal::Signal::try_from(signal).context("invalid signal")?;
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), sig) {
        Ok(()) => {
            info!(
                "do_kill() succeeded - id={}, signal={}, pid={}",
                id, signal, pid
            );
        }
        Err(nix::errno::Errno::ESRCH) => {
            // Process doesn't exist - this is expected if the container already exited
            // Return success since the goal (container not running) is achieved
            info!(
                "do_kill() - process {} already exited (ESRCH), treating as success",
                pid
            );
        }
        Err(e) => {
            bail!("failed to send signal: {}", e);
        }
    }
    Ok(())
}

fn do_delete(id: &str) -> Result<()> {
    info!("do_delete() called - id={}", id);
    delete_state(id)?;
    info!("do_delete() succeeded - id={}", id);
    println!("deleted {}", id);
    Ok(())
}

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
            // Use Ioctl type alias which is u64 on both Linux and macOS
            if nix::libc::ioctl(
                slave_raw_fd,
                nix::libc::TIOCSCTTY as nix::libc::c_ulong,
                0 as nix::libc::c_int,
            ) < 0
            {
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
                if let Ok(mut stdout_file) =
                    std::fs::OpenOptions::new().write(true).open(&stdout_path)
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

fn do_exec(container_id: &str, exec_id: &str) -> Result<()> {
    info!(
        "do_exec() called - container_id={}, exec_id={}",
        container_id, exec_id
    );

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
            // Close inherited stdout/stderr to unblock the shim's cmd.output()
            // (same reason as in do_start - see comment there)
            {
                use std::os::unix::io::AsRawFd;
                if let Ok(devnull) = std::fs::File::open("/dev/null") {
                    let fd = devnull.as_raw_fd();
                    unsafe {
                        nix::libc::dup2(fd, 1);
                        nix::libc::dup2(fd, 2);
                    }
                }
            }

            if let Err(e) = nix::unistd::setsid() {
                let _ = e;
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
                exec_with_pty(
                    &program,
                    &argv,
                    cwd,
                    env_vars,
                    stdin_path,
                    stdout_path,
                    &container_id,
                    &exec_id,
                )
            } else {
                exec_without_pty(
                    &program,
                    &argv,
                    cwd,
                    env_vars,
                    stdin_path,
                    stdout_path,
                    stderr_path,
                    &container_id,
                    &exec_id,
                )
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

fn main() -> Result<()> {
    // Setup tracing similar to shim: use REAPER_RUNTIME_LOG env var
    // If not set, use null writer to prevent stdout pollution
    if let Ok(log_path) = std::env::var("REAPER_RUNTIME_LOG") {
        // If REAPER_RUNTIME_LOG is set, log to that file
        if let Ok(log_file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_ansi(false) // No color codes in log files
                .with_writer(std::sync::Mutex::new(log_file))
                .init();

            info!("===== Reaper Runtime Starting =====");
            info!("Log file: {}", log_path);
        } else {
            // Failed to open log file - use null writer to discard logs safely
            let null_writer = std::io::sink();
            tracing_subscriber::fmt()
                .with_writer(std::sync::Mutex::new(null_writer))
                .with_ansi(false)
                .init();
        }
    } else {
        // REAPER_RUNTIME_LOG not set - use null writer to discard all logs safely
        let null_writer = std::io::sink();
        tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(null_writer))
            .with_ansi(false)
            .init();
    }

    let cli = Cli::parse();
    info!(
        "CLI parsed: bundle={:?}, root={:?}, command={:?}",
        cli.bundle, cli.root, cli.command
    );

    // Default bundle to current directory if not specified
    let bundle = cli.bundle.as_deref().unwrap_or_else(|| Path::new("."));

    let result = match cli.command {
        Commands::Create {
            ref id,
            terminal,
            stdin,
            stdout,
            stderr,
        } => do_create(id, bundle, terminal, stdin, stdout, stderr),
        Commands::Start { ref id } => do_start(id, bundle),
        Commands::State { ref id } => do_state(id),
        Commands::Kill { ref id, signal } => do_kill(id, signal),
        Commands::Delete { ref id, .. } => do_delete(id),
        Commands::Exec {
            ref id,
            ref exec_id,
        } => do_exec(id, exec_id),
    };

    if let Err(ref e) = result {
        tracing::error!("Command failed: {:?}", e);
    } else {
        info!("Command completed successfully");
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_config_without_user() {
        let bundle_dir = TempDir::new().expect("Failed to create temp dir");
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/echo", "hello"],
                "cwd": "/tmp"
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        assert!(parsed.process.is_some());
        let process = parsed.process.unwrap();
        assert!(process.user.is_none());
        assert_eq!(process.args.unwrap(), vec!["/bin/echo", "hello"]);
    }

    #[test]
    fn test_parse_config_with_basic_user() {
        let bundle_dir = TempDir::new().expect("Failed to create temp dir");
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/echo", "hello"],
                "cwd": "/tmp",
                "user": {
                    "uid": 1000,
                    "gid": 1000
                }
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        let process = parsed.process.unwrap();
        let user = process.user.unwrap();
        assert_eq!(user.uid, 1000);
        assert_eq!(user.gid, 1000);
        assert_eq!(user.additional_gids, Vec::<u32>::new());
        assert_eq!(user.umask, None);
    }

    #[test]
    fn test_parse_config_with_full_user() {
        let bundle_dir = TempDir::new().expect("Failed to create temp dir");
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/sh", "-c", "id"],
                "cwd": "/tmp",
                "user": {
                    "uid": 1001,
                    "gid": 1001,
                    "additionalGids": [10, 20, 30],
                    "umask": 22
                }
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        let process = parsed.process.unwrap();
        let user = process.user.unwrap();
        assert_eq!(user.uid, 1001);
        assert_eq!(user.gid, 1001);
        assert_eq!(user.additional_gids, vec![10, 20, 30]);
        assert_eq!(user.umask, Some(22));
    }

    #[test]
    fn test_parse_config_with_root_user() {
        let bundle_dir = TempDir::new().expect("Failed to create temp dir");
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/cat", "/etc/shadow"],
                "cwd": "/",
                "user": {
                    "uid": 0,
                    "gid": 0
                }
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        let process = parsed.process.unwrap();
        let user = process.user.unwrap();
        assert_eq!(user.uid, 0);
        assert_eq!(user.gid, 0);
    }

    #[test]
    fn test_parse_config_missing_user_fields() {
        // Test that additional_gids defaults to empty vec if not provided
        let bundle_dir = TempDir::new().expect("Failed to create temp dir");
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/true"],
                "user": {
                    "uid": 500,
                    "gid": 500
                    // No additionalGids or umask
                }
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        let user = parsed.process.unwrap().user.unwrap();
        assert!(user.additional_gids.is_empty());
        assert!(user.umask.is_none());
    }
}
