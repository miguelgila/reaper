//! Integration tests for the overlay namespace feature.
//!
//! These tests require Linux (overlayfs + mount namespaces) and root privileges.
//! On macOS or without root, tests are skipped gracefully.

#[cfg(target_os = "linux")]
mod overlay_tests {
    use serial_test::serial;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn reaper_runtime_binary() -> PathBuf {
        let mut path = std::env::current_exe().unwrap();
        path.pop(); // remove test binary name
        path.pop(); // remove deps/
        path.push("reaper-runtime");
        path
    }

    fn is_root() -> bool {
        nix::unistd::getuid().is_root()
    }

    /// Check if we can actually create mount namespaces (requires CAP_SYS_ADMIN).
    /// Being root inside a Docker container without --privileged is not enough.
    fn can_use_overlay() -> bool {
        if !is_root() {
            return false;
        }
        // Try to unshare a mount namespace — this is the minimal capability test.
        // If the kernel denies it (e.g. unprivileged Docker container), overlay won't work.
        use std::process::Command;
        Command::new("unshare")
            .args(["--mount", "true"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Helper: run a workload through reaper-runtime create/start/delete lifecycle.
    /// Returns the state JSON captured from the workload.
    fn run_workload(container_id: &str, command: &[&str]) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("bundle");
        fs::create_dir_all(&bundle).unwrap();

        // Create config.json
        let args_json: Vec<String> = command.iter().map(|s| format!("\"{}\"", s)).collect();
        let config = format!(
            r#"{{
                "process": {{
                    "args": [{}],
                    "cwd": "/tmp",
                    "env": ["PATH=/usr/bin:/bin:/usr/local/bin"]
                }}
            }}"#,
            args_json.join(", ")
        );
        fs::write(bundle.join("config.json"), &config).unwrap();

        let runtime = reaper_runtime_binary();
        let state_dir = tmp.path().join("state");

        // Create
        let output = Command::new(&runtime)
            .arg("create")
            .arg(container_id)
            .arg("--bundle")
            .arg(&bundle)
            .env("REAPER_RUNTIME_ROOT", &state_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Start
        let output = Command::new(&runtime)
            .arg("start")
            .arg(container_id)
            .arg("--bundle")
            .arg(&bundle)
            .env("REAPER_RUNTIME_ROOT", &state_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "start failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Wait for workload to finish
        std::thread::sleep(std::time::Duration::from_secs(3));

        // Read state for exit code
        let state_file = state_dir.join(container_id).join("state.json");
        let state_data = fs::read_to_string(&state_file).unwrap_or_default();

        // Delete
        let _ = Command::new(&runtime)
            .arg("delete")
            .arg(container_id)
            .env("REAPER_RUNTIME_ROOT", &state_dir)
            .output();

        state_data
    }

    #[test]
    #[serial]
    fn test_overlay_requires_root() {
        if !can_use_overlay() {
            eprintln!(
                "Skipping test_overlay_requires_root: requires root + mount namespace support"
            );
            return;
        }

        let state = run_workload("overlay-root-test", &["/bin/echo", "overlay-root-ok"]);
        assert!(
            state.contains("\"status\":"),
            "state should contain status field"
        );
    }

    #[test]
    #[serial]
    fn test_overlay_host_protection() {
        if !can_use_overlay() {
            eprintln!(
                "Skipping test_overlay_host_protection: requires root + mount namespace support"
            );
            return;
        }

        let marker = "/tmp/reaper-overlay-host-protection-test";

        // Clean up any leftover marker
        let _ = fs::remove_file(marker);

        // Run workload that writes a file — inside overlay, it should NOT leak to host
        run_workload(
            "overlay-protect-test",
            &["/bin/sh", "-c", &format!("echo protected > {}", marker)],
        );

        // The marker should NOT exist on the host filesystem
        assert!(
            !std::path::Path::new(marker).exists(),
            "file written inside overlay leaked to host filesystem"
        );
    }

    #[test]
    #[serial]
    fn test_overlay_shared_writes() {
        if !can_use_overlay() {
            eprintln!(
                "Skipping test_overlay_shared_writes: requires root + mount namespace support"
            );
            return;
        }

        let marker = "/tmp/reaper-overlay-shared-test";

        // Workload A: write file
        run_workload(
            "overlay-writer",
            &["/bin/sh", "-c", &format!("echo shared-data > {}", marker)],
        );

        // Workload B: read file — should see it because they share the overlay
        let state = run_workload(
            "overlay-reader",
            &["/bin/sh", "-c", &format!("cat {}", marker)],
        );

        // If the shared namespace works, workload B finds the file and exits 0
        assert!(
            state.contains("\"exit_code\": 0") || state.contains("\"exit_code\":0"),
            "reader workload should exit 0 (found the shared file), got: {}",
            state
        );
    }

    #[test]
    #[serial]
    fn test_special_filesystems_accessible() {
        if !can_use_overlay() {
            eprintln!("Skipping test_special_filesystems_accessible: requires root + mount namespace support");
            return;
        }

        // Verify /proc is accessible inside the overlay
        let state = run_workload(
            "overlay-proc-test",
            &["/bin/sh", "-c", "test -f /proc/self/status"],
        );
        assert!(
            state.contains("\"exit_code\": 0") || state.contains("\"exit_code\":0"),
            "/proc/self/status should be accessible inside overlay, got: {}",
            state
        );
    }
}

// On non-Linux, include a single test that confirms the module compiles
#[cfg(not(target_os = "linux"))]
mod non_linux {
    #[test]
    fn test_overlay_not_applicable_on_this_platform() {
        // Overlay is Linux-only; this test confirms the test file compiles on other platforms
        let platform = std::env::consts::OS;
        assert_ne!(
            platform, "linux",
            "overlay tests should be in the linux module"
        );
    }
}
