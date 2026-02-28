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
use nix::libc;
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{setns, unshare, CloneFlags};
use nix::unistd::{fork, ForkResult};

/// Overlay isolation mode: per-Kubernetes-namespace or node-wide (legacy).
#[derive(Debug, PartialEq)]
pub enum OverlayIsolation {
    /// Each K8s namespace gets its own overlay upper/work/ns/lock (default).
    Namespace,
    /// All workloads share a single overlay (legacy behavior).
    Node,
}

/// Read overlay isolation mode from config.
///
/// - `REAPER_OVERLAY_ISOLATION`: "namespace" (default) or "node"
pub fn read_isolation_mode() -> OverlayIsolation {
    std::env::var("REAPER_OVERLAY_ISOLATION")
        .map(|v| match v.to_ascii_lowercase().as_str() {
            "node" => OverlayIsolation::Node,
            _ => OverlayIsolation::Namespace,
        })
        .unwrap_or(OverlayIsolation::Namespace)
}

/// Validate that a Kubernetes namespace name is safe for use as a path component.
/// K8s namespaces follow DNS label rules: [a-z0-9][a-z0-9-]*[a-z0-9], max 63 chars.
fn validate_namespace_for_path(ns: &str) -> Result<()> {
    if ns.is_empty() {
        bail!("namespace must not be empty");
    }
    if ns.len() > 63 {
        bail!("namespace too long: {} chars (max 63)", ns.len());
    }
    if !ns
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!(
            "namespace contains invalid characters: {:?} (allowed: [a-z0-9-])",
            ns
        );
    }
    Ok(())
}

/// Overlay configuration, read from environment variables.
#[derive(Debug)]
pub struct OverlayConfig {
    /// Base directory for overlay upper/work dirs (default: /run/reaper/overlay)
    pub base_dir: PathBuf,
    /// Path to the persisted namespace bind-mount (default: /run/reaper/shared-mnt-ns)
    pub ns_path: PathBuf,
    /// Path to the file lock for namespace creation (default: /run/reaper/overlay.lock)
    pub lock_path: PathBuf,
    /// Directory for pivot_root merged view
    pub merged_dir: PathBuf,
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

/// DNS resolution mode for Reaper workloads.
#[derive(Debug, PartialEq)]
pub enum DnsMode {
    /// Use the host node's /etc/resolv.conf (default)
    Host,
    /// Use the kubelet-prepared resolv.conf pointing to CoreDNS
    Kubernetes,
}

/// DNS configuration, read from environment variables.
pub struct DnsConfig {
    /// Which DNS resolver to use
    pub mode: DnsMode,
}

/// Read DNS configuration from environment variables.
///
/// - `REAPER_DNS_MODE`: "host" (default), "kubernetes", or "k8s"
pub fn read_dns_config() -> DnsConfig {
    let mode = std::env::var("REAPER_DNS_MODE")
        .map(|v| match v.to_ascii_lowercase().as_str() {
            "kubernetes" | "k8s" => DnsMode::Kubernetes,
            _ => DnsMode::Host,
        })
        .unwrap_or(DnsMode::Host);

    DnsConfig { mode }
}

/// Apply Kubernetes DNS by writing the kubelet-prepared resolv.conf into the overlay.
///
/// Finds the `/etc/resolv.conf` mount in the OCI mounts array, reads its content
/// from the host namespace via `/proc/1/root/<source>`, and writes it as a regular
/// file into `/etc/resolv.conf` in the overlay. This avoids bind-mount stale mount
/// issues in the shared namespace.
///
/// Must be called AFTER entering the overlay namespace and AFTER applying volume
/// mounts. DNS failure is fatal when the admin explicitly opted into kubernetes DNS.
///
/// Tested by kind-integration tests (requires root + Linux namespaces + CoreDNS).
#[cfg(not(tarpaulin_include))]
pub fn apply_kubernetes_dns(oci_mounts: &[super::OciMount]) -> Result<()> {
    // Find the /etc/resolv.conf mount in the OCI config
    let resolv_mount = oci_mounts
        .iter()
        .find(|m| m.destination == "/etc/resolv.conf");

    let resolv_mount = match resolv_mount {
        Some(m) => m,
        None => {
            bail!(
                "dns: REAPER_DNS_MODE=kubernetes but no /etc/resolv.conf mount found in OCI config; \
                 kubelet may not have prepared per-pod DNS"
            );
        }
    };

    let source = resolv_mount.source.as_deref().unwrap_or("");
    if source.is_empty() {
        bail!("dns: /etc/resolv.conf mount has no source path in OCI config");
    }

    // Read content from host namespace via /proc/1/root/<source>
    let host_path = format!("/proc/1/root{}", source);
    let content = fs::read(&host_path).with_context(|| {
        format!(
            "dns: failed to read kubelet resolv.conf from {} (source: {})",
            host_path, source
        )
    })?;

    if content.is_empty() {
        bail!(
            "dns: kubelet resolv.conf at {} is empty; CoreDNS may not be running",
            host_path
        );
    }

    // Write into the overlay as a regular file (not a bind mount)
    fs::write("/etc/resolv.conf", &content)
        .context("dns: failed to write /etc/resolv.conf in overlay")?;

    info!(
        "dns: wrote kubelet resolv.conf ({} bytes) to /etc/resolv.conf",
        content.len()
    );

    Ok(())
}

/// Read overlay configuration from environment variables.
///
/// In `Namespace` isolation mode (default), paths are scoped per K8s namespace:
///   - base_dir: `/run/reaper/overlay/<ns>/`
///   - ns_path:  `/run/reaper/ns/<ns>`
///   - lock_path: `/run/reaper/overlay-<ns>.lock`
///   - merged_dir: `/run/reaper/merged/<ns>`
///
/// In `Node` isolation mode (legacy), paths use the old flat layout:
///   - base_dir: `/run/reaper/overlay`
///   - ns_path:  `/run/reaper/shared-mnt-ns`
///   - lock_path: `/run/reaper/overlay.lock`
///   - merged_dir: `/run/reaper/merged`
///
/// Environment overrides (`REAPER_OVERLAY_BASE`, `REAPER_OVERLAY_NS`,
/// `REAPER_OVERLAY_LOCK`) are respected in both modes as explicit overrides.
pub fn read_config(k8s_namespace: Option<&str>) -> Result<OverlayConfig> {
    let isolation = read_isolation_mode();

    match isolation {
        OverlayIsolation::Namespace => {
            let ns = k8s_namespace.ok_or_else(|| {
                anyhow::anyhow!(
                    "overlay isolation mode is 'namespace' but no --namespace was provided. \
                     Set REAPER_OVERLAY_ISOLATION=node or pass --namespace to the runtime."
                )
            })?;
            validate_namespace_for_path(ns)?;

            // Per-namespace paths under the standard /run/reaper/ tree.
            // Explicit env overrides take precedence.
            let base_dir = std::env::var("REAPER_OVERLAY_BASE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(format!("/run/reaper/overlay/{}", ns)));

            let ns_path = std::env::var("REAPER_OVERLAY_NS")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(format!("/run/reaper/ns/{}", ns)));

            let lock_path = std::env::var("REAPER_OVERLAY_LOCK")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(format!("/run/reaper/overlay-{}.lock", ns)));

            let merged_dir = PathBuf::from(format!("/run/reaper/merged/{}", ns));

            Ok(OverlayConfig {
                base_dir,
                ns_path,
                lock_path,
                merged_dir,
            })
        }
        OverlayIsolation::Node => {
            // Legacy flat layout — ignores namespace argument.
            let base_dir = std::env::var("REAPER_OVERLAY_BASE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/run/reaper/overlay"));

            let ns_path = std::env::var("REAPER_OVERLAY_NS")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/run/reaper/shared-mnt-ns"));

            let lock_path = std::env::var("REAPER_OVERLAY_LOCK")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/run/reaper/overlay.lock"));

            let merged_dir = PathBuf::from("/run/reaper/merged");

            Ok(OverlayConfig {
                base_dir,
                ns_path,
                lock_path,
                merged_dir,
            })
        }
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
    let merged_dir = config.merged_dir.clone();

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

/// System mount destinations that are already handled by overlay setup.
/// These are skipped when processing OCI volume mounts.
const SYSTEM_MOUNT_PREFIXES: &[&str] = &["/proc", "/sys", "/dev"];

/// Kubernetes-internal mounts that are handled by the kubelet/containerd
/// and should not be bind-mounted by the runtime.
const K8S_INTERNAL_MOUNTS: &[&str] = &[
    "/etc/hosts",
    "/etc/hostname",
    "/etc/resolv.conf",
    "/dev/termination-log",
];

/// Check if a mount entry is a bind mount.
/// A mount is considered a bind mount if its type is "bind" or if "bind" or "rbind"
/// appears in its options.
fn is_bind_mount(m: &super::OciMount) -> bool {
    if let Some(ref t) = m.mount_type {
        if t == "bind" {
            return true;
        }
    }
    m.options.iter().any(|o| o == "bind" || o == "rbind")
}

/// Check if a mount destination is a system path already handled by overlay.
fn is_system_destination(dest: &str) -> bool {
    for prefix in SYSTEM_MOUNT_PREFIXES {
        if dest == *prefix || dest.starts_with(&format!("{}/", prefix)) {
            return true;
        }
    }
    false
}

/// Check if a mount destination is a Kubernetes-internal mount.
fn is_k8s_internal(dest: &str) -> bool {
    K8S_INTERNAL_MOUNTS.contains(&dest)
}

/// Check if a mount should have read-only applied.
fn is_read_only(m: &super::OciMount) -> bool {
    m.options.iter().any(|o| o == "ro")
}

/// Filter OCI mounts to only those that should be processed as volume mounts.
/// Returns bind mounts that are not system or Kubernetes-internal destinations.
pub fn filter_volume_mounts(mounts: &[super::OciMount]) -> Vec<&super::OciMount> {
    mounts
        .iter()
        .filter(|m| {
            if !is_bind_mount(m) {
                info!("volume: skipping non-bind mount: {}", m.destination);
                return false;
            }
            if is_system_destination(&m.destination) {
                info!("volume: skipping system destination: {}", m.destination);
                return false;
            }
            if is_k8s_internal(&m.destination) {
                info!("volume: skipping k8s-internal mount: {}", m.destination);
                return false;
            }
            true
        })
        .collect()
}

/// Clone a mount from the host mount namespace into the current (overlay) namespace.
///
/// Uses open_tree(OPEN_TREE_CLONE) + move_mount() (Linux 5.2+) with namespace
/// switching. Mount-related syscalls cannot follow /proc/1/root/ magic links, so
/// we must temporarily setns() into the host mount namespace to resolve the path,
/// clone the mount, switch back to the overlay namespace, then attach it.
///
/// The `source` parameter is the path AS SEEN FROM THE HOST namespace (not
/// /proc/1/root-prefixed).
#[cfg(not(tarpaulin_include))]
fn cross_namespace_mount(source: &Path, dest: &Path) -> Result<()> {
    use std::ffi::CString;

    // Syscall numbers (same on x86_64 and aarch64 for Linux >= 5.2)
    const SYS_OPEN_TREE: libc::c_long = 428;
    const SYS_MOVE_MOUNT: libc::c_long = 429;

    // Flags for open_tree
    const OPEN_TREE_CLONE: libc::c_uint = 1;
    const OPEN_TREE_CLOEXEC: libc::c_uint = libc::O_CLOEXEC as libc::c_uint;
    const AT_RECURSIVE: libc::c_uint = 0x8000;

    // Flags for move_mount
    const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x00000004;

    let source_cstr = CString::new(source.as_os_str().as_encoded_bytes())
        .context("invalid source path for open_tree")?;
    let dest_cstr = CString::new(dest.as_os_str().as_encoded_bytes())
        .context("invalid dest path for move_mount")?;

    // Save the current (overlay) mount namespace so we can return to it
    let overlay_ns =
        fs::File::open("/proc/self/ns/mnt").context("opening overlay mount namespace fd")?;
    let host_ns = fs::File::open("/proc/1/ns/mnt").context("opening host mount namespace fd")?;

    // Step 1: Enter host mount namespace to resolve the source path
    setns(&host_ns, CloneFlags::CLONE_NEWNS).context("setns to host mount namespace")?;

    // Step 2: Clone the mount at source (now visible in host ns) into a detached fd
    let tree_fd = unsafe {
        libc::syscall(
            SYS_OPEN_TREE,
            libc::AT_FDCWD,
            source_cstr.as_ptr(),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_RECURSIVE,
        )
    };
    let open_tree_err = if tree_fd < 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };

    // Step 3: Return to overlay namespace (MUST happen even if open_tree failed)
    setns(&overlay_ns, CloneFlags::CLONE_NEWNS).context("setns back to overlay mount namespace")?;

    // Now check if open_tree succeeded
    if let Some(err) = open_tree_err {
        bail!("open_tree({}) failed: {}", source.display(), err);
    }

    // Step 4: Attach the detached mount to the destination in the overlay namespace
    let ret = unsafe {
        libc::syscall(
            SYS_MOVE_MOUNT,
            tree_fd as libc::c_int,
            c"".as_ptr(),
            libc::AT_FDCWD,
            dest_cstr.as_ptr(),
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    };

    // Close the tree fd regardless of move_mount result
    unsafe { libc::close(tree_fd as libc::c_int) };

    if ret < 0 {
        let err = std::io::Error::last_os_error();
        bail!(
            "move_mount({} -> {}) failed: {}",
            source.display(),
            dest.display(),
            err
        );
    }

    Ok(())
}

/// Apply volume mounts from OCI config inside the current mount namespace.
///
/// For each filtered bind mount:
/// 1. Creates the destination directory (or file) if it doesn't exist
/// 2. Performs a recursive bind mount from source to destination
/// 3. If "ro" is in options, remounts read-only
///
/// Must be called AFTER entering the overlay namespace and BEFORE spawning
/// the workload. Mount failures are fatal.
///
/// Tested by kind-integration tests (requires root + Linux namespaces).
#[cfg(not(tarpaulin_include))]
pub fn apply_volume_mounts(mounts: &[super::OciMount]) -> Result<()> {
    let volume_mounts = filter_volume_mounts(mounts);

    if volume_mounts.is_empty() {
        info!("volume: no volume mounts to apply");
        return Ok(());
    }

    info!("volume: applying {} volume mount(s)", volume_mounts.len());

    for m in &volume_mounts {
        let source = m.source.as_deref().unwrap_or("");
        let dest = &m.destination;

        if source.is_empty() {
            bail!("volume mount for {} has no source path", dest);
        }

        let dest_path = Path::new(dest);

        // Volume sources are prepared by kubelet as mounts in the host mount
        // namespace (tmpfs for projected/secret, bind for configmap/hostpath, etc.).
        // Inside the overlay namespace, the underlying directory structure from the
        // host root IS visible through the overlay lower layer, but any MOUNTS on
        // those directories (tmpfs, projected, etc.) are NOT visible because mount
        // propagation is set to MS_PRIVATE.
        //
        // Strategy: check if the source exists via /proc/1/root/<path> (host ns).
        // If so, use cross_namespace_mount() which setns's to the host ns, clones
        // the mount with open_tree(), returns to overlay ns, and attaches via
        // move_mount(). Falls back to direct bind mount for sources visible in the
        // overlay (e.g., emptyDir is a plain directory, not a mount).
        let host_path = PathBuf::from(format!("/proc/1/root{}", source));
        let direct_path = PathBuf::from(source);

        // Determine whether source needs cross-namespace mount
        let use_host_ns = host_path.exists();
        let source_path = PathBuf::from(source);

        if !use_host_ns && !direct_path.exists() {
            tracing::warn!(
                "volume: source {} does not exist (checked host ns and overlay), skipping mount to {}",
                source,
                dest
            );
            continue;
        }

        if use_host_ns {
            info!(
                "volume: source {} exists in host namespace, will use cross-ns mount",
                source
            );
        } else {
            info!(
                "volume: source {} exists directly in overlay, will use bind mount",
                source
            );
        }

        // Unmount any stale mount at the destination from a previous pod.
        // Volume mounts in the shared namespace persist after pod deletion,
        // referencing kubelet directories that no longer exist. Attempting to
        // move_mount on top of a stale mount fails with ENOENT because path
        // lookup traverses into the disconnected mount.
        if dest_path.exists() {
            match umount2(dest_path, MntFlags::MNT_DETACH) {
                Ok(()) => info!("volume: unmounted stale mount at {}", dest),
                Err(nix::errno::Errno::EINVAL) => {} // not a mount point, fine
                Err(e) => info!("volume: umount2({}) returned {} (continuing)", dest, e),
            }
        }

        // Create destination: directory if source is a directory, file otherwise.
        // Check via /proc/1/root if using host ns, otherwise direct.
        let check_path = if use_host_ns {
            &host_path
        } else {
            &direct_path
        };
        if check_path.is_dir() {
            fs::create_dir_all(dest_path)
                .with_context(|| format!("creating mount destination dir {}", dest))?;
        } else {
            // Source is a file — create parent dir and touch the file
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dir for {}", dest))?;
            }
            if !dest_path.exists() {
                fs::write(dest_path, b"")
                    .with_context(|| format!("creating mount destination file {}", dest))?;
            }
        }

        if use_host_ns {
            // Cross-namespace mount: setns to host, open_tree(CLONE), setns back, move_mount
            cross_namespace_mount(&source_path, dest_path)
                .with_context(|| format!("cross-ns mounting {} -> {}", source, dest))?;
        } else {
            // Direct bind mount within the overlay namespace
            mount(
                Some(source),
                dest_path,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .with_context(|| format!("bind-mounting {} -> {}", source, dest))?;
        }

        info!("volume: mounted {} -> {}", source, dest);

        // Apply read-only remount if requested
        if is_read_only(m) {
            mount(
                None::<&str>,
                dest_path,
                None::<&str>,
                MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
                None::<&str>,
            )
            .with_context(|| format!("remounting {} as read-only", dest))?;
            info!("volume: remounted {} as read-only", dest);
        }
    }

    info!("volume: all volume mounts applied successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_read_config_node_mode_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_OVERLAY_ISOLATION", "node");
        std::env::remove_var("REAPER_OVERLAY_BASE");
        std::env::remove_var("REAPER_OVERLAY_NS");
        std::env::remove_var("REAPER_OVERLAY_LOCK");

        let config = read_config(None).unwrap();
        assert_eq!(config.base_dir, PathBuf::from("/run/reaper/overlay"));
        assert_eq!(config.ns_path, PathBuf::from("/run/reaper/shared-mnt-ns"));
        assert_eq!(config.lock_path, PathBuf::from("/run/reaper/overlay.lock"));
        assert_eq!(config.merged_dir, PathBuf::from("/run/reaper/merged"));

        std::env::remove_var("REAPER_OVERLAY_ISOLATION");
    }

    #[test]
    fn test_read_config_node_mode_custom_base() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_OVERLAY_ISOLATION", "node");
        std::env::set_var("REAPER_OVERLAY_BASE", "/custom/overlay");
        let config = read_config(None).unwrap();
        assert_eq!(config.base_dir, PathBuf::from("/custom/overlay"));
        std::env::remove_var("REAPER_OVERLAY_BASE");
        std::env::remove_var("REAPER_OVERLAY_NS");
        std::env::remove_var("REAPER_OVERLAY_LOCK");
        std::env::remove_var("REAPER_OVERLAY_ISOLATION");
    }

    #[test]
    fn test_read_isolation_mode_defaults_to_namespace() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("REAPER_OVERLAY_ISOLATION");
        assert_eq!(read_isolation_mode(), OverlayIsolation::Namespace);
    }

    #[test]
    fn test_read_isolation_mode_node() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("REAPER_OVERLAY_ISOLATION", "node");
        assert_eq!(read_isolation_mode(), OverlayIsolation::Node);
        std::env::remove_var("REAPER_OVERLAY_ISOLATION");
    }

    #[test]
    fn test_read_config_namespace_mode_with_ns() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("REAPER_OVERLAY_ISOLATION");
        std::env::remove_var("REAPER_OVERLAY_BASE");
        std::env::remove_var("REAPER_OVERLAY_NS");
        std::env::remove_var("REAPER_OVERLAY_LOCK");

        let config = read_config(Some("default")).unwrap();
        assert_eq!(
            config.base_dir,
            PathBuf::from("/run/reaper/overlay/default")
        );
        assert_eq!(config.ns_path, PathBuf::from("/run/reaper/ns/default"));
        assert_eq!(
            config.lock_path,
            PathBuf::from("/run/reaper/overlay-default.lock")
        );
        assert_eq!(
            config.merged_dir,
            PathBuf::from("/run/reaper/merged/default")
        );
    }

    #[test]
    fn test_read_config_namespace_mode_no_ns_fails() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("REAPER_OVERLAY_ISOLATION");
        std::env::remove_var("REAPER_OVERLAY_BASE");
        std::env::remove_var("REAPER_OVERLAY_NS");
        std::env::remove_var("REAPER_OVERLAY_LOCK");

        let result = read_config(None);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("namespace"));
    }

    #[test]
    fn test_read_config_node_mode_ignores_namespace() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("REAPER_OVERLAY_ISOLATION", "node");
        std::env::remove_var("REAPER_OVERLAY_BASE");
        std::env::remove_var("REAPER_OVERLAY_NS");
        std::env::remove_var("REAPER_OVERLAY_LOCK");

        // Node mode returns flat paths regardless of namespace arg
        let config = read_config(Some("production")).unwrap();
        assert_eq!(config.base_dir, PathBuf::from("/run/reaper/overlay"));
        assert_eq!(config.ns_path, PathBuf::from("/run/reaper/shared-mnt-ns"));

        std::env::remove_var("REAPER_OVERLAY_ISOLATION");
    }

    #[test]
    fn test_validate_namespace_for_path() {
        assert!(validate_namespace_for_path("default").is_ok());
        assert!(validate_namespace_for_path("kube-system").is_ok());
        assert!(validate_namespace_for_path("my-app-123").is_ok());
        assert!(validate_namespace_for_path("").is_err());
        assert!(validate_namespace_for_path("My-App").is_err()); // uppercase
        assert!(validate_namespace_for_path("ns/evil").is_err()); // path traversal
        assert!(validate_namespace_for_path("ns..evil").is_err()); // dots
        let long = "a".repeat(64);
        assert!(validate_namespace_for_path(&long).is_err());
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

    // --- Volume mount filtering tests ---

    fn make_mount(
        destination: &str,
        source: Option<&str>,
        mount_type: Option<&str>,
        options: &[&str],
    ) -> super::super::OciMount {
        super::super::OciMount {
            destination: destination.to_string(),
            source: source.map(|s| s.to_string()),
            mount_type: mount_type.map(|t| t.to_string()),
            options: options.iter().map(|o| o.to_string()).collect(),
        }
    }

    #[test]
    fn test_is_bind_mount_by_type() {
        let m = make_mount("/data", Some("/host/data"), Some("bind"), &[]);
        assert!(super::is_bind_mount(&m));
    }

    #[test]
    fn test_is_bind_mount_by_option_bind() {
        let m = make_mount("/data", Some("/host/data"), None, &["bind", "ro"]);
        assert!(super::is_bind_mount(&m));
    }

    #[test]
    fn test_is_bind_mount_by_option_rbind() {
        let m = make_mount("/data", Some("/host/data"), None, &["rbind", "ro"]);
        assert!(super::is_bind_mount(&m));
    }

    #[test]
    fn test_is_not_bind_mount_proc() {
        let m = make_mount("/proc", Some("proc"), Some("proc"), &[]);
        assert!(!super::is_bind_mount(&m));
    }

    #[test]
    fn test_is_not_bind_mount_tmpfs() {
        let m = make_mount("/dev", Some("tmpfs"), Some("tmpfs"), &[]);
        assert!(!super::is_bind_mount(&m));
    }

    #[test]
    fn test_is_system_destination() {
        assert!(super::is_system_destination("/proc"));
        assert!(super::is_system_destination("/proc/sys"));
        assert!(super::is_system_destination("/sys"));
        assert!(super::is_system_destination("/sys/fs/cgroup"));
        assert!(super::is_system_destination("/dev"));
        assert!(super::is_system_destination("/dev/pts"));
        assert!(!super::is_system_destination("/data"));
        assert!(!super::is_system_destination("/scripts"));
        assert!(!super::is_system_destination("/var/data"));
    }

    #[test]
    fn test_is_k8s_internal() {
        assert!(super::is_k8s_internal("/etc/hosts"));
        assert!(super::is_k8s_internal("/etc/hostname"));
        assert!(super::is_k8s_internal("/etc/resolv.conf"));
        assert!(super::is_k8s_internal("/dev/termination-log"));
        assert!(!super::is_k8s_internal("/scripts"));
        assert!(!super::is_k8s_internal("/etc/config"));
    }

    #[test]
    fn test_is_read_only() {
        let m_ro = make_mount("/data", Some("/host"), Some("bind"), &["rbind", "ro"]);
        assert!(super::is_read_only(&m_ro));

        let m_rw = make_mount("/data", Some("/host"), Some("bind"), &["rbind", "rw"]);
        assert!(!super::is_read_only(&m_rw));

        let m_empty = make_mount("/data", Some("/host"), Some("bind"), &[]);
        assert!(!super::is_read_only(&m_empty));
    }

    #[test]
    fn test_filter_volume_mounts_selects_bind_only() {
        let mounts = vec![
            make_mount("/proc", Some("proc"), Some("proc"), &[]),
            make_mount("/dev", Some("tmpfs"), Some("tmpfs"), &[]),
            make_mount(
                "/scripts",
                Some("/var/lib/kubelet/pods/abc/scripts"),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/data",
                Some("/var/lib/kubelet/pods/abc/data"),
                None,
                &["rbind"],
            ),
        ];

        let filtered = super::filter_volume_mounts(&mounts);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].destination, "/scripts");
        assert_eq!(filtered[1].destination, "/data");
    }

    #[test]
    fn test_filter_volume_mounts_skips_system_destinations() {
        let mounts = vec![
            make_mount("/proc", Some("/proc"), Some("bind"), &["rbind"]),
            make_mount(
                "/sys/fs/cgroup",
                Some("/sys/fs/cgroup"),
                Some("bind"),
                &["rbind"],
            ),
            make_mount("/dev/pts", Some("/dev/pts"), None, &["bind"]),
            make_mount(
                "/app/config",
                Some("/host/config"),
                Some("bind"),
                &["rbind"],
            ),
        ];

        let filtered = super::filter_volume_mounts(&mounts);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].destination, "/app/config");
    }

    #[test]
    fn test_filter_volume_mounts_skips_k8s_internal() {
        let mounts = vec![
            make_mount(
                "/etc/hosts",
                Some("/var/lib/containerd/hosts"),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/etc/hostname",
                Some("/var/lib/containerd/hostname"),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/etc/resolv.conf",
                Some("/var/lib/containerd/resolv.conf"),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/dev/termination-log",
                Some("/var/lib/containerd/termination-log"),
                Some("bind"),
                &["rbind"],
            ),
            make_mount(
                "/scripts",
                Some("/var/lib/kubelet/pods/abc/scripts"),
                Some("bind"),
                &["rbind", "ro"],
            ),
        ];

        let filtered = super::filter_volume_mounts(&mounts);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].destination, "/scripts");
    }

    #[test]
    fn test_filter_volume_mounts_empty_input() {
        let filtered = super::filter_volume_mounts(&[]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_volume_mounts_realistic_k8s_config() {
        // Simulates a real Kubernetes config.json mounts array
        let mounts = vec![
            make_mount(
                "/proc",
                Some("proc"),
                Some("proc"),
                &["nosuid", "noexec", "nodev"],
            ),
            make_mount(
                "/dev",
                Some("tmpfs"),
                Some("tmpfs"),
                &["nosuid", "strictatime", "mode=755", "size=65536k"],
            ),
            make_mount(
                "/dev/pts",
                Some("devpts"),
                Some("devpts"),
                &["nosuid", "noexec", "newinstance"],
            ),
            make_mount(
                "/dev/mqueue",
                Some("mqueue"),
                Some("mqueue"),
                &["nosuid", "noexec", "nodev"],
            ),
            make_mount(
                "/sys",
                Some("sysfs"),
                Some("sysfs"),
                &["nosuid", "noexec", "nodev", "ro"],
            ),
            make_mount(
                "/sys/fs/cgroup",
                Some("cgroup"),
                Some("cgroup"),
                &["nosuid", "noexec", "nodev", "ro"],
            ),
            make_mount(
                "/etc/hosts",
                Some("/var/lib/containerd/io.containerd.grpc.v1.cri/sandboxes/abc/hostname/hosts"),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/etc/hostname",
                Some(
                    "/var/lib/containerd/io.containerd.grpc.v1.cri/sandboxes/abc/hostname/hostname",
                ),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/etc/resolv.conf",
                Some("/var/lib/containerd/io.containerd.grpc.v1.cri/sandboxes/abc/resolv.conf"),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/dev/termination-log",
                Some("/var/lib/kubelet/pods/abc/containers/test/termination-log"),
                Some("bind"),
                &["rbind"],
            ),
            // User-defined volume mounts
            make_mount(
                "/scripts",
                Some("/var/lib/kubelet/pods/abc/volumes/kubernetes.io~configmap/scripts"),
                Some("bind"),
                &["rbind", "ro"],
            ),
            make_mount(
                "/data",
                Some("/var/lib/kubelet/pods/abc/volumes/kubernetes.io~empty-dir/data"),
                Some("bind"),
                &["rbind"],
            ),
            // Service account token
            make_mount(
                "/var/run/secrets/kubernetes.io/serviceaccount",
                Some("/var/lib/kubelet/pods/abc/volumes/kubernetes.io~projected/kube-api-access"),
                Some("bind"),
                &["rbind", "ro"],
            ),
        ];

        let filtered = super::filter_volume_mounts(&mounts);
        assert_eq!(filtered.len(), 3);
        assert_eq!(filtered[0].destination, "/scripts");
        assert_eq!(filtered[1].destination, "/data");
        assert_eq!(
            filtered[2].destination,
            "/var/run/secrets/kubernetes.io/serviceaccount"
        );
    }

    // --- DNS configuration tests ---

    #[test]
    fn test_read_dns_config_default_is_host() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("REAPER_DNS_MODE");

        let config = super::read_dns_config();
        assert_eq!(config.mode, super::DnsMode::Host);
    }

    #[test]
    fn test_read_dns_config_kubernetes() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_DNS_MODE", "kubernetes");
        let config = super::read_dns_config();
        assert_eq!(config.mode, super::DnsMode::Kubernetes);

        std::env::remove_var("REAPER_DNS_MODE");
    }

    #[test]
    fn test_read_dns_config_k8s_shorthand() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_DNS_MODE", "k8s");
        let config = super::read_dns_config();
        assert_eq!(config.mode, super::DnsMode::Kubernetes);

        std::env::remove_var("REAPER_DNS_MODE");
    }

    #[test]
    fn test_read_dns_config_case_insensitive() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_DNS_MODE", "Kubernetes");
        let config = super::read_dns_config();
        assert_eq!(config.mode, super::DnsMode::Kubernetes);

        std::env::set_var("REAPER_DNS_MODE", "K8S");
        let config = super::read_dns_config();
        assert_eq!(config.mode, super::DnsMode::Kubernetes);

        std::env::remove_var("REAPER_DNS_MODE");
    }

    #[test]
    fn test_read_dns_config_unknown_value_defaults_to_host() {
        let _guard = ENV_LOCK.lock().unwrap();

        std::env::set_var("REAPER_DNS_MODE", "invalid");
        let config = super::read_dns_config();
        assert_eq!(config.mode, super::DnsMode::Host);

        std::env::set_var("REAPER_DNS_MODE", "host");
        let config = super::read_dns_config();
        assert_eq!(config.mode, super::DnsMode::Host);

        std::env::remove_var("REAPER_DNS_MODE");
    }
}
