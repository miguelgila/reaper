use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::fs;
use std::os::unix::process::CommandExt; // For pre_exec
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
        #[arg(long, default_value_t = 15)]
        signal: i32,
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

#[derive(Debug, Deserialize)]
struct OciConfig {
    process: Option<OciProcess>,
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

fn do_create(id: &str, bundle: &Path) -> Result<()> {
    info!(
        "do_create() called - id={}, bundle={}",
        id,
        bundle.display()
    );
    let state = ContainerState::new(id.to_string(), bundle.to_path_buf());
    save_state(&state)?;
    info!("do_create() succeeded - state saved for container={}", id);
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

fn do_start(id: &str, bundle: &Path) -> Result<()> {
    info!("do_start() called - id={}, bundle={}", id, bundle.display());
    let cfg = read_oci_config(bundle)?;
    let proc = cfg
        .process
        .context("config.json missing 'process' section")?;
    let args = proc.args.unwrap_or_default();
    if args.is_empty() {
        bail!("process.args must contain at least one element (program)");
    }
    let program = &args[0];
    let argv = &args[1..];
    info!(
        "do_start() - program={}, args={:?}, cwd={:?}",
        program, argv, proc.cwd
    );

    // Handle user/group ID switching before exec
    // This must be done in the child process, not the parent
    let user_config = proc.user;
    if let Some(ref user) = user_config {
        info!(
            "do_start() - user config: uid={}, gid={}, additional_gids={:?}, umask={:?}",
            user.uid, user.gid, user.additional_gids, user.umask
        );
    } else {
        info!("do_start() - no user config, will run as current user");
    }

    // We need to use a pre_exec hook to set uid/gid before the child process runs
    // This is safer than trying to do it in the parent
    unsafe {
        let mut cmd = Command::new(program);
        cmd.args(argv);
        if let Some(cwd) = proc.cwd.as_deref() {
            cmd.current_dir(cwd);
        }
        // Pass env as key=value
        if let Some(envs) = proc.env.as_ref() {
            for kv in envs {
                if let Some((k, v)) = kv.split_once('=') {
                    cmd.env(k, v);
                }
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        // Set uid/gid/groups in the child process before exec
        if let Some(user) = user_config {
            cmd.pre_exec(move || {
                use nix::unistd::{setgid, setuid, Gid, Uid};

                // Set supplementary groups first
                if !user.additional_gids.is_empty() {
                    let gids: Vec<nix::libc::gid_t> = user.additional_gids.clone();
                    // setgroups signature differs by platform:
                    // - Linux: setgroups(size_t, *const gid_t)
                    // - macOS/BSD: setgroups(c_int, *const gid_t)
                    #[cfg(target_os = "linux")]
                    let ret = nix::libc::setgroups(gids.len(), gids.as_ptr());
                    #[cfg(not(target_os = "linux"))]
                    let ret = nix::libc::setgroups(gids.len() as nix::libc::c_int, gids.as_ptr());

                    if ret != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }

                // Set GID (must be done before UID for privilege dropping)
                let gid = Gid::from_raw(user.gid);
                setgid(gid).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("setgid failed: {}", e),
                    )
                })?;

                // Set UID (this drops privileges if running as root)
                let uid = Uid::from_raw(user.uid);
                setuid(uid).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("setuid failed: {}", e),
                    )
                })?;

                // Set umask if specified
                if let Some(umask_value) = user.umask {
                    nix::libc::umask(umask_value as nix::libc::mode_t);
                }

                Ok(())
            });
        }

        info!("do_start() - spawning process...");
        let child = cmd.spawn().context("failed to spawn process")?;
        let pid = child.id() as i32;
        info!("do_start() - process spawned successfully, pid={}", pid);
        let mut state = load_state(id)?;
        state.status = "running".into();
        state.pid = Some(pid);
        save_state(&state)?;
        save_pid(id, pid)?;
        info!("do_start() succeeded - container={}, pid={}", id, pid);

        println!("started pid={}", pid);
        Ok(())
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

fn do_kill(id: &str, signal: i32) -> Result<()> {
    info!("do_kill() called - id={}, signal={}", id, signal);
    let pid = load_pid(id)?;
    info!("do_kill() - sending signal {} to pid {}", signal, pid);
    // Use nix to send signal
    let sig = nix::sys::signal::Signal::try_from(signal).context("invalid signal")?;
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), sig)
        .context("failed to send signal")?;
    info!(
        "do_kill() succeeded - id={}, signal={}, pid={}",
        id, signal, pid
    );
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
        Commands::Create { ref id } => do_create(id, bundle),
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
