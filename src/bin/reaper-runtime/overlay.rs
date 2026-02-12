//! Shared mount namespace + overlayfs management.
//!
//! All Reaper workloads on a node share a single mount namespace with an
//! overlay filesystem. The host root becomes a read-only lower layer, and
//! a shared writable upper layer captures all writes. This protects the
//! host filesystem while allowing cross-deployment file sharing.
//!
//! # Architecture
//!
//! The first workload to start creates the shared namespace:
//! 1. Fork an inner helper child
//! 2. Helper: `unshare(CLONE_NEWNS)`, mount overlay, `pivot_root`
//! 3. Parent (host ns): bind-mount helper's `/proc/<pid>/ns/mnt` to persist
//! 4. Parent: kill helper (namespace lives on via bind-mount)
//! 5. Parent: `setns()` to join the namespace
//!
//! Subsequent workloads simply `setns()` into the existing namespace.

use anyhow::{bail, Context, Result};
use std::fs;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use tracing::info;

use nix::fcntl::{Flock, FlockArg};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{setns, unshare, CloneFlags};
use nix::unistd::{fork, ForkResult};

/// Overlay configuration, read from environment variables.
pub struct OverlayConfig {
    /// Base directory for overlay upper/work dirs (default: /run/reaper/overlay)
    pub base_dir: PathBuf,
    /// Path to the persisted namespace bind-mount (default: /run/reaper/shared-mnt-ns)
    pub ns_path: PathBuf,
    /// Path to the file lock for namespace creation (default: /run/reaper/overlay.lock)
    pub lock_path: PathBuf,
}

/// Filter configuration for sensitive file filtering.
pub struct FilterConfig {
    /// Whether filtering is enabled (default: true)
    pub enabled: bool,
    /// Filter mode: append to defaults or replace them
    pub mode: FilterMode,
    /// Custom paths to filter (from REAPER_FILTER_PATHS)
    pub custom_paths: Vec<PathBuf>,
    /// Allowlist: paths to exclude from filtering (from REAPER_FILTER_ALLOWLIST)
    pub allowlist: Vec<PathBuf>,
    /// Directory to store empty placeholder files (default: /run/reaper/overlay-filters)
    pub filter_dir: PathBuf,
}

/// Filter mode: append to default filters or replace them entirely.
#[derive(Debug, PartialEq)]
pub enum FilterMode {
    /// Apply default filters + custom paths
    Append,
    /// Only apply custom paths (ignore defaults)
    Replace,
}

/// Read overlay configuration from environment variables.
///
/// - `REAPER_OVERLAY_BASE`: base dir for overlay (default: "/run/reaper/overlay")
/// - `REAPER_OVERLAY_NS`: path to namespace bind-mount (default: "/run/reaper/shared-mnt-ns")
/// - `REAPER_OVERLAY_LOCK`: path to lock file (default: "/run/reaper/overlay.lock")
pub fn read_config() -> OverlayConfig {
    let base_dir = std::env::var("REAPER_OVERLAY_BASE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/reaper/overlay"));

    let ns_path = std::env::var("REAPER_OVERLAY_NS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/reaper/shared-mnt-ns"));

    let lock_path = std::env::var("REAPER_OVERLAY_LOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/reaper/overlay.lock"));

    OverlayConfig {
        base_dir,
        ns_path,
        lock_path,
    }
}

/// Read filter configuration from environment variables.
///
/// - `REAPER_FILTER_ENABLED`: enable/disable filtering (default: true)
/// - `REAPER_FILTER_MODE`: "append" or "replace" (default: append)
/// - `REAPER_FILTER_PATHS`: colon-separated custom paths to filter
/// - `REAPER_FILTER_ALLOWLIST`: colon-separated paths to exclude from filtering
/// - `REAPER_FILTER_DIR`: directory for placeholder files (default: /run/reaper/overlay-filters)
pub fn read_filter_config() -> FilterConfig {
    let enabled = std::env::var("REAPER_FILTER_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);

    let mode = std::env::var("REAPER_FILTER_MODE")
        .map(|v| match v.as_str() {
            "replace" => FilterMode::Replace,
            _ => FilterMode::Append,
        })
        .unwrap_or(FilterMode::Append);

    let custom_paths = std::env::var("REAPER_FILTER_PATHS")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();

    let allowlist = std::env::var("REAPER_FILTER_ALLOWLIST")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();

    let filter_dir = std::env::var("REAPER_FILTER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/reaper/overlay-filters"));

    FilterConfig {
        enabled,
        mode,
        custom_paths,
        allowlist,
        filter_dir,
    }
}

/// Returns the default list of sensitive paths to filter.
fn get_default_filters() -> Vec<PathBuf> {
    vec![
        // Authentication & Credentials
        PathBuf::from("/root/.ssh"),
        PathBuf::from("/etc/shadow"),
        PathBuf::from("/etc/gshadow"),
        PathBuf::from("/etc/ssh/ssh_host_rsa_key"),
        PathBuf::from("/etc/ssh/ssh_host_ecdsa_key"),
        PathBuf::from("/etc/ssh/ssh_host_ed25519_key"),
        // System Secrets
        PathBuf::from("/etc/ssl/private"),
        PathBuf::from("/var/lib/docker"),
        PathBuf::from("/run/secrets"),
        // Sensitive Configuration
        PathBuf::from("/etc/sudoers"),
        PathBuf::from("/etc/sudoers.d"),
    ]
}

/// Join or create the shared overlay namespace.
///
/// Must be called AFTER `setsid()` and BEFORE `Command::new()` in the
/// monitoring daemon child process.
///
/// Overlay is mandatory — if this fails, the workload must not run.
///
/// Tested by kind-integration tests (requires root + Linux namespaces).
#[cfg(not(tarpaulin_include))]
pub fn enter_overlay(config: &OverlayConfig) -> Result<()> {
    info!(
        "overlay: enter_overlay started, lock_path={}, ns_path={}",
        config.lock_path.display(),
        config.ns_path.display()
    );

    // Acquire exclusive lock to prevent races during namespace creation
    info!("overlay: acquiring lock...");
    let _lock = acquire_lock(&config.lock_path).context("failed to acquire overlay lock")?;
    info!("overlay: lock acquired");

    // Read host /etc files up front so we can restore them inside an existing namespace
    let host_etc = read_host_etc_files(Path::new("/etc"));

    if namespace_exists(&config.ns_path) {
        info!(
            "overlay: joining existing shared namespace at {}",
            config.ns_path.display()
        );
        join_namespace(&config.ns_path).context("failed to join existing namespace")?;
    } else {
        info!("overlay: creating new shared namespace (first workload on this node)");
        create_namespace(config).context("failed to create shared namespace")?;
        info!("overlay: shared namespace created successfully");
    }

    // After joining (or creating) the namespace, ensure resolver files exist/non-empty.
    ensure_etc_files_in_namespace(Path::new("/etc"), &host_etc);

    info!("overlay: enter_overlay completed successfully");
    Ok(())
}

/// Acquire an exclusive file lock. Blocks until the lock is available.
/// The lock is released when the returned File is dropped.
fn acquire_lock(lock_path: &Path) -> Result<Flock<fs::File>> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).context("creating lock dir")?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .context("opening lock file")?;

    // LOCK_EX: exclusive lock, blocks until acquired
    let locked = Flock::lock(file, FlockArg::LockExclusive)
        .map_err(|(_, errno)| anyhow::anyhow!("flock: {}", errno))
        .context("acquiring file lock")?;

    Ok(locked)
}

/// Check if the shared namespace bind-mount exists and is a valid namespace.
fn namespace_exists(ns_path: &Path) -> bool {
    if !ns_path.exists() {
        return false;
    }

    // Try opening to verify it's still a valid namespace reference.
    // We'll handle setns errors gracefully in join_namespace.
    fs::File::open(ns_path).is_ok()
}

/// Join an existing shared mount namespace via setns().
/// Tested by kind-integration (requires root + Linux namespaces).
#[cfg(not(tarpaulin_include))]
fn join_namespace(ns_path: &Path) -> Result<()> {
    let f = fs::File::open(ns_path).context("opening namespace file")?;
    setns(&f, CloneFlags::CLONE_NEWNS).context("setns into shared namespace")?;
    info!("overlay: successfully joined shared namespace");
    Ok(())
}

/// Create the shared mount namespace with overlay filesystem.
///
/// Uses an inner fork:
/// - Inner child: unshare(CLONE_NEWNS), mount overlay, pivot_root, signal parent
/// - Inner parent (host ns): bind-mount child's ns to persist it, kill child, join ns
///
/// Tested by kind-integration (requires root + Linux namespaces).
#[cfg(not(tarpaulin_include))]
fn create_namespace(config: &OverlayConfig) -> Result<()> {
    let upper_dir = config.base_dir.join("upper");
    let work_dir = config.base_dir.join("work");
    let merged_dir = PathBuf::from("/run/reaper/merged");

    // Create overlay directories
    fs::create_dir_all(&upper_dir).context("creating overlay upper dir")?;
    fs::create_dir_all(&work_dir).context("creating overlay work dir")?;
    fs::create_dir_all(&merged_dir).context("creating overlay merged dir")?;

    // Create pipe for coordination between inner parent and child
    let (read_fd, write_fd) = nix::unistd::pipe().context("creating coordination pipe")?;

    match unsafe { fork() }.context("inner fork for namespace creation")? {
        ForkResult::Child => {
            // Inner child: create the namespace and set up overlay
            drop(read_fd);
            let _ = inner_child_setup(config, &merged_dir, write_fd);
            // If we get here, something went wrong; exit
            std::process::exit(1);
        }
        ForkResult::Parent { child: helper_pid } => {
            // Inner parent (still in host namespace): persist and join
            drop(write_fd);
            inner_parent_persist(config, helper_pid, read_fd)?;
        }
    }

    Ok(())
}

/// Inner child: creates the mount namespace, mounts overlay, pivots root.
#[cfg(not(tarpaulin_include))]
fn inner_child_setup(config: &OverlayConfig, merged_dir: &Path, write_fd: OwnedFd) -> Result<()> {
    // 1. Create new mount namespace
    unshare(CloneFlags::CLONE_NEWNS).context("unshare CLONE_NEWNS")?;

    // 2. Make all existing mounts private (prevent propagation to host)
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_PRIVATE | MsFlags::MS_REC,
        None::<&str>,
    )
    .context("making mounts private")?;

    // 3. Mount overlay on the merged directory
    let opts = format!(
        "lowerdir=/,upperdir={},workdir={}",
        config.base_dir.join("upper").display(),
        config.base_dir.join("work").display(),
    );
    mount(
        Some("overlay"),
        merged_dir,
        Some("overlay"),
        MsFlags::empty(),
        Some(opts.as_str()),
    )
    .context("mounting overlay")?;

    // 4. Bind-mount special filesystems into the merged root.
    // ONLY kernel-backed filesystems (/proc, /sys, /dev) and /run (needed for
    // state file communication between daemon and shim) are bind-mounted.
    // /tmp is NOT bind-mounted — writes to /tmp go through the overlay upper
    // layer, protecting the host filesystem.
    for dir in &["proc", "sys", "dev", "run"] {
        let src = PathBuf::from("/").join(dir);
        let dst = merged_dir.join(dir);
        if src.exists() && src.is_dir() {
            fs::create_dir_all(&dst).ok();
            mount(
                Some(&src),
                &dst,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .with_context(|| format!("bind-mounting /{}", dir))?;
        }
    }

    // Copy resolver/hosts config into the namespace so workloads can override them if needed.
    copy_etc_files(Path::new("/etc"), &merged_dir.join("etc"));

    // 5. pivot_root to the merged overlay root
    let old_root = merged_dir.join("old_root");
    fs::create_dir_all(&old_root).context("creating old_root")?;

    nix::unistd::pivot_root(merged_dir, &old_root).context("pivot_root")?;

    // 6. Change to new root
    std::env::set_current_dir("/").context("chdir to new root")?;

    // 7. Unmount and remove old root
    umount2("/old_root", MntFlags::MNT_DETACH).context("unmounting old root")?;
    fs::remove_dir("/old_root").ok();

    // 7.5. Filter sensitive host paths
    let filter_config = read_filter_config();
    if let Err(e) = filter_sensitive_paths(&filter_config) {
        tracing::error!("filter: failed to filter sensitive paths: {:#}", e);
        // Non-fatal: log error but continue (graceful degradation)
    }

    // 8. Signal parent that namespace is ready
    let _ = nix::unistd::write(&write_fd, b"R");
    drop(write_fd);

    // 9. Sleep until parent kills us (namespace persists via bind-mount)
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Inner parent: persists the namespace via bind-mount, kills helper, joins namespace.
#[cfg(not(tarpaulin_include))]
fn inner_parent_persist(
    config: &OverlayConfig,
    helper_pid: nix::unistd::Pid,
    read_fd: OwnedFd,
) -> Result<()> {
    // 1. Wait for helper to signal namespace is ready
    let mut buf = [0u8; 1];
    let n = nix::unistd::read(read_fd.as_raw_fd(), &mut buf).context("reading from helper pipe")?;
    drop(read_fd);

    if n == 0 || buf[0] != b'R' {
        bail!("helper child failed to create namespace");
    }

    // 2. Persist namespace via bind-mount from HOST namespace
    let ns_source = format!("/proc/{}/ns/mnt", helper_pid);

    // Touch target file for bind-mount
    if let Some(parent) = config.ns_path.parent() {
        fs::create_dir_all(parent).context("creating ns dir")?;
    }
    fs::File::create(&config.ns_path).context("creating ns file")?;

    mount(
        Some(ns_source.as_str()),
        &config.ns_path,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .context("bind-mounting namespace")?;

    info!(
        "overlay: namespace persisted at {}",
        config.ns_path.display()
    );

    // 3. Keep helper alive as a namespace anchor so the mount namespace never
    //    disappears between workloads. The helper is sleeping in the namespace
    //    and keeps the mount tree referenced even if no containers are running.
    info!(
        "overlay: keeping helper pid={} alive to anchor shared mount namespace",
        helper_pid
    );

    // 4. Join the namespace ourselves
    join_namespace(&config.ns_path)?;

    Ok(())
}

/// Filter sensitive host paths by bind-mounting empty placeholders over them.
/// Called AFTER pivot_root, in the new mount namespace.
///
/// Tested by kind-integration tests (requires root + Linux namespaces).
#[cfg(not(tarpaulin_include))]
fn filter_sensitive_paths(config: &FilterConfig) -> Result<()> {
    if !config.enabled {
        info!("filter: sensitive file filtering disabled");
        return Ok(());
    }

    // Build filter list
    let mut paths = match config.mode {
        FilterMode::Append => {
            let mut p = get_default_filters();
            p.extend(config.custom_paths.clone());
            p
        }
        FilterMode::Replace => config.custom_paths.clone(),
    };

    // Apply allowlist (remove paths in allowlist)
    paths.retain(|p| !config.allowlist.contains(p));

    if paths.is_empty() {
        info!("filter: no paths to filter");
        return Ok(());
    }

    // Create filter directory
    fs::create_dir_all(&config.filter_dir).context("creating filter directory")?;

    // Filter each path
    let mut filtered_count = 0;
    for path in &paths {
        match filter_single_path(path, &config.filter_dir) {
            Ok(_) => {
                filtered_count += 1;
                tracing::debug!("filter: filtered {}", path.display());
            }
            Err(e) => {
                tracing::warn!("filter: failed to filter {}: {}", path.display(), e);
            }
        }
    }

    info!("filter: filtered {} sensitive paths", filtered_count);
    Ok(())
}

/// Filter a single path by bind-mounting an empty placeholder over it.
///
/// Tested by kind-integration tests (requires root + Linux namespaces).
#[cfg(not(tarpaulin_include))]
fn filter_single_path(path: &Path, filter_dir: &Path) -> Result<()> {
    // Skip if path doesn't exist on host
    if !path.exists() {
        tracing::debug!("filter: {} does not exist, skipping", path.display());
        return Ok(());
    }

    // Create placeholder (file or directory)
    let sanitized = path.to_string_lossy().replace('/', "_");
    let placeholder = filter_dir.join(sanitized);

    if path.is_dir() {
        fs::create_dir_all(&placeholder)
            .with_context(|| format!("creating placeholder dir for {}", path.display()))?;
    } else {
        fs::write(&placeholder, b"")
            .with_context(|| format!("creating placeholder file for {}", path.display()))?;
    }

    // Bind-mount placeholder over sensitive path
    mount(
        Some(&placeholder),
        path,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .with_context(|| format!("bind-mounting filter over {}", path.display()))?;

    Ok(())
}

// Copy a subset of /etc into the overlay namespace so workloads can edit resolver configuration.
fn copy_etc_files(src_etc: &Path, dst_etc: &Path) {
    if let Err(e) = fs::create_dir_all(dst_etc) {
        tracing::warn!("overlay: failed to create {}: {}", dst_etc.display(), e);
        return;
    }

    for name in ["resolv.conf", "hosts", "nsswitch.conf"] {
        let src = src_etc.join(name);
        let dst = dst_etc.join(name);

        if src.exists() {
            if let Err(e) = fs::copy(&src, &dst) {
                tracing::warn!("overlay: failed to copy {}: {}", src.display(), e);
            }
        } else {
            tracing::warn!(
                "overlay: host {} missing; functionality may be limited",
                src.display()
            );
        }
    }
}

// Read host resolver-related files so we can restore them into an existing namespace.
fn read_host_etc_files(src_etc: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    for name in ["resolv.conf", "hosts", "nsswitch.conf"] {
        let src = src_etc.join(name);
        if let Ok(data) = fs::read(&src) {
            files.push((name.to_string(), data));
        } else {
            tracing::warn!(
                "overlay: host {} missing or unreadable; functionality may be limited",
                src.display()
            );
        }
    }
    files
}

// Ensure /etc files inside the current namespace are present and non-empty; if empty/missing, restore from host copies.
fn ensure_etc_files_in_namespace(etc_dir: &Path, host_files: &[(String, Vec<u8>)]) {
    for (name, data) in host_files {
        let path = etc_dir.join(name);
        let needs_write = match fs::metadata(&path) {
            Ok(meta) => meta.len() == 0,
            Err(_) => true,
        };

        if needs_write {
            if let Err(e) = fs::create_dir_all(etc_dir) {
                tracing::warn!("overlay: failed to create {}: {}", etc_dir.display(), e);
                continue;
            }
            if let Err(e) = fs::write(&path, data) {
                tracing::warn!("overlay: failed to restore {}: {}", path.display(), e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_read_config_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::remove_var("REAPER_OVERLAY_BASE");
        std::env::remove_var("REAPER_OVERLAY_NS");
        std::env::remove_var("REAPER_OVERLAY_LOCK");

        let config = read_config();
        assert_eq!(config.base_dir, PathBuf::from("/run/reaper/overlay"));
        assert_eq!(config.ns_path, PathBuf::from("/run/reaper/shared-mnt-ns"));
        assert_eq!(config.lock_path, PathBuf::from("/run/reaper/overlay.lock"));
    }

    #[test]
    fn test_read_config_custom_base() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_OVERLAY_BASE", "/custom/overlay");
        let config = read_config();
        assert_eq!(config.base_dir, PathBuf::from("/custom/overlay"));
        std::env::remove_var("REAPER_OVERLAY_BASE");
        std::env::remove_var("REAPER_OVERLAY_NS");
        std::env::remove_var("REAPER_OVERLAY_LOCK");
    }

    #[test]
    fn test_namespace_exists_nonexistent() {
        assert!(!namespace_exists(Path::new("/nonexistent/path/ns")));
    }

    #[test]
    fn test_copy_etc_files_copies_expected_files() {
        let src_dir = tempfile::tempdir().expect("src tempdir");
        let src_etc = src_dir.path().join("etc");
        fs::create_dir_all(&src_etc).unwrap();

        let files = [
            ("resolv.conf", "nameserver 8.8.8.8\n"),
            ("hosts", "127.0.0.1 localhost\n"),
            ("nsswitch.conf", "hosts: files dns\n"),
        ];

        for (name, contents) in &files {
            fs::write(src_etc.join(name), contents).unwrap();
        }

        let dst_dir = tempfile::tempdir().expect("dst tempdir");
        let dst_etc = dst_dir.path().join("etc");

        super::copy_etc_files(&src_etc, &dst_etc);

        for (name, contents) in &files {
            let copied = fs::read_to_string(dst_etc.join(name)).unwrap();
            assert_eq!(copied, *contents);
        }
    }

    #[test]
    fn test_copy_etc_files_handles_missing_source_files() {
        // Source dir exists but contains no files — exercises the "missing" warning branch
        let src_dir = tempfile::tempdir().expect("src tempdir");
        let src_etc = src_dir.path().join("etc");
        fs::create_dir_all(&src_etc).unwrap();

        let dst_dir = tempfile::tempdir().expect("dst tempdir");
        let dst_etc = dst_dir.path().join("etc");

        super::copy_etc_files(&src_etc, &dst_etc);

        // Destination dir is created but no files are copied
        assert!(dst_etc.exists());
        assert!(!dst_etc.join("resolv.conf").exists());
        assert!(!dst_etc.join("hosts").exists());
        assert!(!dst_etc.join("nsswitch.conf").exists());
    }

    #[test]
    fn test_read_host_etc_files_reads_existing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let etc = dir.path().join("etc");
        fs::create_dir_all(&etc).unwrap();

        fs::write(etc.join("resolv.conf"), b"nameserver 1.1.1.1\n").unwrap();
        fs::write(etc.join("hosts"), b"127.0.0.1 localhost\n").unwrap();
        fs::write(etc.join("nsswitch.conf"), b"hosts: files dns\n").unwrap();

        let result = super::read_host_etc_files(&etc);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "resolv.conf");
        assert_eq!(result[0].1, b"nameserver 1.1.1.1\n");
        assert_eq!(result[1].0, "hosts");
        assert_eq!(result[2].0, "nsswitch.conf");
    }

    #[test]
    fn test_read_host_etc_files_handles_missing() {
        // Empty dir — none of the expected files exist, exercises the warning branch
        let dir = tempfile::tempdir().expect("tempdir");
        let etc = dir.path().join("etc");
        fs::create_dir_all(&etc).unwrap();

        let result = super::read_host_etc_files(&etc);
        assert!(result.is_empty());
    }

    #[test]
    fn test_read_host_etc_files_partial() {
        // Only one file present — exercises both the Ok and Err branches
        let dir = tempfile::tempdir().expect("tempdir");
        let etc = dir.path().join("etc");
        fs::create_dir_all(&etc).unwrap();

        fs::write(etc.join("hosts"), b"127.0.0.1 localhost\n").unwrap();

        let result = super::read_host_etc_files(&etc);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "hosts");
    }

    #[test]
    fn test_ensure_etc_files_restores_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let etc = dir.path().join("etc");
        // Don't create the dir — ensure_etc_files_in_namespace should create it

        let host_files = vec![
            ("resolv.conf".to_string(), b"nameserver 8.8.8.8\n".to_vec()),
            ("hosts".to_string(), b"127.0.0.1 localhost\n".to_vec()),
        ];

        super::ensure_etc_files_in_namespace(&etc, &host_files);

        assert_eq!(
            fs::read_to_string(etc.join("resolv.conf")).unwrap(),
            "nameserver 8.8.8.8\n"
        );
        assert_eq!(
            fs::read_to_string(etc.join("hosts")).unwrap(),
            "127.0.0.1 localhost\n"
        );
    }

    #[test]
    fn test_acquire_lock_creates_and_locks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("subdir").join("test.lock");

        // Lock file (and parent dir) don't exist yet
        assert!(!lock_path.exists());

        let lock = super::acquire_lock(&lock_path).unwrap();
        assert!(lock_path.exists());

        // Dropping the lock releases it
        drop(lock);
    }

    #[test]
    fn test_ensure_etc_files_restores_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let etc = dir.path().join("etc");
        fs::create_dir_all(&etc).unwrap();

        // Create an empty file — should be overwritten
        fs::write(etc.join("resolv.conf"), b"").unwrap();
        // Create a non-empty file — should be left alone
        fs::write(etc.join("hosts"), b"existing content\n").unwrap();

        let host_files = vec![
            ("resolv.conf".to_string(), b"nameserver 1.1.1.1\n".to_vec()),
            ("hosts".to_string(), b"127.0.0.1 localhost\n".to_vec()),
        ];

        super::ensure_etc_files_in_namespace(&etc, &host_files);

        // Empty file was restored
        assert_eq!(
            fs::read_to_string(etc.join("resolv.conf")).unwrap(),
            "nameserver 1.1.1.1\n"
        );
        // Non-empty file was left alone
        assert_eq!(
            fs::read_to_string(etc.join("hosts")).unwrap(),
            "existing content\n"
        );
    }

    #[test]
    fn test_read_filter_config_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::remove_var("REAPER_FILTER_ENABLED");
        std::env::remove_var("REAPER_FILTER_MODE");
        std::env::remove_var("REAPER_FILTER_PATHS");
        std::env::remove_var("REAPER_FILTER_ALLOWLIST");
        std::env::remove_var("REAPER_FILTER_DIR");

        let config = super::read_filter_config();
        assert!(config.enabled);
        assert_eq!(config.mode, super::FilterMode::Append);
        assert!(config.custom_paths.is_empty());
        assert!(config.allowlist.is_empty());
        assert_eq!(
            config.filter_dir,
            PathBuf::from("/run/reaper/overlay-filters")
        );
    }

    #[test]
    fn test_read_filter_config_disabled() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_FILTER_ENABLED", "false");
        let config = super::read_filter_config();
        assert!(!config.enabled);

        std::env::set_var("REAPER_FILTER_ENABLED", "0");
        let config = super::read_filter_config();
        assert!(!config.enabled);

        std::env::remove_var("REAPER_FILTER_ENABLED");
    }

    #[test]
    fn test_read_filter_config_custom_paths() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_FILTER_PATHS", "/custom/path:/another/path");

        let config = super::read_filter_config();
        assert_eq!(config.custom_paths.len(), 2);
        assert_eq!(config.custom_paths[0], PathBuf::from("/custom/path"));
        assert_eq!(config.custom_paths[1], PathBuf::from("/another/path"));

        std::env::remove_var("REAPER_FILTER_PATHS");
    }

    #[test]
    fn test_read_filter_config_replace_mode() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_FILTER_MODE", "replace");

        let config = super::read_filter_config();
        assert_eq!(config.mode, super::FilterMode::Replace);

        std::env::remove_var("REAPER_FILTER_MODE");
    }

    #[test]
    fn test_read_filter_config_allowlist() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_FILTER_ALLOWLIST", "/etc/shadow:/root/.ssh");

        let config = super::read_filter_config();
        assert_eq!(config.allowlist.len(), 2);
        assert_eq!(config.allowlist[0], PathBuf::from("/etc/shadow"));
        assert_eq!(config.allowlist[1], PathBuf::from("/root/.ssh"));

        std::env::remove_var("REAPER_FILTER_ALLOWLIST");
    }

    #[test]
    fn test_get_default_filters_not_empty() {
        let filters = super::get_default_filters();
        assert!(!filters.is_empty());
        assert!(filters.contains(&PathBuf::from("/etc/shadow")));
        assert!(filters.contains(&PathBuf::from("/root/.ssh")));
        assert!(filters.contains(&PathBuf::from("/etc/ssh/ssh_host_rsa_key")));
    }
}
