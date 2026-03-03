use anyhow::Result;
use k8s_openapi::api::core::v1::Namespace;
use kube::{api::ListParams, Api, Client};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use tracing::{debug, error, info, warn};

use crate::metrics::MetricsState;

/// Check whether any container state directory references the given namespace
/// with status "running". Returns true if at least one running container exists.
fn has_running_containers(state_dir: &str, namespace: &str) -> bool {
    let base = Path::new(state_dir);
    let entries = match fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let state_file = path.join("state.json");
        let data = match fs::read(&state_file) {
            Ok(d) => d,
            Err(_) => continue,
        };

        // Deserialize into a Value first to check namespace field
        let value: serde_json::Value = match serde_json::from_slice(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let state_ns = value
            .get("namespace")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("");

        if state_ns == namespace && status == "running" {
            // Also verify the PID is actually alive
            if let Some(pid) = value.get("pid").and_then(|v| v.as_i64()) {
                if pid > 0 && is_pid_alive(pid as i32) {
                    debug!(
                        namespace = namespace,
                        container_id = ?path.file_name(),
                        pid = pid,
                        "running container references namespace, skipping overlay GC"
                    );
                    return true;
                }
            }
        }
    }

    false
}

/// Check whether a PID is still alive using kill(pid, 0).
fn is_pid_alive(pid: i32) -> bool {
    use nix::sys::signal;
    use nix::unistd::Pid;
    signal::kill(Pid::from_raw(pid), None).is_ok()
}

/// Attempt to unmount a path, ignoring ENOENT and EINVAL.
fn try_unmount(path: &Path) {
    #[cfg(target_os = "linux")]
    {
        use nix::mount::MntFlags;
        match nix::mount::umount2(path, MntFlags::MNT_DETACH) {
            Ok(()) => {
                info!(path = ?path, "unmounted namespace bind-mount");
            }
            Err(nix::errno::Errno::ENOENT | nix::errno::Errno::EINVAL) => {
                debug!(path = ?path, "already unmounted or not a mount point");
            }
            Err(e) => {
                warn!(error = %e, path = ?path, "failed to unmount namespace bind-mount");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        debug!("unmount not supported on this platform");
    }
}

/// Remove a file or directory, logging on failure.
fn remove_path(path: &Path, description: &str) -> bool {
    if !path.exists() {
        return false;
    }

    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    match result {
        Ok(()) => {
            info!(path = ?path, what = description, "removed overlay artifact");
            true
        }
        Err(e) => {
            warn!(error = %e, path = ?path, what = description, "failed to remove overlay artifact");
            false
        }
    }
}

/// Run a single overlay GC reconciliation pass.
///
/// Lists K8s namespaces via the API, compares against on-disk overlay directories,
/// and removes overlays for namespaces that no longer exist.
pub async fn run_overlay_gc(
    client: &Client,
    state_dir: &str,
    metrics: &MetricsState,
) -> Result<()> {
    let overlay_dir = Path::new(state_dir).join("overlay");
    if !overlay_dir.exists() {
        debug!(path = ?overlay_dir, "overlay directory does not exist, skipping");
        metrics.inc_overlay_gc_runs();
        metrics.set_overlay_namespaces(0);
        return Ok(());
    }

    // List all K8s namespaces
    let ns_api: Api<Namespace> = Api::all(client.clone());
    let ns_list = ns_api.list(&ListParams::default()).await?;
    let k8s_namespaces: HashSet<String> = ns_list
        .items
        .iter()
        .filter_map(|ns| ns.metadata.name.clone())
        .collect();

    debug!(count = k8s_namespaces.len(), "fetched K8s namespaces");

    // Scan on-disk overlay directories
    let entries = match fs::read_dir(&overlay_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, path = ?overlay_dir, "failed to read overlay directory");
            metrics.inc_overlay_gc_runs();
            return Ok(());
        }
    };

    let mut on_disk_namespaces: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                on_disk_namespaces.push(name.to_string());
            }
        }
    }

    metrics.set_overlay_namespaces(on_disk_namespaces.len() as u64);

    let mut cleaned = 0u64;

    for ns_name in &on_disk_namespaces {
        if k8s_namespaces.contains(ns_name) {
            debug!(
                namespace = ns_name,
                "namespace exists in K8s, keeping overlay"
            );
            continue;
        }

        // Safety check: skip if any running containers reference this namespace
        if has_running_containers(state_dir, ns_name) {
            info!(
                namespace = ns_name,
                "namespace deleted but running containers reference it, skipping"
            );
            continue;
        }

        info!(
            namespace = ns_name,
            "namespace no longer exists in K8s, cleaning overlay artifacts"
        );

        let base = Path::new(state_dir);

        // Unmount namespace bind-mount before removing
        let ns_file = base.join("ns").join(ns_name);
        try_unmount(&ns_file);

        // Remove namespace-level artifacts
        remove_path(&base.join("overlay").join(ns_name), "overlay dir");
        remove_path(&base.join("merged").join(ns_name), "merged dir");
        remove_path(&ns_file, "namespace file");
        remove_path(&base.join(format!("overlay-{}.lock", ns_name)), "lock file");

        // Clean named overlay groups: ns/<ns>--* files and overlay-<ns>--*.lock files
        let ns_prefix = format!("{}--", ns_name);

        // Scan ns/ directory for named group bind-mounts
        let ns_dir = base.join("ns");
        if ns_dir.exists() {
            if let Ok(ns_entries) = fs::read_dir(&ns_dir) {
                for entry in ns_entries.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        if name.starts_with(&ns_prefix) {
                            try_unmount(&entry.path());
                            remove_path(&entry.path(), "named group ns file");
                        }
                    }
                }
            }
        }

        // Scan for overlay-<ns>--*.lock files
        if let Ok(base_entries) = fs::read_dir(base) {
            let lock_prefix = format!("overlay-{}--", ns_name);
            for entry in base_entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&lock_prefix) && name.ends_with(".lock") {
                        remove_path(&entry.path(), "named group lock file");
                    }
                }
            }
        }

        cleaned += 1;
    }

    metrics.inc_overlay_gc_runs();
    if cleaned > 0 {
        metrics.inc_overlay_gc_cleaned(cleaned);
    }

    info!(
        on_disk = on_disk_namespaces.len(),
        k8s = k8s_namespaces.len(),
        cleaned = cleaned,
        "overlay GC reconciliation complete"
    );

    Ok(())
}

/// Run overlay GC in a loop at the configured interval.
pub async fn overlay_gc_loop(state_dir: &str, interval_secs: u64, metrics: &MetricsState) {
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to create Kubernetes client, overlay GC disabled");
            return;
        }
    };

    let interval = tokio::time::Duration::from_secs(interval_secs);
    info!(interval_secs = interval_secs, "overlay GC loop starting");

    loop {
        tokio::time::sleep(interval).await;
        if let Err(e) = run_overlay_gc(&client, state_dir, metrics).await {
            error!(error = %e, "overlay GC cycle failed");
        }
    }
}
