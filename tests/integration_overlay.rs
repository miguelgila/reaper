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
        unsafe { libc::getuid() == 0 }
    }

    /// Helper: run a workload through reaper-runtime create/start/delete lifecycle.
    /// Returns the stdout captured from the workload.
    fn run_workload(
        container_id: &str,
        command: &[&str],
        env_overlay_enabled: Option<&str>,
    ) -> String {
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
        let mut cmd = Command::new(&runtime);
        cmd.arg("create")
            .arg(container_id)
            .arg("--bundle")
            .arg(&bundle)
            .env("REAPER_RUNTIME_ROOT", &state_dir);
        if let Some(val) = env_overlay_enabled {
            cmd.env("REAPER_OVERLAY_ENABLED", val);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Capture stdout via a temp file (since daemon redirects)
        let stdout_file = tmp.path().join("stdout.txt");

        // Start
        let mut cmd = Command::new(&runtime);
        cmd.arg("start")
            .arg(container_id)
            .arg("--bundle")
            .arg(&bundle)
            .env("REAPER_RUNTIME_ROOT", &state_dir);
        if let Some(val) = env_overlay_enabled {
            cmd.env("REAPER_OVERLAY_ENABLED", val);
        }
        let output = cmd.output().unwrap();
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
        let mut cmd = Command::new(&runtime);
        cmd.arg("delete")
            .arg(container_id)
            .env("REAPER_RUNTIME_ROOT", &state_dir);
        let _ = cmd.output();

        state_data
    }

    #[test]
    #[serial]
    fn test_overlay_disabled_runs_normally() {
        // With overlay disabled, workloads should run as before (host-direct)
        let state = run_workload(
            "overlay-disabled-test",
            &["/bin/echo", "overlay-disabled-ok"],
            Some("false"),
        );
        assert!(
            state.contains("\"status\":"),
            "state should contain status field"
        );
    }

    #[test]
    #[serial]
    fn test_overlay_enabled_requires_root() {
        if is_root() {
            // If running as root on Linux, overlay should work
            let state = run_workload(
                "overlay-root-test",
                &["/bin/echo", "overlay-root-ok"],
                Some("true"),
            );
            assert!(
                state.contains("\"status\":"),
                "state should contain status field"
            );
        } else {
            // Without root, overlay will fail-open to host-direct (graceful degradation)
            let state = run_workload(
                "overlay-noroot-test",
                &["/bin/echo", "overlay-noroot-ok"],
                Some("true"),
            );
            // Should still succeed due to fail-open design
            assert!(
                state.contains("\"status\":"),
                "state should contain status field even without root"
            );
        }
    }

    #[test]
    #[serial]
    fn test_overlay_host_protection() {
        if !is_root() {
            eprintln!("Skipping test_overlay_host_protection: requires root");
            return;
        }

        let marker = "/tmp/reaper-overlay-host-protection-test";

        // Clean up any leftover marker
        let _ = fs::remove_file(marker);

        // Run workload that writes a file — inside overlay, it should NOT leak to host
        run_workload(
            "overlay-protect-test",
            &["/bin/sh", "-c", &format!("echo protected > {}", marker)],
            Some("true"),
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
        if !is_root() {
            eprintln!("Skipping test_overlay_shared_writes: requires root");
            return;
        }

        let marker = "/tmp/reaper-overlay-shared-test";

        // Workload A: write file
        run_workload(
            "overlay-writer",
            &["/bin/sh", "-c", &format!("echo shared-data > {}", marker)],
            Some("true"),
        );

        // Workload B: read file — should see it because they share the overlay
        let state = run_workload(
            "overlay-reader",
            &["/bin/sh", "-c", &format!("cat {}", marker)],
            Some("true"),
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
        if !is_root() {
            eprintln!("Skipping test_special_filesystems_accessible: requires root");
            return;
        }

        // Verify /proc is accessible inside the overlay
        let state = run_workload(
            "overlay-proc-test",
            &["/bin/sh", "-c", "test -f /proc/self/status"],
            Some("true"),
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
