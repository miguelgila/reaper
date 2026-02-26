use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::fs;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod state;
use state::{
    delete as delete_state, load_exec_state, load_pid, load_state, save_exec_state, save_pid,
    save_state, ContainerState, OciUser,
};

#[cfg(target_os = "linux")]
mod overlay;

#[path = "../../config.rs"]
mod config;

fn version_string() -> &'static str {
    const VERSION: &str = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("GIT_HASH"),
        " ",
        env!("BUILD_DATE"),
        ")"
    );
    VERSION
}

#[derive(Parser, Debug)]
#[command(
    name = "reaper-runtime",
    version = version_string(),
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
struct OciProcess {
    args: Option<Vec<String>>, // command and args
    env: Option<Vec<String>>,  // key=value
    cwd: Option<String>,
    user: Option<OciUser>,
    // #[serde(default)]
    // terminal: bool,
}

/// OCI mount specification from config.json.
/// Containerd populates this array with bind-mount directives for volumes.
#[derive(Debug, Clone, Deserialize)]
pub struct OciMount {
    pub destination: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(rename = "type", default)]
    pub mount_type: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct OciConfig {
    process: Option<OciProcess>,
    #[serde(default)]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    mounts: Vec<OciMount>,
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

/// Platform-specific wrapper for setgroups syscall.
/// Linux uses size_t (usize), macOS/BSD uses c_int (i32).
#[cfg(target_os = "linux")]
unsafe fn safe_setgroups(gids: &[nix::libc::gid_t]) -> std::io::Result<()> {
    if nix::libc::setgroups(gids.len(), gids.as_ptr()) != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
unsafe fn safe_setgroups(gids: &[nix::libc::gid_t]) -> std::io::Result<()> {
    if nix::libc::setgroups(gids.len() as nix::libc::c_int, gids.as_ptr()) != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
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

/// Extract exit code from an ExitStatus, handling signal-killed processes.
///
/// When a process is killed by a signal, `ExitStatus::code()` returns `None`.
/// PTY sessions commonly see SIGHUP (signal 1) during teardown — the kernel
/// sends it when the controlling terminal's slave side closes. This is normal
/// and should be treated as a clean exit (code 0), not an error.
/// Other signals use the standard 128+signal convention.
fn exit_code_from_status(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    // Process was killed by a signal
    match status.signal() {
        Some(nix::libc::SIGHUP) => 0, // PTY teardown — not an error
        Some(sig) => 128 + sig,
        None => 1,
    }
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

/// Extract program path and arguments from an OCI config.
/// Returns (program_path, remaining_argv).
fn parse_program_and_args(cfg: &OciConfig) -> Result<(PathBuf, Vec<String>)> {
    let proc = cfg
        .process
        .as_ref()
        .context("config.json missing 'process' section")?;
    let args = proc.args.as_deref().unwrap_or(&[]);
    if args.is_empty() {
        bail!("process.args must contain at least one element (program)");
    }
    let program = PathBuf::from(&args[0]);
    let argv = args[1..].to_vec();
    Ok((program, argv))
}

fn do_start(id: &str, bundle: &Path) -> Result<()> {
    info!("do_start() called - id={}, bundle={}", id, bundle.display());

    // Load state to get the original bundle path (in case bundle arg is just ".")
    let state = load_state(id)?;
    let bundle = &state.bundle;
    info!("do_start() - using bundle from state: {}", bundle.display());

    let cfg = read_oci_config(bundle)?;
    let (program_path, argv) = parse_program_and_args(&cfg)?;
    let program = program_path.to_string_lossy().to_string();
    let proc = cfg
        .process
        .as_ref()
        .context("config.json missing 'process' section")?;

    info!(
        "do_start() - program={}, resolved_path={}, args={:?}, cwd={:?}",
        program,
        program_path.display(),
        argv,
        proc.cwd
    );

    // Handle user/group ID switching before exec
    let user_config = &proc.user;
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
    #[cfg(target_os = "linux")]
    let oci_mounts = cfg.mounts.clone();

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
            // The daemon needs time to: setsid(), [enter overlay namespace on Linux], spawn workload, write state to disk
            // On Linux, overlay namespace setup can take significant time (creating namespace, mounting, pivot_root)
            // Poll the state file until we see the workload PID or the container exits
            let mut workload_pid = None;
            let max_attempts = if cfg!(target_os = "linux") { 100 } else { 20 };
            let poll_interval_ms = 100;

            for attempt in 0..max_attempts {
                if let Ok(state) = load_state(&container_id) {
                    if let Some(pid) = state.pid {
                        workload_pid = Some(pid);
                        break;
                    }
                    // If container is already stopped, daemon failed to start workload
                    if state.status == "stopped" {
                        info!(
                            "do_start() - container stopped before PID was recorded (daemon likely failed), exit_code={:?}",
                            state.exit_code
                        );
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(poll_interval_ms));

                // Log progress every second for debugging
                if attempt > 0 && attempt % 10 == 0 {
                    info!(
                        "do_start() - still waiting for workload PID (attempt {}/{})",
                        attempt, max_attempts
                    );
                }
            }

            // Print the workload PID if we got it, otherwise fall back to daemon PID
            if let Some(pid) = workload_pid {
                println!("started pid={}", pid);
            } else {
                // Fallback: report daemon PID if workload PID not yet available
                info!(
                    "do_start() - timeout waiting for workload PID after {}ms, reporting daemon PID instead",
                    max_attempts * poll_interval_ms
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
            // Overlay is mandatory in production — workloads must not run on the host filesystem.
            // REAPER_NO_OVERLAY=1 disables overlay for unit tests that lack CAP_SYS_ADMIN.
            #[cfg(target_os = "linux")]
            {
                let skip_overlay = std::env::var("REAPER_NO_OVERLAY")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);

                if skip_overlay {
                    info!("do_start() - overlay disabled via REAPER_NO_OVERLAY");
                } else {
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

                    // Apply volume mounts from OCI config (FATAL on failure)
                    if !oci_mounts.is_empty() {
                        if let Err(e) = overlay::apply_volume_mounts(&oci_mounts) {
                            tracing::error!(
                                "do_start() - volume mount failed: {:#}, refusing to start workload",
                                e
                            );
                            if let Ok(mut state) = load_state(&container_id) {
                                state.status = "stopped".into();
                                state.exit_code = Some(1);
                                let _ = save_state(&state);
                            }
                            std::process::exit(1);
                        }
                        info!("do_start() - volume mounts applied");
                    }

                    // Apply Kubernetes DNS if configured (FATAL on failure)
                    let dns_config = overlay::read_dns_config();
                    if dns_config.mode == overlay::DnsMode::Kubernetes {
                        if let Err(e) = overlay::apply_kubernetes_dns(&oci_mounts) {
                            tracing::error!(
                                "do_start() - kubernetes DNS setup failed: {:#}, refusing to start workload",
                                e
                            );
                            if let Ok(mut state) = load_state(&container_id) {
                                state.status = "stopped".into();
                                state.exit_code = Some(1);
                                let _ = save_state(&state);
                            }
                            std::process::exit(1);
                        }
                        info!("do_start() - kubernetes DNS configured");
                    }
                }
            }

            // Now spawn the workload - we are its parent!
            // Reload state to get I/O paths and terminal flag from create
            let io_state = load_state(&container_id).ok();
            let use_terminal = io_state.as_ref().is_some_and(|s| s.terminal);

            // Clone user config for use in pre_exec closures (both PTY and non-PTY modes)
            let user_cfg_for_exec = user_config.clone();

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
                        // TIOCSCTTY: ioctl request type varies by arch (c_int on aarch64, c_ulong on x86_64/macOS)
                        if nix::libc::ioctl(
                            slave_raw_fd,
                            nix::libc::TIOCSCTTY as _,
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

                        // Apply user/group configuration if present
                        if let Some(ref user) = user_cfg_for_exec {
                            // Set supplementary groups first (must be done while privileged)
                            if !user.additional_gids.is_empty() {
                                safe_setgroups(&user.additional_gids)?;
                            }

                            // Set GID before UID (privilege dropping order matters)
                            if nix::libc::setgid(user.gid) != 0 {
                                return Err(std::io::Error::last_os_error());
                            }

                            // Set UID (this must be last - irreversible privilege drop)
                            if nix::libc::setuid(user.uid) != 0 {
                                return Err(std::io::Error::last_os_error());
                            }

                            // Apply umask if specified
                            if let Some(mask) = user.umask {
                                nix::libc::umask(mask as nix::libc::mode_t);
                            }
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

                        // Hold the stdout FIFO write end open so containerd doesn't
                        // see EOF when the relay thread exits. This prevents a race
                        // where containerd tears down streams via stdout-EOF while
                        // also receiving the TaskExit event. O_NONBLOCK because by
                        // now containerd should have the read end open (via attach).
                        let _stdout_holder = {
                            use std::os::unix::fs::OpenOptionsExt;
                            if let Some(ref state) = io_state {
                                state.stdout.as_ref().and_then(|p| {
                                    std::fs::OpenOptions::new()
                                        .write(true)
                                        .custom_flags(nix::libc::O_NONBLOCK)
                                        .open(p)
                                        .ok()
                                })
                            } else {
                                None
                            }
                        };

                        match child.wait() {
                            Ok(exit_status) => {
                                let exit_code = exit_code_from_status(exit_status);
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

                        // Keep the daemon alive briefly so the shim can detect the
                        // stopped state and publish the TaskExit event before we
                        // drop _stdout_holder. This ensures containerd tears down
                        // streams via the orderly TaskExit path, not a racy
                        // stdout-EOF path.
                        std::thread::sleep(std::time::Duration::from_secs(2));
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

                // Always call setsid() so the workload gets its own process group
                // (PGID == workload PID). This is critical for do_kill() which sends
                // signals to -pid (the process group). Without setsid(), the workload
                // inherits the daemon's PGID, so kill(-workload_pid) targets a
                // non-existent process group and the signal never reaches the process.
                {
                    let user_cfg_clone = user_config.clone();
                    unsafe {
                        cmd.pre_exec(move || {
                            if nix::libc::setsid() < 0 {
                                return Err(std::io::Error::last_os_error());
                            }

                            // Apply user/group configuration if present
                            if let Some(ref user) = user_cfg_clone {
                                // Set supplementary groups first (must be done while privileged)
                                if !user.additional_gids.is_empty() {
                                    safe_setgroups(&user.additional_gids)?;
                                }

                                // Set GID before UID (privilege dropping order matters)
                                if nix::libc::setgid(user.gid) != 0 {
                                    return Err(std::io::Error::last_os_error());
                                }

                                // Set UID (this must be last - irreversible privilege drop)
                                if nix::libc::setuid(user.uid) != 0 {
                                    return Err(std::io::Error::last_os_error());
                                }

                                // Apply umask if specified
                                if let Some(mask) = user.umask {
                                    nix::libc::umask(mask as nix::libc::mode_t);
                                }
                            }

                            Ok(())
                        });
                    }
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
                                let exit_code = exit_code_from_status(exit_status);
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
    info!(
        "do_kill() - sending signal {} to process group (pgid={})",
        signal, pid
    );
    // Kill the entire process group (-pid) so children of the workload (e.g. backgrounded
    // processes) are also signalled. The workload calls setsid() in pre_exec, so its PGID
    // equals its PID.
    let sig = nix::sys::signal::Signal::try_from(signal).context("invalid signal")?;
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), sig) {
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

#[allow(clippy::too_many_arguments)]
fn exec_with_pty(
    program: &str,
    argv: &[String],
    cwd: Option<String>,
    env_vars: Option<Vec<String>>,
    stdin_path: Option<String>,
    stdout_path: Option<String>,
    user_config: Option<OciUser>,
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
            // TIOCSCTTY: ioctl request type varies by arch (c_int on aarch64, c_ulong on x86_64/macOS)
            if nix::libc::ioctl(
                slave_raw_fd,
                nix::libc::TIOCSCTTY as _,
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

            // Apply user/group configuration if present
            if let Some(ref user) = user_config {
                // Set supplementary groups first (must be done while privileged)
                if !user.additional_gids.is_empty() {
                    safe_setgroups(&user.additional_gids)?;
                }

                // Set GID before UID (privilege dropping order matters)
                if nix::libc::setgid(user.gid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // Set UID (this must be last - irreversible privilege drop)
                if nix::libc::setuid(user.uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // Apply umask if specified
                if let Some(mask) = user.umask {
                    nix::libc::umask(mask as nix::libc::mode_t);
                }
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
        Ok(status) => exit_code_from_status(status),
        Err(_) => 1,
    }
}

#[allow(clippy::too_many_arguments)]
fn exec_without_pty(
    program: &str,
    argv: &[String],
    cwd: Option<String>,
    env_vars: Option<Vec<String>>,
    stdin_path: Option<String>,
    stdout_path: Option<String>,
    stderr_path: Option<String>,
    user_config: Option<OciUser>,
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

    // Always call setsid() so the exec process gets its own process group
    // (PGID == PID), matching what the shim's kill() expects when sending
    // signals to -pid.
    {
        let user_cfg_clone = user_config;
        unsafe {
            cmd.pre_exec(move || {
                if nix::libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // Apply user/group configuration if present
                if let Some(ref user) = user_cfg_clone {
                    // Set supplementary groups first (must be done while privileged)
                    if !user.additional_gids.is_empty() {
                        safe_setgroups(&user.additional_gids)?;
                    }

                    // Set GID before UID (privilege dropping order matters)
                    if nix::libc::setgid(user.gid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }

                    // Set UID (this must be last - irreversible privilege drop)
                    if nix::libc::setuid(user.uid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }

                    // Apply umask if specified
                    if let Some(mask) = user.umask {
                        nix::libc::umask(mask as nix::libc::mode_t);
                    }
                }

                Ok(())
            });
        }
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
        Ok(status) => exit_code_from_status(status),
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
    let user_cfg = exec_state.user.clone();

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
                    user_cfg,
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
                    user_cfg,
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
    // Load config file before anything else (env vars override file values)
    config::load_config();

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
    use serial_test::serial;
    use tempfile::TempDir;

    fn setup_test_root() -> TempDir {
        tempfile::tempdir().expect("Failed to create temp dir")
    }

    fn with_test_root<F>(f: F)
    where
        F: FnOnce(String),
    {
        let temp = setup_test_root();
        let root = temp.path().to_string_lossy().to_string();
        std::env::set_var("REAPER_RUNTIME_ROOT", &root);
        f(root);
        std::env::remove_var("REAPER_RUNTIME_ROOT");
    }

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

    // --- read_oci_config edge cases ---

    #[test]
    fn test_read_oci_config_missing_file() {
        let bundle_dir = TempDir::new().unwrap();
        // No config.json created
        let result = read_oci_config(bundle_dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_read_oci_config_invalid_json() {
        let bundle_dir = TempDir::new().unwrap();
        fs::write(bundle_dir.path().join("config.json"), "not valid json").unwrap();
        let result = read_oci_config(bundle_dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_read_oci_config_no_process() {
        let bundle_dir = TempDir::new().unwrap();
        let config = serde_json::json!({});
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        assert!(parsed.process.is_none());
    }

    #[test]
    fn test_read_oci_config_empty_args() {
        let bundle_dir = TempDir::new().unwrap();
        let config = serde_json::json!({
            "process": {
                "args": [],
                "cwd": "/"
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        let process = parsed.process.unwrap();
        assert!(process.args.unwrap().is_empty());
    }

    #[test]
    fn test_read_oci_config_with_env() {
        let bundle_dir = TempDir::new().unwrap();
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/sh"],
                "env": ["PATH=/usr/bin", "HOME=/root"]
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        let env = parsed.process.unwrap().env.unwrap();
        assert_eq!(env.len(), 2);
        assert_eq!(env[0], "PATH=/usr/bin");
    }

    #[test]
    fn test_parse_config_with_mounts() {
        let bundle_dir = TempDir::new().expect("Failed to create temp dir");
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/sh", "-c", "ls /scripts"],
                "cwd": "/"
            },
            "mounts": [
                {
                    "destination": "/proc",
                    "type": "proc",
                    "source": "proc",
                    "options": ["nosuid", "noexec", "nodev"]
                },
                {
                    "destination": "/scripts",
                    "type": "bind",
                    "source": "/var/lib/kubelet/pods/abc/volumes/kubernetes.io~configmap/scripts",
                    "options": ["rbind", "ro"]
                },
                {
                    "destination": "/data",
                    "source": "/host/data",
                    "options": ["rbind"]
                }
            ]
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        assert_eq!(parsed.mounts.len(), 3);

        assert_eq!(parsed.mounts[0].destination, "/proc");
        assert_eq!(parsed.mounts[0].mount_type, Some("proc".to_string()));

        assert_eq!(parsed.mounts[1].destination, "/scripts");
        assert_eq!(parsed.mounts[1].mount_type, Some("bind".to_string()));
        assert_eq!(
            parsed.mounts[1].source,
            Some("/var/lib/kubelet/pods/abc/volumes/kubernetes.io~configmap/scripts".to_string())
        );
        assert_eq!(parsed.mounts[1].options, vec!["rbind", "ro"]);

        assert_eq!(parsed.mounts[2].destination, "/data");
        assert!(parsed.mounts[2].mount_type.is_none());
        assert_eq!(parsed.mounts[2].options, vec!["rbind"]);
    }

    #[test]
    fn test_parse_config_without_mounts() {
        let bundle_dir = TempDir::new().expect("Failed to create temp dir");
        let config = serde_json::json!({
            "process": {
                "args": ["/bin/echo", "hello"]
            }
        });
        fs::write(
            bundle_dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        let parsed = read_oci_config(bundle_dir.path()).unwrap();
        assert!(parsed.mounts.is_empty());
    }

    // --- parse_program_and_args tests ---

    #[test]
    fn test_parse_program_and_args_valid() {
        let cfg = OciConfig {
            process: Some(OciProcess {
                args: Some(vec!["/bin/echo".into(), "hello".into(), "world".into()]),
                env: None,
                cwd: None,
                user: None,
            }),
            mounts: vec![],
        };
        let (program, argv) = parse_program_and_args(&cfg).unwrap();
        assert_eq!(program, PathBuf::from("/bin/echo"));
        assert_eq!(argv, vec!["hello", "world"]);
    }

    #[test]
    fn test_parse_program_and_args_single_element() {
        let cfg = OciConfig {
            process: Some(OciProcess {
                args: Some(vec!["/bin/true".into()]),
                env: None,
                cwd: None,
                user: None,
            }),
            mounts: vec![],
        };
        let (program, argv) = parse_program_and_args(&cfg).unwrap();
        assert_eq!(program, PathBuf::from("/bin/true"));
        assert!(argv.is_empty());
    }

    #[test]
    fn test_parse_program_and_args_empty_args() {
        let cfg = OciConfig {
            process: Some(OciProcess {
                args: Some(vec![]),
                env: None,
                cwd: None,
                user: None,
            }),
            mounts: vec![],
        };
        let result = parse_program_and_args(&cfg);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("at least one element"));
    }

    #[test]
    fn test_parse_program_and_args_none_args() {
        let cfg = OciConfig {
            process: Some(OciProcess {
                args: None,
                env: None,
                cwd: None,
                user: None,
            }),
            mounts: vec![],
        };
        let result = parse_program_and_args(&cfg);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_program_and_args_no_process() {
        let cfg = OciConfig {
            process: None,
            mounts: vec![],
        };
        let result = parse_program_and_args(&cfg);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing 'process'"));
    }

    #[test]
    fn test_parse_program_and_args_relative_path() {
        let cfg = OciConfig {
            process: Some(OciProcess {
                args: Some(vec!["my-binary".into(), "--flag".into()]),
                env: None,
                cwd: None,
                user: None,
            }),
            mounts: vec![],
        };
        let (program, argv) = parse_program_and_args(&cfg).unwrap();
        assert_eq!(program, PathBuf::from("my-binary"));
        assert_eq!(argv, vec!["--flag"]);
    }

    // --- do_create tests ---

    #[test]
    #[serial]
    fn test_do_create_basic() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create("test-create", bundle.path(), false, None, None, None).unwrap();

            let state = load_state("test-create").unwrap();
            assert_eq!(state.id, "test-create");
            assert_eq!(state.status, "created");
            assert!(!state.terminal);
            assert!(state.stdin.is_none());
            assert!(state.stdout.is_none());
            assert!(state.stderr.is_none());
        });
    }

    #[test]
    #[serial]
    fn test_do_create_with_terminal() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create("test-term", bundle.path(), true, None, None, None).unwrap();

            let state = load_state("test-term").unwrap();
            assert!(state.terminal);
        });
    }

    #[test]
    #[serial]
    fn test_do_create_with_io_paths() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create(
                "test-io",
                bundle.path(),
                false,
                Some("/path/stdin".into()),
                Some("/path/stdout".into()),
                Some("/path/stderr".into()),
            )
            .unwrap();

            let state = load_state("test-io").unwrap();
            assert_eq!(state.stdin, Some("/path/stdin".into()));
            assert_eq!(state.stdout, Some("/path/stdout".into()));
            assert_eq!(state.stderr, Some("/path/stderr".into()));
        });
    }

    // --- do_state tests ---

    #[test]
    #[serial]
    fn test_do_state_existing_container() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create("test-state", bundle.path(), false, None, None, None).unwrap();
            // do_state prints JSON to stdout — just verify it doesn't error
            let result = do_state("test-state");
            assert!(result.is_ok());
        });
    }

    #[test]
    #[serial]
    fn test_do_state_nonexistent() {
        with_test_root(|_| {
            let result = do_state("nonexistent-container");
            assert!(result.is_err());
        });
    }

    // --- do_delete tests ---

    #[test]
    #[serial]
    fn test_do_delete_existing() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create("test-del", bundle.path(), false, None, None, None).unwrap();
            let result = do_delete("test-del");
            assert!(result.is_ok());
            // Verify state is gone
            assert!(load_state("test-del").is_err());
        });
    }

    #[test]
    #[serial]
    fn test_do_delete_nonexistent() {
        with_test_root(|_| {
            // delete_state currently succeeds for nonexistent containers (remove_dir_all on missing dir)
            let result = do_delete("no-such-container");
            // Just verify it doesn't panic; it may succeed or error depending on state module behavior
            let _ = result;
        });
    }

    // --- do_kill tests ---

    #[test]
    #[serial]
    fn test_do_kill_no_pid_file() {
        with_test_root(|_| {
            // No container state / PID file exists
            let result = do_kill("nonexistent", Some(15));
            assert!(result.is_err());
        });
    }

    #[test]
    #[serial]
    fn test_do_kill_default_signal() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create("test-kill", bundle.path(), false, None, None, None).unwrap();
            // Spawn a real short-lived child so we have a valid PID
            let child = std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap();
            let pid = child.id() as i32;
            save_pid("test-kill", pid).unwrap();

            // Kill with default signal (SIGTERM)
            let result = do_kill("test-kill", None);
            assert!(result.is_ok());

            // Clean up: wait for child to actually die
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
        });
    }

    #[test]
    #[serial]
    fn test_do_kill_esrch_already_exited() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create("test-esrch", bundle.path(), false, None, None, None).unwrap();

            // Spawn a child and wait for it to exit, then try to kill its (now-dead) PID
            let child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id() as i32;
            let mut child = child;
            let _ = child.wait(); // wait for it to finish

            save_pid("test-esrch", pid).unwrap();

            // Kill should succeed (ESRCH is treated as success)
            let result = do_kill("test-esrch", Some(15));
            assert!(result.is_ok());
        });
    }

    #[test]
    #[serial]
    fn test_do_kill_invalid_signal() {
        with_test_root(|_| {
            let bundle = TempDir::new().unwrap();
            do_create("test-badsig", bundle.path(), false, None, None, None).unwrap();
            save_pid("test-badsig", std::process::id() as i32).unwrap();

            // Signal 999 is invalid
            let result = do_kill("test-badsig", Some(999));
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("invalid signal"));
        });
    }

    // --- open_log_file tests ---

    #[test]
    fn test_open_log_file_regular_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("testfile");
        // Create a regular file first (open_log_file expects it to exist)
        fs::write(&path, "").unwrap();
        let result = open_log_file(path.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_open_log_file_nonexistent() {
        let result = open_log_file("/nonexistent/path/to/fifo");
        assert!(result.is_err());
    }
}
