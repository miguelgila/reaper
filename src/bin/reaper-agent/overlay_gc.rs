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

/// Kill a stale helper process (best-effort).
fn kill_helper(pid: i32) {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    match signal::kill(Pid::from_raw(pid), Signal::SIGKILL) {
        Ok(()) => info!(pid = pid, "killed stale namespace helper"),
        Err(e) => debug!(pid = pid, error = %e, "failed to kill stale helper (already dead?)"),
    }
}

/// Read a PID file in `<pid> <inode>` format. Returns None on any error.
fn read_pid_file(path: &Path) -> Option<(i32, u64)> {
    let content = fs::read_to_string(path).ok()?;
    let mut parts = content.trim().split_whitespace();
    let pid: i32 = parts.next()?.parse().ok()?;
    let inode: u64 = parts.next()?.parse().ok()?;
    Some((pid, inode))
}

/// Check if a helper process's mount namespace inode matches the expected value.
#[cfg(target_os = "linux")]
fn ns_inode_matches(pid: i32, expected_inode: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    let ns_path = format!("/proc/{}/ns/mnt", pid);
    match fs::metadata(&ns_path) {
        Ok(meta) => meta.ino() == expected_inode,
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn ns_inode_matches(_pid: i32, _expected_inode: u64) -> bool {
    false
}

/// Check whether a path is a mount point by checking mountinfo.
///
/// First checks `/proc/self/mountinfo` (agent's own mount namespace).
/// If not found and the path starts with `/host/`, also checks `/proc/1/mountinfo`
/// (host mount namespace via hostPID) with the `/host` prefix stripped.
/// This handles the case where the agent runs in a container with hostPath volumes
/// and mount propagation doesn't cross nested container boundaries (e.g., Kind).
#[cfg(target_os = "linux")]
fn is_mount_point(path: &Path) -> bool {
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let canonical_str = canonical.to_string_lossy();

    // Check agent's own mount namespace first
    if check_mountinfo("/proc/self/mountinfo", &canonical_str) {
        return true;
    }

    // If path starts with /host/, check host's mount namespace with prefix stripped
    if let Some(host_path) = canonical_str.strip_prefix("/host") {
        if check_mountinfo("/proc/1/mountinfo", host_path) {
            return true;
        }
    }

    false
}

/// Check if a path appears as a mount point in the given mountinfo file.
#[cfg(target_os = "linux")]
fn check_mountinfo(mountinfo_path: &str, target_path: &str) -> bool {
    use std::io::BufRead;

    let file = match fs::File::open(mountinfo_path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // mountinfo format: id parent major:minor root mount_point options ...
    // Field 5 (0-indexed: 4) is the mount point
    for line in std::io::BufReader::new(file).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 5 && fields[4] == target_path {
            return true;
        }
    }

    false
}

/// Try to acquire a non-blocking exclusive flock on a lock file.
/// Returns None if the lock is held by another process (skip this ns).
/// Returns None if the lock file doesn't exist (nothing to protect).
#[cfg(target_os = "linux")]
fn try_lock_nonblocking(lock_path: &Path) -> Option<nix::fcntl::Flock<std::fs::File>> {
    use nix::fcntl::{Flock, FlockArg};
    let file = match std::fs::File::open(lock_path) {
        Ok(f) => f,
        Err(_) => return None,
    };
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Some(lock),
        Err(_) => None,
    }
}

/// Parse a namespace filename into (k8s_namespace, optional_overlay_name).
/// `"default"` → `("default", None)`
/// `"default--my-group"` → `("default", Some("my-group"))`
#[cfg(target_os = "linux")]
fn parse_ns_filename(name: &str) -> (&str, Option<&str>) {
    match name.split_once("--") {
        Some((ns, group)) => (ns, Some(group)),
        None => (name, None),
    }
}

/// Check whether ANY container state directory has status "running" with a live PID,
/// regardless of namespace. Used for the legacy `shared-mnt-ns` file.
#[cfg(target_os = "linux")]
fn has_any_running_container(state_dir: &str) -> bool {
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

        let value: serde_json::Value = match serde_json::from_slice(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status == "running" {
            if let Some(pid) = value.get("pid").and_then(|v| v.as_i64()) {
                if pid > 0 && is_pid_alive(pid as i32) {
                    return true;
                }
            }
        }
    }

    false
}

/// Run a single mount namespace cleanup pass.
///
/// Scans `/run/reaper/ns/` for stale bind-mount files (files that are no longer
/// actual mount points) and removes them after safety checks.
#[cfg(target_os = "linux")]
pub fn run_ns_cleanup(state_dir: &str, metrics: &MetricsState) {
    let ns_dir = Path::new(state_dir).join("ns");
    if !ns_dir.exists() {
        debug!(path = ?ns_dir, "ns directory does not exist, skipping cleanup");
        metrics.inc_ns_cleanup_runs();
        return;
    }

    let entries = match fs::read_dir(&ns_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, path = ?ns_dir, "failed to read ns directory");
            metrics.inc_ns_cleanup_runs();
            return;
        }
    };

    let mut scanned = 0u64;
    let mut cleaned = 0u64;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Skip .pid files — they are managed alongside their ns files
        if name.ends_with(".pid") {
            continue;
        }

        scanned += 1;

        // If it's a live mount point, skip
        if is_mount_point(&path) {
            debug!(file = %name, "ns file is a live mount point, skipping");
            continue;
        }

        // Check PID file fallback: if a .pid file exists with a live helper
        // whose namespace inode matches, the namespace is live via PID fallback
        // (bind-mount returned EINVAL in nested container environments)
        let pid_file = path.with_extension("pid");
        if pid_file.exists() {
            if let Some((pid, expected_inode)) = read_pid_file(&pid_file) {
                if is_pid_alive(pid) && ns_inode_matches(pid, expected_inode) {
                    debug!(
                        file = %name,
                        helper_pid = pid,
                        "namespace live via PID fallback, skipping"
                    );
                    continue;
                }
            }
        }

        // Parse filename to determine k8s namespace
        let (k8s_ns, overlay_name) = parse_ns_filename(&name);

        // Special case: legacy shared-mnt-ns (node-isolation mode)
        if name == "shared-mnt-ns" {
            if has_any_running_container(state_dir) {
                debug!("shared-mnt-ns has running containers, skipping");
                continue;
            }
        } else {
            // Safety: skip if any running containers reference this namespace
            if has_running_containers(state_dir, k8s_ns) {
                debug!(
                    namespace = k8s_ns,
                    overlay_name = ?overlay_name,
                    "running containers reference namespace, skipping ns cleanup"
                );
                continue;
            }
        }

        // Try non-blocking lock — skip if runtime holds it
        let lock_name = format!("overlay-{}.lock", name);
        let lock_path = Path::new(state_dir).join(&lock_name);
        let _lock = if lock_path.exists() {
            match try_lock_nonblocking(&lock_path) {
                Some(lock) => Some(lock),
                None => {
                    debug!(file = %name, "lock held by runtime, skipping ns cleanup");
                    continue;
                }
            }
        } else {
            None
        };

        info!(file = %name, "stale ns bind-mount detected, cleaning up");
        try_unmount(&path);
        if remove_path(&path, "stale ns file") {
            cleaned += 1;
        }

        // Clean up associated .pid file and kill stale helper
        if pid_file.exists() {
            if let Some((pid, _)) = read_pid_file(&pid_file) {
                if is_pid_alive(pid) {
                    kill_helper(pid);
                }
            }
            remove_path(&pid_file, "stale ns pid file");
        }
    }

    metrics.inc_ns_cleanup_runs();
    if cleaned > 0 {
        metrics.inc_ns_cleaned(cleaned);
    }

    info!(
        scanned = scanned,
        cleaned = cleaned,
        "ns cleanup pass complete"
    );
}

#[cfg(not(target_os = "linux"))]
pub fn run_ns_cleanup(_state_dir: &str, metrics: &MetricsState) {
    debug!("ns cleanup not supported on this platform");
    metrics.inc_ns_cleanup_runs();
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

        // Clean up .pid file and kill stale helper for the namespace
        let ns_pid_file = base.join("ns").join(format!("{}.pid", ns_name));
        if ns_pid_file.exists() {
            if let Some((pid, _)) = read_pid_file(&ns_pid_file) {
                if is_pid_alive(pid) {
                    kill_helper(pid);
                }
            }
            remove_path(&ns_pid_file, "namespace pid file");
        }

        // Clean named overlay groups: ns/<ns>--* files and overlay-<ns>--*.lock files
        let ns_prefix = format!("{}--", ns_name);

        // Scan ns/ directory for named group bind-mounts and .pid files
        let ns_dir = base.join("ns");
        if ns_dir.exists() {
            if let Ok(ns_entries) = fs::read_dir(&ns_dir) {
                for entry in ns_entries.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        if name.starts_with(&ns_prefix) {
                            if name.ends_with(".pid") {
                                // Clean up .pid file and kill helper
                                if let Some((pid, _)) = read_pid_file(&entry.path()) {
                                    if is_pid_alive(pid) {
                                        kill_helper(pid);
                                    }
                                }
                                remove_path(&entry.path(), "named group pid file");
                            } else {
                                try_unmount(&entry.path());
                                remove_path(&entry.path(), "named group ns file");
                            }
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

        // Clean stale ns bind-mounts before overlay GC so overlay GC sees accurate state
        run_ns_cleanup(state_dir, metrics);

        if let Err(e) = run_overlay_gc(&client, state_dir, metrics).await {
            error!(error = %e, "overlay GC cycle failed");
        }
    }
}
