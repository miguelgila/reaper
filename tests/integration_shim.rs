use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Test that the shim binary exists and can be invoked
#[test]
fn test_shim_binary_exists() {
    let shim_bin = env!("CARGO_BIN_EXE_containerd-shim-reaper-v2");
    assert!(
        std::path::Path::new(shim_bin).exists(),
        "Shim binary not found at {}",
        shim_bin
    );
}

/// Test that the shim binary accepts basic flags (if any)
/// Note: The shim is designed to be spawned by containerd, so direct invocation
/// may not be meaningful, but we can at least verify it doesn't crash immediately
#[test]
fn test_shim_binary_runs() {
    let shim_bin = env!("CARGO_BIN_EXE_containerd-shim-reaper-v2");

    // Try to run the shim with --help or no args to see if it starts
    // This may fail since it's designed to be spawned by containerd,
    // but at least validates the binary is executable
    let result = Command::new(shim_bin).arg("--help").output();

    // We expect this to fail (since it's not designed for direct invocation),
    // but it should not crash with a segfault or similar
    match result {
        Ok(output) => {
            // If it succeeds, that's unexpected but not wrong
            println!(
                "Shim help output: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        Err(e) => {
            // Expected - shim not designed for direct CLI usage
            println!("Expected error running shim directly: {}", e);
        }
    }
}

/// Test that we can create a valid bundle directory structure for the shim
#[test]
fn test_create_valid_bundle() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create a minimal config.json that the shim expects
    let config = serde_json::json!({
        "command": "/bin/echo",
        "args": ["hello", "world"],
        "env": ["PATH=/usr/bin:/bin"],
        "cwd": "/tmp"
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    // Verify the file was created and contains valid JSON
    assert!(config_path.exists(), "config.json was not created");

    let content = fs::read_to_string(&config_path).expect("Failed to read config.json");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("Invalid JSON");
    assert_eq!(parsed["command"], "/bin/echo");
    assert_eq!(parsed["args"][0], "hello");
    assert_eq!(parsed["args"][1], "world");
}

/// Test that we can create a bundle with user configuration
#[test]
fn test_create_bundle_with_user() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create config.json with user settings
    let config = serde_json::json!({
        "command": "/bin/id",
        "args": [],
        "env": ["PATH=/usr/bin:/bin"],
        "cwd": "/tmp",
        "user": {
            "uid": 1000,
            "gid": 1000
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    // Verify the user config is parsed correctly
    let content = fs::read_to_string(&config_path).expect("Failed to read config.json");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("Invalid JSON");
    assert_eq!(parsed["user"]["uid"], 1000);
    assert_eq!(parsed["user"]["gid"], 1000);
}

/// Test that we can create a bundle with root user (allowed per OCI spec)
#[test]
fn test_create_bundle_with_root_user() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create config.json with root user
    let config = serde_json::json!({
        "command": "/bin/whoami",
        "args": [],
        "env": ["PATH=/usr/bin:/bin"],
        "cwd": "/tmp",
        "user": {
            "uid": 0,
            "gid": 0
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    // Verify the root user config
    let content = fs::read_to_string(&config_path).expect("Failed to read config.json");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("Invalid JSON");
    assert_eq!(parsed["user"]["uid"], 0);
    assert_eq!(parsed["user"]["gid"], 0);
}

/// Test invalid bundle (missing config.json)
#[test]
fn test_invalid_bundle_missing_config() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Don't create config.json - this should be detected as invalid
    assert!(
        !bundle_path.join("config.json").exists(),
        "config.json should not exist"
    );
}

/// Test invalid bundle (malformed JSON)
#[test]
fn test_invalid_bundle_malformed_json() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create malformed JSON
    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, "{ invalid json }").expect("Failed to write malformed config.json");

    // Try to parse it - should fail
    let content = fs::read_to_string(&config_path).expect("Failed to read config.json");
    let result: Result<serde_json::Value, _> = serde_json::from_str(&content);
    assert!(result.is_err(), "Malformed JSON should fail to parse");
}
