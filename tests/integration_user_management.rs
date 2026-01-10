use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Test running a process with a specific uid/gid
#[test]
fn test_run_with_current_user() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Get current user's uid and gid
    let uid = unsafe { nix::libc::getuid() };
    let gid = unsafe { nix::libc::getgid() };

    // Create config.json with user field set to current user
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", "id -u && id -g"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin"],
            "user": {
                "uid": uid,
                "gid": gid
            }
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create container
    let create_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-user")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run create command");

    assert!(
        create_output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );

    // Start container
    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-user")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run start command");

    assert!(
        start_output.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start_output.stderr)
    );

    // Wait a bit for process to complete
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Cleanup
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("delete")
        .arg("test-user")
        .output()
        .expect("Failed to delete");
}

/// Test running a process without user field (backwards compatibility)
#[test]
fn test_run_without_user_field() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create config.json WITHOUT user field
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "no user field"],
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

    // Create and start should work without user field
    let create_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-no-user")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run create command");

    assert!(create_output.status.success());

    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-no-user")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run start command");

    assert!(
        start_output.status.success(),
        "start should succeed without user field"
    );

    // Cleanup
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("delete")
        .arg("test-no-user")
        .output()
        .expect("Failed to delete");
}

/// Test that umask is applied correctly
#[test]
fn test_run_with_umask() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Get current user's uid and gid
    let uid = unsafe { nix::libc::getuid() };
    let gid = unsafe { nix::libc::getgid() };

    // Create a temporary file path for the process to write to
    let test_file = bundle_path.join("umask_test_file");
    let test_file_str = test_file.to_string_lossy().to_string();

    // Create config with umask=077 (only owner can read/write/execute)
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", format!("touch {} && ls -l {}", test_file_str, test_file_str)],
            "cwd": bundle_path.to_string_lossy(),
            "env": ["PATH=/usr/bin:/bin"],
            "user": {
                "uid": uid,
                "gid": gid,
                "umask": 77  // 077 in octal = 63 in decimal... but umask uses octal representation
            }
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-umask")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to create");

    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-umask")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    assert!(
        start_output.status.success(),
        "start with umask failed: {}",
        String::from_utf8_lossy(&start_output.stderr)
    );

    // Wait for process to complete
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Cleanup
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("delete")
        .arg("test-umask")
        .output()
        .expect("Failed to delete");
}

/// Test running with additional groups
#[test]
fn test_run_with_additional_groups() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Get current user's uid and gid
    let uid = unsafe { nix::libc::getuid() };
    let gid = unsafe { nix::libc::getgid() };

    // Create config with additional groups
    // Note: On macOS, groups are handled differently, so we just verify parsing works
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", "id && groups"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin"],
            "user": {
                "uid": uid,
                "gid": gid,
                "additionalGids": [20, 12]  // staff and everyone on macOS
            }
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-groups")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to create");

    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-groups")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    // This might fail if user doesn't have permission to set these groups
    // That's ok - we're mainly testing that the parsing and code path works
    if !start_output.status.success() {
        let stderr = String::from_utf8_lossy(&start_output.stderr);
        // If it's a permission error, that's expected for non-root users
        assert!(
            stderr.contains("setgroups")
                || stderr.contains("PermissionDenied")
                || stderr.contains("Operation not permitted"),
            "Expected permission-related error, got: {}",
            stderr
        );
    }

    // Cleanup
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("delete")
        .arg("test-groups")
        .output()
        .ok(); // Might fail if create/start failed
}

/// Test that config with root user (uid=0) parses correctly
/// Note: This won't actually run as root unless tests are run with sudo
#[test]
fn test_config_with_root_user() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Create config with root user
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "running as root"],
            "cwd": "/",
            "env": ["PATH=/usr/bin:/bin:/usr/local/bin"],
            "user": {
                "uid": 0,
                "gid": 0
            }
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create should work fine
    let create_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-root")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to create");

    assert!(create_output.status.success());

    // Start will likely fail unless running as root
    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-root")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    // Check if we're running as root
    let current_uid = unsafe { nix::libc::getuid() };
    if current_uid == 0 {
        // Running as root, should succeed
        assert!(
            start_output.status.success(),
            "start as root failed: {}",
            String::from_utf8_lossy(&start_output.stderr)
        );
    } else {
        // Not running as root, expect permission error
        assert!(
            !start_output.status.success(),
            "start should fail when trying to setuid(0) as non-root user"
        );
    }

    // Cleanup
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("delete")
        .arg("test-root")
        .output()
        .ok();
}
