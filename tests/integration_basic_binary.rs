use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Test basic binary execution: run `echo "hello world"` through reaper-runtime
#[test]
fn test_run_echo_hello_world() {
    // Setup temp directory for the bundle
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create config.json with echo command
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "hello world"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin:/usr/local/bin"]
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    // Set state root to temp dir for isolation
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    // Get the reaper-runtime binary (built during test)
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create container
    let create_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-echo")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run create command");

    assert!(
        create_output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );

    // Parse and verify create response contains the container ID
    let create_stdout = String::from_utf8_lossy(&create_output.stdout);
    assert!(
        create_stdout.contains("test-echo"),
        "create output should contain container ID"
    );

    // Start container
    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-echo")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run start command");

    assert!(
        start_output.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start_output.stderr)
    );

    // Verify start output contains "started pid="
    let start_stdout = String::from_utf8_lossy(&start_output.stdout);
    assert!(
        start_stdout.contains("started pid="),
        "start output should contain 'started pid=', got: {}",
        start_stdout
    );

    // Extract PID from start output
    let pid_str = start_stdout
        .split("started pid=")
        .nth(1)
        .and_then(|s| s.split('\n').next())
        .expect("Failed to extract PID");
    let pid: i32 = pid_str.trim().parse().expect("Failed to parse PID as i32");

    assert!(pid > 0, "PID should be positive, got {}", pid);

    // Query state to verify container state
    // Note: echo is very fast, so the container may already be "stopped" by the time we check
    // We'll poll for a bit to see if it's still running, but accept either "running" or "stopped"
    let state_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("state")
        .arg("test-echo")
        .output()
        .expect("Failed to run state command");

    assert!(
        state_output.status.success(),
        "state failed: {}",
        String::from_utf8_lossy(&state_output.stderr)
    );

    // Parse state JSON
    let state_json = String::from_utf8_lossy(&state_output.stdout);
    let state: serde_json::Value =
        serde_json::from_str(&state_json).expect("Failed to parse state JSON");

    assert_eq!(state["id"], "test-echo", "Container ID mismatch");

    // Container status should be either "running" or "stopped" (echo is fast)
    let status = state["status"].as_str().expect("status should be a string");
    assert!(
        status == "running" || status == "stopped",
        "Container status should be 'running' or 'stopped', got: {}",
        status
    );

    assert_eq!(state["pid"], pid, "PID in state should match start output");

    // If stopped, verify exit code is 0
    if status == "stopped" {
        let exit_code = state["exit_code"].as_i64();
        assert_eq!(exit_code, Some(0), "echo should exit with code 0");
    }

    // Delete container (cleanup)
    let delete_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("delete")
        .arg("test-echo")
        .output()
        .expect("Failed to run delete command");

    assert!(
        delete_output.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&delete_output.stderr)
    );

    // Verify state directory is cleaned up
    let container_dir = PathBuf::from(&state_root).join("test-echo");
    assert!(
        !container_dir.exists(),
        "Container state directory should be deleted"
    );
}

/// Test running a shell script that produces output
#[test]
fn test_run_shell_script() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create config.json with shell command that outputs multiple lines
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", "echo 'line 1'; echo 'line 2'; echo 'line 3'"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin"]
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create and start
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-script")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to create");

    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-script")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    assert!(start_output.status.success());

    // Cleanup
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("delete")
        .arg("test-script")
        .output()
        .expect("Failed to delete");
}

/// Test that invalid bundle fails gracefully
#[test]
fn test_invalid_bundle() {
    let bundle_path = PathBuf::from("/tmp/nonexistent-bundle-12345");
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create should succeed (just stores metadata)
    let _create_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-invalid")
        .arg("--bundle")
        .arg(&bundle_path)
        .output()
        .expect("Failed to run create command");

    // But start will fail due to missing config.json
    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-invalid")
        .arg("--bundle")
        .arg(&bundle_path)
        .output()
        .expect("Failed to run start command");

    // Should fail because config.json doesn't exist
    assert!(
        !start_output.status.success(),
        "start should fail for missing config.json"
    );
}
