use anyhow::Result;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .ok();

    tracing::info!("containerd-shim-reaper-v2 starting");

    // TODO: Set up TTRPC server
    // TODO: Implement Task service
    // TODO: Handle signals and shutdown

    println!("Shim v2 implementation - coming soon!");

    Ok(())
}
