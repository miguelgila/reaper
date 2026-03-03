use std::path::Path;
use tracing::{debug, warn};

use crate::metrics::MetricsState;

/// Result of a health check cycle.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HealthResult {
    pub healthy: bool,
    pub shim_present: bool,
    pub runtime_present: bool,
    pub state_dir_accessible: bool,
    pub details: Vec<String>,
}

/// Run health checks against the host filesystem (via hostPath mounts).
pub fn check_health(shim_path: &str, runtime_path: &str, state_dir: &str) -> HealthResult {
    let mut details = Vec::new();

    let shim_present = check_binary(shim_path, "containerd-shim-reaper-v2", &mut details);
    let runtime_present = check_binary(runtime_path, "reaper-runtime", &mut details);
    let state_dir_accessible = check_directory(state_dir, "state directory", &mut details);

    let healthy = shim_present && runtime_present && state_dir_accessible;

    HealthResult {
        healthy,
        shim_present,
        runtime_present,
        state_dir_accessible,
        details,
    }
}

fn check_binary(path: &str, name: &str, details: &mut Vec<String>) -> bool {
    let p = Path::new(path);
    if !p.exists() {
        let msg = format!("{} not found at {}", name, path);
        warn!(msg);
        details.push(msg);
        return false;
    }

    // Check executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = p.metadata() {
            let mode = meta.permissions().mode();
            if mode & 0o111 == 0 {
                let msg = format!("{} at {} is not executable", name, path);
                warn!(msg);
                details.push(msg);
                return false;
            }
        }
    }

    debug!(path = path, name = name, "binary check passed");
    true
}

fn check_directory(path: &str, name: &str, details: &mut Vec<String>) -> bool {
    let p = Path::new(path);
    if !p.exists() {
        let msg = format!("{} does not exist at {}", name, path);
        warn!(msg);
        details.push(msg);
        return false;
    }
    if !p.is_dir() {
        let msg = format!("{} at {} is not a directory", name, path);
        warn!(msg);
        details.push(msg);
        return false;
    }
    debug!(path = path, name = name, "directory check passed");
    true
}

/// Periodic health check loop (runs every 30s).
pub async fn health_loop(
    shim_path: &str,
    runtime_path: &str,
    state_dir: &str,
    metrics: &MetricsState,
) {
    let interval = tokio::time::Duration::from_secs(30);
    loop {
        tokio::time::sleep(interval).await;
        let result = check_health(shim_path, runtime_path, state_dir);
        metrics.set_healthy(result.healthy);
    }
}
