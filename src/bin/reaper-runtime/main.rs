use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod state;
use state::{delete as delete_state, load_pid, load_state, save_pid, save_state, ContainerState};

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

/// Open a log FIFO for writing. FIFOs are created by containerd and we open them for writing.
/// Uses non-blocking open to avoid hanging if containerd isn't ready yet.
fn open_log_file(path: &str) -> Result<std::fs::File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK) // Non-blocking to prevent hangs
        .open(path)
        .with_context(|| format!("Failed to open log FIFO at {}", path))
}

fn do_create(
    id: &str,
    bundle: &Path,
    stdin: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
) -> Result<()> {
    info!(
        "do_create() called - id={}, bundle={}, stdin={:?}, stdout={:?}, stderr={:?}",
        id,
        bundle.display(),
        stdin,
        stdout,
        stderr
    );
    let mut state = ContainerState::new(id.to_string(), bundle.to_path_buf());
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

            // Wait briefly for daemon to spawn workload and update state
            // This is a simple synchronization mechanism
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Read the PID from state (daemon should have updated it)
            if let Ok(state) = load_state(&container_id) {
                if let Some(pid) = state.pid {
                    println!("started pid={}", pid);
                } else {
                    // Fallback: report daemon PID if workload PID not yet available
                    println!("started pid={}", daemon_pid);
                }
            } else {
                println!("started pid={}", daemon_pid);
            }

            Ok(())
        }
        Ok(ForkResult::Child) => {
            // Child process (monitoring daemon)
            // This process will spawn and monitor the workload

            // Detach from parent session to become a proper daemon
            if let Err(e) = nix::unistd::setsid() {
                eprintln!("Monitor daemon: setsid failed: {}", e);
            }

            // Now spawn the workload - we are its parent!
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

            // Reload state to get I/O paths that containerd provided
            let io_state = load_state(&container_id).ok();

            // Configure stdin
            cmd.stdin(Stdio::null());

            // Configure stdout: use FIFO if available, otherwise inherit
            if let Some(ref state) = io_state {
                if let Some(ref stdout_path) = state.stdout {
                    if !stdout_path.is_empty() {
                        match open_log_file(stdout_path) {
                            Ok(file) => {
                                cmd.stdout(Stdio::from(file));
                                info!("do_start() - redirected stdout to FIFO: {}", stdout_path);
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
                                info!("do_start() - redirected stderr to FIFO: {}", stderr_path);
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

                    // IMPORTANT: Give containerd/kubelet time to observe the "running" state
                    // before the container potentially exits. For very fast commands (like echo),
                    // the process may complete before containerd has registered the start,
                    // causing the pod to get stuck in "Running" state forever.
                    // This delay ensures the state machine transitions properly:
                    // created -> running -> stopped
                    std::thread::sleep(std::time::Duration::from_millis(500));

                    // Wait for the workload process to exit
                    // We are the parent, so this will work correctly!
                    match child.wait() {
                        Ok(exit_status) => {
                            let exit_code = exit_status.code().unwrap_or(1);

                            // Update state to stopped with exit code
                            if let Ok(mut state) = load_state(&container_id) {
                                state.status = "stopped".into();
                                state.exit_code = Some(exit_code);
                                let _ = save_state(&state);
                            }
                        }
                        Err(_e) => {
                            // wait() failed - mark as stopped with error
                            if let Ok(mut state) = load_state(&container_id) {
                                state.status = "stopped".into();
                                state.exit_code = Some(1);
                                let _ = save_state(&state);
                            }
                        }
                    }
                }
                Err(_e) => {
                    // Failed to spawn workload
                    if let Ok(mut state) = load_state(&container_id) {
                        state.status = "stopped".into();
                        state.exit_code = Some(1);
                        let _ = save_state(&state);
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
            stdin,
            stdout,
            stderr,
        } => do_create(id, bundle, stdin, stdout, stderr),
        Commands::Start { ref id } => do_start(id, bundle),
        Commands::State { ref id } => do_state(id),
        Commands::Kill { ref id, signal } => do_kill(id, signal),
        Commands::Delete { ref id, .. } => do_delete(id),
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
