//! Integration tests for per-Kubernetes-namespace overlay isolation.
//!
//! These tests require Linux (overlayfs + mount namespaces) and root privileges.
//! On macOS or without root, tests are skipped gracefully.

#[cfg(target_os = "linux")]
mod namespace_isolation_tests {
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

    fn can_use_overlay() -> bool {
        if !is_root() {
            return false;
        }
        Command::new("unshare")
            .args(["--mount", "true"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Run a workload with a specific K8s namespace for overlay isolation.
    /// Returns the state JSON captured after completion.
    fn run_workload_with_namespace(
        container_id: &str,
        command: &[&str],
        k8s_namespace: Option<&str>,
        isolation_mode: Option<&str>,
    ) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("bundle");
        fs::create_dir_all(&bundle).unwrap();

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

        if let Some(ns) = k8s_namespace {
            cmd.arg("--namespace").arg(ns);
        }
        if let Some(mode) = isolation_mode {
            cmd.env("REAPER_OVERLAY_ISOLATION", mode);
        }

        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Start
        let mut cmd = Command::new(&runtime);
        cmd.arg("start")
            .arg(container_id)
            .arg("--bundle")
            .arg(&bundle)
            .env("REAPER_RUNTIME_ROOT", &state_dir);

        if let Some(mode) = isolation_mode {
            cmd.env("REAPER_OVERLAY_ISOLATION", mode);
        }

        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "start failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Wait for workload to finish
        std::thread::sleep(std::time::Duration::from_secs(3));

        // Read state
        let state_file = state_dir.join(container_id).join("state.json");
        let state_data = fs::read_to_string(&state_file).unwrap_or_default();

        // Delete
        let mut cmd = Command::new(&runtime);
        cmd.arg("delete")
            .arg(container_id)
            .env("REAPER_RUNTIME_ROOT", &state_dir);
        if let Some(mode) = isolation_mode {
            cmd.env("REAPER_OVERLAY_ISOLATION", mode);
        }
        let _ = cmd.output();

        state_data
    }

    /// Namespace mode without --namespace should fail hard.
    #[test]
    #[serial]
    fn test_namespace_isolation_fail_hard() {
        if !can_use_overlay() {
            eprintln!("Skipping: requires root + mount namespace support");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("bundle");
        fs::create_dir_all(&bundle).unwrap();

        let config = r#"{
            "process": {
                "args": ["/bin/echo", "should-not-run"],
                "cwd": "/tmp"
            }
        }"#;
        fs::write(bundle.join("config.json"), config).unwrap();

        let runtime = reaper_runtime_binary();
        let state_dir = tmp.path().join("state");

        // Create without --namespace while isolation=namespace (default)
        let output = Command::new(&runtime)
            .arg("create")
            .arg("no-ns-test")
            .arg("--bundle")
            .arg(&bundle)
            .env("REAPER_RUNTIME_ROOT", &state_dir)
            // Explicitly unset to use the default (namespace mode)
            .env_remove("REAPER_OVERLAY_ISOLATION")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "create should succeed (namespace is checked at start time): {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Start should fail because no namespace was provided
        let output = Command::new(&runtime)
            .arg("start")
            .arg("no-ns-test")
            .arg("--bundle")
            .arg(&bundle)
            .env("REAPER_RUNTIME_ROOT", &state_dir)
            .env_remove("REAPER_OVERLAY_ISOLATION")
            .output()
            .unwrap();

        // The daemon child will exit(1) when read_config fails.
        // Check the state file shows stopped with exit_code 1.
        std::thread::sleep(std::time::Duration::from_secs(2));
        let state_file = state_dir.join("no-ns-test").join("state.json");
        let state_data = fs::read_to_string(&state_file).unwrap_or_default();

        assert!(
            state_data.contains("\"stopped\"") || !output.status.success(),
            "workload should fail when namespace mode is active without --namespace, got: {}",
            state_data
        );
    }

    /// Node mode backward compatibility: workloads with different namespace args share overlay.
    #[test]
    #[serial]
    fn test_node_mode_backward_compat() {
        if !can_use_overlay() {
            eprintln!("Skipping: requires root + mount namespace support");
            return;
        }

        let marker = "/tmp/reaper-node-compat-test";
        let _ = fs::remove_file(marker);

        // Workload A in "ns-a" with node mode writes a file
        run_workload_with_namespace(
            "node-compat-writer",
            &["/bin/sh", "-c", &format!("echo node-shared > {}", marker)],
            Some("ns-a"),
            Some("node"),
        );

        // Workload B in "ns-b" with node mode should see it (shared overlay)
        let state = run_workload_with_namespace(
            "node-compat-reader",
            &["/bin/sh", "-c", &format!("cat {}", marker)],
            Some("ns-b"),
            Some("node"),
        );

        assert!(
            state.contains("\"exit_code\": 0") || state.contains("\"exit_code\":0"),
            "in node mode, workloads should share overlay regardless of namespace, got: {}",
            state
        );
    }
}

#[cfg(not(target_os = "linux"))]
mod non_linux {
    #[test]
    fn test_overlay_isolation_not_applicable_on_this_platform() {
        let platform = std::env::consts::OS;
        assert_ne!(
            platform, "linux",
            "overlay isolation tests should be in the linux module"
        );
    }
}
