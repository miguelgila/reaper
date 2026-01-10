use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing_subscriber::EnvFilter;

mod state;
use state::{delete as delete_state, load_pid, load_state, save_pid, save_state, ContainerState};

#[derive(Parser, Debug)]
#[command(
    name = "reaper-runtime",
    version,
    about = "Minimal OCI-like runtime for dummy apps"
)]
struct Cli {
    /// Bundle directory containing config.json
    #[arg(global = true, long, default_value = ".")]
    bundle: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a container (record metadata)
    Create { id: String },
    /// Start the container process
    Start { id: String },
    /// Print container state as JSON
    State { id: String },
    /// Send a signal to the container process
    Kill {
        id: String,
        #[arg(long, default_value_t = 15)]
        signal: i32,
    },
    /// Delete container state
    Delete { id: String },
}

#[derive(Debug, Deserialize)]
struct OciProcess {
    args: Option<Vec<String>>, // command and args
    env: Option<Vec<String>>,  // key=value
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OciConfig {
    process: Option<OciProcess>,
}

fn read_oci_config(bundle: &Path) -> Result<OciConfig> {
    let path = bundle.join("config.json");
    let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: OciConfig = serde_json::from_slice(&data).context("parsing config.json")?;
    Ok(cfg)
}

fn do_create(id: &str, bundle: &Path) -> Result<()> {
    let mut state = ContainerState::new(id.to_string(), bundle.to_path_buf());
    save_state(&state)?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

fn do_start(id: &str, bundle: &Path) -> Result<()> {
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

    let child = cmd.spawn().context("failed to spawn process")?;
    let pid = child.id() as i32;
    let mut state = load_state(id)?;
    state.status = "running".into();
    state.pid = Some(pid);
    save_state(&state)?;
    save_pid(id, pid)?;

    println!("started pid={}", pid);
    Ok(())
}

fn do_state(id: &str) -> Result<()> {
    let state = load_state(id)?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

fn do_kill(id: &str, signal: i32) -> Result<()> {
    let pid = load_pid(id)?;
    // Use nix to send signal
    let sig = nix::sys::signal::Signal::try_from(signal).context("invalid signal")?;
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), sig)
        .context("failed to send signal")?;
    Ok(())
}

fn do_delete(id: &str) -> Result<()> {
    delete_state(id)?;
    println!("deleted {}", id);
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .ok();

    let cli = Cli::parse();
    match cli.command {
        Commands::Create { id } => do_create(&id, &cli.bundle),
        Commands::Start { id } => do_start(&id, &cli.bundle),
        Commands::State { id } => do_state(&id),
        Commands::Kill { id, signal } => do_kill(&id, signal),
        Commands::Delete { id } => do_delete(&id),
    }
}
