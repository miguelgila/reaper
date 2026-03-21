use clap::Parser;
use std::net::SocketAddr;
use tokio::signal;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

mod config_sync;
mod executor;
mod gc;
mod health;
mod jobs;
mod metrics;
mod node_condition;
mod overlay_api;
mod overlay_gc;

// config.rs is available as shared module but not needed by the agent
// (the agent writes config files, it doesn't read them)

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
    name = "reaper-agent",
    version = version_string(),
    about = "Per-node Reaper agent: config sync, GC, health, and metrics"
)]
struct Cli {
    /// HTTP listen address for health and metrics endpoints
    #[arg(long, default_value = "0.0.0.0:9100", env = "REAPER_AGENT_LISTEN")]
    listen: SocketAddr,

    /// Kubernetes namespace containing the reaper-config ConfigMap
    #[arg(
        long,
        default_value = "reaper-system",
        env = "REAPER_AGENT_CONFIG_NAMESPACE"
    )]
    config_namespace: String,

    /// Name of the ConfigMap to watch
    #[arg(
        long,
        default_value = "reaper-config",
        env = "REAPER_AGENT_CONFIG_NAME"
    )]
    config_name: String,

    /// Path to write the config file on the host (via hostPath mount)
    #[arg(
        long,
        default_value = "/host/etc/reaper/reaper.conf",
        env = "REAPER_AGENT_CONFIG_PATH"
    )]
    config_path: String,

    /// GC scan interval in seconds
    #[arg(long, default_value = "60", env = "REAPER_AGENT_GC_INTERVAL")]
    gc_interval: u64,

    /// Overlay GC reconciliation interval in seconds
    #[arg(long, default_value = "300", env = "REAPER_AGENT_OVERLAY_GC_INTERVAL")]
    overlay_gc_interval: u64,

    /// Enable overlay GC (reconcile overlay dirs against K8s namespaces)
    #[arg(long, default_value = "true", env = "REAPER_AGENT_OVERLAY_GC_ENABLED")]
    overlay_gc_enabled: bool,

    /// Base state directory (via hostPath mount)
    #[arg(
        long,
        default_value = "/host/run/reaper",
        env = "REAPER_AGENT_STATE_DIR"
    )]
    state_dir: String,

    /// Path to check for shim binary (via hostPath mount)
    #[arg(
        long,
        default_value = "/host/usr/local/bin/containerd-shim-reaper-v2",
        env = "REAPER_AGENT_SHIM_PATH"
    )]
    shim_path: String,

    /// Path to check for runtime binary (via hostPath mount)
    #[arg(
        long,
        default_value = "/host/usr/local/bin/reaper-runtime",
        env = "REAPER_AGENT_RUNTIME_PATH"
    )]
    runtime_path: String,

    /// Enable node condition reporting (patch Node with ReaperReady condition)
    #[arg(
        long,
        default_value = "true",
        env = "REAPER_AGENT_NODE_CONDITION_ENABLED"
    )]
    node_condition_enabled: bool,

    /// Node condition update interval in seconds
    #[arg(
        long,
        default_value = "30",
        env = "REAPER_AGENT_NODE_CONDITION_INTERVAL"
    )]
    node_condition_interval: u64,

    /// Node name (set via downward API). Required when node condition reporting is enabled.
    #[arg(long, env = "NODE_NAME")]
    node_name: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    info!(version = version_string(), "reaper-agent starting");

    // Initialize shared metrics registry
    let metrics_state = metrics::MetricsState::new();

    // Run initial GC before starting loops
    info!("running initial GC scan");
    gc::run_gc(&cli.state_dir, &metrics_state).await;

    // Run initial health check
    let health_result = health::check_health(&cli.shim_path, &cli.runtime_path, &cli.state_dir);
    metrics_state.set_healthy(health_result.healthy);
    info!(healthy = health_result.healthy, "initial health check");

    // Spawn concurrent tasks
    let gc_state_dir = cli.state_dir.clone();
    let gc_metrics = metrics_state.clone();
    let gc_interval = cli.gc_interval;
    let gc_handle = tokio::spawn(async move {
        gc::gc_loop(&gc_state_dir, gc_interval, &gc_metrics).await;
    });

    let health_shim = cli.shim_path.clone();
    let health_runtime = cli.runtime_path.clone();
    let health_state_dir = cli.state_dir.clone();
    let health_metrics = metrics_state.clone();
    let health_handle = tokio::spawn(async move {
        health::health_loop(
            &health_shim,
            &health_runtime,
            &health_state_dir,
            &health_metrics,
        )
        .await;
    });

    let sync_namespace = cli.config_namespace.clone();
    let sync_name = cli.config_name.clone();
    let sync_path = cli.config_path.clone();
    let sync_metrics = metrics_state.clone();
    let sync_handle = tokio::spawn(async move {
        if let Err(e) =
            config_sync::config_sync_loop(&sync_namespace, &sync_name, &sync_path, &sync_metrics)
                .await
        {
            error!(error = %e, "config sync loop exited with error");
        }
    });

    // Spawn node condition reporting loop (patch Node with ReaperReady condition)
    let node_condition_handle = if cli.node_condition_enabled {
        match &cli.node_name {
            Some(name) => {
                let nc_node = name.clone();
                let nc_shim = cli.shim_path.clone();
                let nc_runtime = cli.runtime_path.clone();
                let nc_state_dir = cli.state_dir.clone();
                let nc_metrics = metrics_state.clone();
                let nc_interval = cli.node_condition_interval;
                Some(tokio::spawn(async move {
                    node_condition::node_condition_loop(
                        &nc_node,
                        &nc_shim,
                        &nc_runtime,
                        &nc_state_dir,
                        nc_interval,
                        &nc_metrics,
                    )
                    .await;
                }))
            }
            None => {
                error!("node condition reporting enabled but NODE_NAME not set; disable with --node-condition-enabled=false or set NODE_NAME via downward API");
                None
            }
        }
    } else {
        info!("node condition reporting disabled via --node-condition-enabled=false");
        None
    };

    // Spawn overlay GC loop (reconcile overlay dirs against K8s namespaces)
    let overlay_gc_handle = if cli.overlay_gc_enabled {
        let ogc_state_dir = cli.state_dir.clone();
        let ogc_metrics = metrics_state.clone();
        let ogc_interval = cli.overlay_gc_interval;
        Some(tokio::spawn(async move {
            overlay_gc::overlay_gc_loop(&ogc_state_dir, ogc_interval, &ogc_metrics).await;
        }))
    } else {
        info!("overlay GC disabled via --overlay-gc-enabled=false");
        None
    };

    let job_manager = executor::JobManager::new(true);
    let server_metrics = metrics_state.clone();
    let server_shim = cli.shim_path.clone();
    let server_runtime = cli.runtime_path.clone();
    let server_state_dir = cli.state_dir.clone();
    let server_job_manager = job_manager.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = metrics::serve(
            cli.listen,
            server_metrics,
            &server_shim,
            &server_runtime,
            &server_state_dir,
            server_job_manager,
        )
        .await
        {
            error!(error = %e, "HTTP server exited with error");
        }
    });

    info!(listen = %cli.listen, "reaper-agent running");

    // Wait for shutdown signal
    signal::ctrl_c().await?;
    info!("shutdown signal received, exiting");

    // Abort all tasks
    gc_handle.abort();
    health_handle.abort();
    sync_handle.abort();
    server_handle.abort();
    if let Some(h) = node_condition_handle {
        h.abort();
    }
    if let Some(h) = overlay_gc_handle {
        h.abort();
    }

    Ok(())
}
