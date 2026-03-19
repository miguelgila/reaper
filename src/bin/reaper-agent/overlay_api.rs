use serde::Serialize;
use std::fs;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::overlay_gc::{self};

/// Response for GET /api/v1/overlays
#[derive(Serialize)]
pub struct OverlayInfo {
    pub namespace: String,
    pub name: String,
    pub ready: bool,
}

/// Response for GET /api/v1/overlays/{namespace}/{name}
#[derive(Serialize)]
pub struct OverlayDetail {
    pub namespace: String,
    pub name: String,
    pub ready: bool,
    pub helper_pid: Option<i32>,
}

/// List all named overlays on this node.
///
/// Scans the ns/ directory for files matching the `<ns>--<name>` pattern.
pub fn list_overlays(state_dir: &str) -> Vec<OverlayInfo> {
    let ns_dir = Path::new(state_dir).join("ns");
    let mut overlays = Vec::new();

    if !ns_dir.exists() {
        return overlays;
    }

    let entries = match fs::read_dir(&ns_dir) {
        Ok(e) => e,
        Err(_) => return overlays,
    };

    for entry in entries.flatten() {
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Skip .pid files
        if name.ends_with(".pid") {
            continue;
        }

        // Only named overlays (ns--name pattern)
        if let Some((ns, overlay_name)) = name.split_once("--") {
            overlays.push(OverlayInfo {
                namespace: ns.to_string(),
                name: overlay_name.to_string(),
                ready: true,
            });
        }
    }

    overlays
}

/// Get details for a specific named overlay on this node.
pub fn get_overlay(state_dir: &str, namespace: &str, name: &str) -> Option<OverlayDetail> {
    let ns_key = format!("{}--{}", namespace, name);
    let ns_file = Path::new(state_dir).join("ns").join(&ns_key);

    if !ns_file.exists() {
        // Also check overlay directory existence (overlay may exist but ns file is gone)
        let overlay_dir = Path::new(state_dir)
            .join("overlay")
            .join(namespace)
            .join(name);
        if !overlay_dir.exists() {
            return None;
        }
    }

    let pid_file = Path::new(state_dir)
        .join("ns")
        .join(format!("{}.pid", ns_key));
    let helper_pid = overlay_gc::read_pid_file_pub(&pid_file).map(|(pid, _)| pid);

    Some(OverlayDetail {
        namespace: namespace.to_string(),
        name: name.to_string(),
        ready: ns_file.exists(),
        helper_pid,
    })
}

/// Delete/reset a named overlay on this node.
///
/// Returns Ok(true) if overlay was found and cleaned, Ok(false) if not found.
pub fn delete_overlay(state_dir: &str, namespace: &str, name: &str) -> Result<bool, String> {
    let ns_key = format!("{}--{}", namespace, name);
    let base = Path::new(state_dir);

    let ns_file = base.join("ns").join(&ns_key);
    let overlay_dir = base.join("overlay").join(namespace).join(name);
    let merged_dir = base.join("merged").join(namespace).join(name);
    let lock_path = base.join(format!("overlay-{}.lock", ns_key));
    let pid_file = base.join("ns").join(format!("{}.pid", ns_key));

    // Check if overlay exists at all
    if !ns_file.exists() && !overlay_dir.exists() {
        debug!(
            namespace = namespace,
            name = name,
            "overlay not found on this node"
        );
        return Ok(false);
    }

    // Safety: check for running containers referencing this namespace
    if overlay_gc::has_running_containers_pub(state_dir, namespace) {
        return Err(format!(
            "cannot delete overlay {}/{}: running containers reference namespace '{}'",
            namespace, name, namespace
        ));
    }

    info!(
        namespace = namespace,
        name = name,
        "deleting named overlay on this node"
    );

    // 1. Kill helper process
    if pid_file.exists() {
        if let Some((pid, _)) = overlay_gc::read_pid_file_pub(&pid_file) {
            if overlay_gc::is_pid_alive_pub(pid) {
                overlay_gc::kill_helper_pub(pid);
            }
        }
        remove_path(&pid_file, "pid file");
    }

    // 2. Unmount namespace bind-mount
    overlay_gc::try_unmount_pub(&ns_file);
    remove_path(&ns_file, "namespace bind-mount");

    // 3. Remove overlay directories
    remove_path(&overlay_dir, "overlay dir");
    remove_path(&merged_dir, "merged dir");

    // 4. Remove lock file
    remove_path(&lock_path, "lock file");

    info!(
        namespace = namespace,
        name = name,
        "overlay deleted successfully"
    );
    Ok(true)
}

/// Remove a file or directory, logging on failure.
fn remove_path(path: &Path, description: &str) {
    if !path.exists() {
        return;
    }

    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    match result {
        Ok(()) => {
            info!(path = ?path, what = description, "removed overlay artifact");
        }
        Err(e) => {
            warn!(
                error = %e,
                path = ?path,
                what = description,
                "failed to remove overlay artifact"
            );
        }
    }
}
