use clap::Parser;
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod daemon_job_reconciler;
mod overlay_reconciler;
mod pod_builder;
mod reconciler;

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
    name = "reaper-controller",
    version = version_string(),
    about = "Kubernetes controller for ReaperPod custom resources"
)]
struct Cli {
    /// Print the CRD YAML and exit.
    #[arg(long)]
    generate_crds: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    if cli.generate_crds {
        let pod_crd = serde_json::to_string_pretty(
            &<reaper::crds::ReaperPod as kube::CustomResourceExt>::crd(),
        )?;
        let overlay_crd = serde_json::to_string_pretty(
            &<reaper::crds::ReaperOverlay as kube::CustomResourceExt>::crd(),
        )?;
        let daemon_job_crd = serde_json::to_string_pretty(
            &<reaper::crds::ReaperDaemonJob as kube::CustomResourceExt>::crd(),
        )?;
        println!("{pod_crd}");
        eprintln!("---");
        println!("{overlay_crd}");
        eprintln!("---");
        println!("{daemon_job_crd}");
        return Ok(());
    }

    info!(version = version_string(), "reaper-controller starting");

    let client = kube::Client::try_default().await?;

    // Run all controllers with graceful shutdown
    let reaperpod_client = client.clone();
    let overlay_client = client.clone();
    let daemon_job_client = client.clone();

    tokio::select! {
        result = reconciler::run(reaperpod_client) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "ReaperPod controller exited with error");
            }
        }
        result = overlay_reconciler::run(overlay_client) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "ReaperOverlay controller exited with error");
            }
        }
        result = daemon_job_reconciler::run(daemon_job_client) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "ReaperDaemonJob controller exited with error");
            }
        }
        _ = signal::ctrl_c() => {
            info!("shutdown signal received, exiting");
        }
    }

    Ok(())
}
