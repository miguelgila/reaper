use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Test that reaper-runtime loads configuration from a config file via REAPER_CONFIG.
/// The config file sets REAPER_RUNTIME_ROOT; the binary should use it without
/// needing an explicit env var.
#[test]
fn test_config_file_sets_runtime_root() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "config-test"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin:/usr/local/bin"]
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    // Create a custom state root
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    // Write a reaper.conf that sets REAPER_RUNTIME_ROOT
    let conf_dir = TempDir::new().expect("Failed to create conf dir");
    let conf_path = conf_dir.path().join("reaper.conf");
    fs::write(
        &conf_path,
        format!(
            "# Test config\nREAPER_RUNTIME_ROOT={}\nREAPER_NO_OVERLAY=1\n",
            state_root
        ),
    )
    .expect("Failed to write reaper.conf");

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create — only REAPER_CONFIG is set, NOT REAPER_RUNTIME_ROOT directly
    let create_output = Command::new(reaper_bin)
        .env("REAPER_CONFIG", conf_path.to_str().unwrap())
        .env_remove("REAPER_RUNTIME_ROOT")
        .env_remove("REAPER_NO_OVERLAY")
        .arg("create")
        .arg("test-conf")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run create command");

    assert!(
        create_output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );

    // Verify state was written to the config-file-specified root
    let state_file = state_dir.path().join("test-conf").join("state.json");
    assert!(
        state_file.exists(),
        "State file should exist at config-file-specified root: {:?}",
        state_file
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_CONFIG", conf_path.to_str().unwrap())
        .env_remove("REAPER_RUNTIME_ROOT")
        .env_remove("REAPER_NO_OVERLAY")
        .arg("delete")
        .arg("test-conf")
        .output();
}

/// Test that environment variables override config file values.
#[test]
fn test_env_var_overrides_config_file() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "override-test"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin:/usr/local/bin"]
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    // Config file points to one directory
    let conf_state_dir = TempDir::new().expect("Failed to create conf state dir");
    let conf_state_root = conf_state_dir.path().to_string_lossy().to_string();

    // Env var points to a different directory
    let env_state_dir = TempDir::new().expect("Failed to create env state dir");
    let env_state_root = env_state_dir.path().to_string_lossy().to_string();

    let conf_dir = TempDir::new().expect("Failed to create conf dir");
    let conf_path = conf_dir.path().join("reaper.conf");
    fs::write(
        &conf_path,
        format!(
            "REAPER_RUNTIME_ROOT={}\nREAPER_NO_OVERLAY=1\n",
            conf_state_root
        ),
    )
    .expect("Failed to write reaper.conf");

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Set both REAPER_CONFIG and REAPER_RUNTIME_ROOT — env var should win
    let create_output = Command::new(reaper_bin)
        .env("REAPER_CONFIG", conf_path.to_str().unwrap())
        .env("REAPER_RUNTIME_ROOT", &env_state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-override")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run create command");

    assert!(
        create_output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );

    // State should be in the env-var directory, NOT the config-file directory
    let env_state_file = env_state_dir
        .path()
        .join("test-override")
        .join("state.json");
    let conf_state_file = conf_state_dir
        .path()
        .join("test-override")
        .join("state.json");

    assert!(
        env_state_file.exists(),
        "State should be in env-var root: {:?}",
        env_state_file
    );
    assert!(
        !conf_state_file.exists(),
        "State should NOT be in config-file root: {:?}",
        conf_state_file
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &env_state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-override")
        .output();
}

/// Test that a missing config file is silently ignored (no error).
#[test]
fn test_missing_config_file_is_silent() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "no-config-test"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin:/usr/local/bin"]
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Point REAPER_CONFIG to a nonexistent file — should not cause an error
    let create_output = Command::new(reaper_bin)
        .env("REAPER_CONFIG", "/tmp/nonexistent-reaper-config-12345.conf")
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-noconf")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run create command");

    assert!(
        create_output.status.success(),
        "create should succeed even with missing config file: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-noconf")
        .output();
}
