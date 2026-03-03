use serde::Deserialize;
use std::fs;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::metrics::MetricsState;

/// Minimal deserialization of state.json — only the fields GC needs.
#[derive(Debug, Deserialize)]
pub struct ContainerStateMinimal {
    pub id: String,
    pub status: String,
    pub pid: Option<i32>,
    #[serde(default)]
    #[allow(dead_code)]
    pub exit_code: Option<i32>,
}

/// Check whether a PID is still alive using kill(pid, 0).
fn is_pid_alive(pid: i32) -> bool {
    use nix::sys::signal;
    use nix::unistd::Pid;
    // Signal 0 doesn't send a signal, just checks if process exists
    signal::kill(Pid::from_raw(pid), None).is_ok()
}

/// Run a single GC pass: scan state dirs, detect dead PIDs, update state files.
///
/// Returns (running, stopped, cleaned) counts for metrics.
pub async fn run_gc(state_dir: &str, metrics: &MetricsState) {
    let base = Path::new(state_dir);
    if !base.exists() {
        debug!(
            path = state_dir,
            "state directory does not exist, skipping GC"
        );
        return;
    }

    let entries = match fs::read_dir(base) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, path = state_dir, "failed to read state directory");
            return;
        }
    };

    let mut running = 0u64;
    let mut stopped = 0u64;
    let mut created = 0u64;
    let mut cleaned = 0u64;

    // Infrastructure directories that are NOT container state dirs — skip during GC
    const INFRA_DIRS: &[&str] = &["overlay", "merged", "ns"];

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Skip overlay infrastructure directories (managed by overlay GC, not container GC)
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if INFRA_DIRS.contains(&name) {
                continue;
            }
        }

        let state_file = path.join("state.json");
        if !state_file.exists() {
            // Directory with no state.json — orphaned, clean up
            debug!(dir = ?path, "removing orphaned state directory (no state.json)");
            if let Err(e) = fs::remove_dir_all(&path) {
                warn!(error = %e, dir = ?path, "failed to remove orphaned directory");
            } else {
                cleaned += 1;
            }
            continue;
        }

        let data = match fs::read(&state_file) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, file = ?state_file, "failed to read state file");
                continue;
            }
        };

        let state: ContainerStateMinimal = match serde_json::from_slice(&data) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, file = ?state_file, "failed to parse state file, removing");
                if let Err(e) = fs::remove_dir_all(&path) {
                    warn!(error = %e, dir = ?path, "failed to remove corrupted state dir");
                } else {
                    cleaned += 1;
                }
                continue;
            }
        };

        match state.status.as_str() {
            "running" => {
                if let Some(pid) = state.pid {
                    if is_pid_alive(pid) {
                        running += 1;
                    } else {
                        // Process is gone — mark as stopped
                        info!(
                            id = state.id,
                            pid = pid,
                            "detected dead process, marking as stopped"
                        );
                        if let Err(e) = mark_stopped(&state_file, &data) {
                            warn!(error = %e, id = state.id, "failed to update state to stopped");
                        }
                        stopped += 1;
                    }
                } else {
                    // Running with no PID — shouldn't happen, count as stopped
                    warn!(
                        id = state.id,
                        "running container with no PID, marking as stopped"
                    );
                    if let Err(e) = mark_stopped(&state_file, &data) {
                        warn!(error = %e, id = state.id, "failed to update state to stopped");
                    }
                    stopped += 1;
                }
            }
            "stopped" => stopped += 1,
            "created" => created += 1,
            other => {
                debug!(id = state.id, status = other, "unknown container status");
            }
        }
    }

    metrics.set_containers(created, running, stopped);
    metrics.inc_gc_runs();

    info!(
        running = running,
        stopped = stopped,
        created = created,
        cleaned = cleaned,
        "GC scan complete"
    );
}

/// Update a state file to mark the container as stopped with exit_code -1.
/// Uses serde_json::Value to preserve all existing fields.
fn mark_stopped(state_file: &Path, data: &[u8]) -> anyhow::Result<()> {
    let mut value: serde_json::Value = serde_json::from_slice(data)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "status".to_string(),
            serde_json::Value::String("stopped".to_string()),
        );
        obj.insert("exit_code".to_string(), serde_json::json!(-1));
    }
    let updated = serde_json::to_vec_pretty(&value)?;
    fs::write(state_file, updated)?;
    Ok(())
}

/// Run GC in a loop at the configured interval.
pub async fn gc_loop(state_dir: &str, interval_secs: u64, metrics: &MetricsState) {
    let interval = tokio::time::Duration::from_secs(interval_secs);
    loop {
        tokio::time::sleep(interval).await;
        run_gc(state_dir, metrics).await;
    }
}
