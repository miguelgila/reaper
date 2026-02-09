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

/// Read overlay configuration from environment variables.
///
/// - `REAPER_OVERLAY_BASE`: base dir for overlay (default: "/run/reaper/overlay")
pub fn read_config() -> OverlayConfig {
    let base_dir = std::env::var("REAPER_OVERLAY_BASE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/reaper/overlay"));

    let ns_path = PathBuf::from("/run/reaper/shared-mnt-ns");
    let lock_path = PathBuf::from("/run/reaper/overlay.lock");

    OverlayConfig {
        base_dir,
        ns_path,
        lock_path,
    }
}

/// Join or create the shared overlay namespace.
///
/// Must be called AFTER `setsid()` and BEFORE `Command::new()` in the
/// monitoring daemon child process.
///
/// Overlay is mandatory — if this fails, the workload must not run.
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
    ensure_etc_files_in_namespace(&host_etc);

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

    // 8. Signal parent that namespace is ready
    let _ = nix::unistd::write(&write_fd, b"R");
    drop(write_fd);

    // 9. Sleep until parent kills us (namespace persists via bind-mount)
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Inner parent: persists the namespace via bind-mount, kills helper, joins namespace.
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
fn ensure_etc_files_in_namespace(host_files: &[(String, Vec<u8>)]) {
    for (name, data) in host_files {
        let path = Path::new("/etc").join(name);
        let needs_write = match fs::metadata(&path) {
            Ok(meta) => meta.len() == 0,
            Err(_) => true,
        };

        if needs_write {
            if let Err(e) = fs::create_dir_all(Path::new("/etc")) {
                tracing::warn!("overlay: failed to create /etc: {}", e);
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
    use tempfile;

    #[test]
    fn test_read_config_defaults() {
        std::env::remove_var("REAPER_OVERLAY_BASE");

        let config = read_config();
        assert_eq!(config.base_dir, PathBuf::from("/run/reaper/overlay"));
        assert_eq!(config.ns_path, PathBuf::from("/run/reaper/shared-mnt-ns"));
        assert_eq!(config.lock_path, PathBuf::from("/run/reaper/overlay.lock"));
    }

    #[test]
    fn test_read_config_custom_base() {
        std::env::set_var("REAPER_OVERLAY_BASE", "/custom/overlay");
        let config = read_config();
        assert_eq!(config.base_dir, PathBuf::from("/custom/overlay"));
        std::env::remove_var("REAPER_OVERLAY_BASE");
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
}
