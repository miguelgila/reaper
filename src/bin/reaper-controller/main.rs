use clap::Parser;
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;

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
        let crd = serde_json::to_string_pretty(
            &<reaper::crds::ReaperPod as kube::CustomResourceExt>::crd(),
        )?;
        println!("{crd}");
        return Ok(());
    }

    info!(version = version_string(), "reaper-controller starting");

    let client = kube::Client::try_default().await?;

    // Run controller with graceful shutdown
    tokio::select! {
        result = reconciler::run(client) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "controller exited with error");
            }
        }
        _ = signal::ctrl_c() => {
            info!("shutdown signal received, exiting");
        }
    }

    Ok(())
}
