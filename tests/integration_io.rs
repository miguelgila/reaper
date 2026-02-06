use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

// Use nix::libc for FIFO and file operations
use nix::libc;

/// Helper: Create a FIFO (named pipe) at the given path
fn create_fifo(path: &str) -> std::io::Result<()> {
    use std::ffi::CString;

    let c_path = CString::new(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

    unsafe {
        let ret = libc::mkfifo(c_path.as_ptr(), 0o666);
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// CRITICAL: Test that I/O paths are stored in state file
#[test]
fn test_io_paths_stored_in_state() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "hello"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create with I/O paths
    let stdout_path = "/run/test-stdout-fifo";
    let stderr_path = "/run/test-stderr-fifo";

    let create_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-io-paths")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--stdout")
        .arg(stdout_path)
        .arg("--stderr")
        .arg(stderr_path)
        .output()
        .expect("Failed to run create command");

    assert!(
        create_output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );

    // Verify state file contains I/O paths
    let state_file_path = PathBuf::from(&state_root)
        .join("test-io-paths")
        .join("state.json");

    assert!(state_file_path.exists(), "State file should exist");

    let state_content = fs::read_to_string(&state_file_path).expect("Failed to read state file");
    let state_json: serde_json::Value =
        serde_json::from_str(&state_content).expect("Failed to parse state JSON");

    // Verify I/O paths are present in state
    assert_eq!(
        state_json["stdout"].as_str(),
        Some(stdout_path),
        "stdout path should be stored in state"
    );
    assert_eq!(
        state_json["stderr"].as_str(),
        Some(stderr_path),
        "stderr path should be stored in state"
    );

    // Verify other state fields are intact
    assert_eq!(state_json["id"], "test-io-paths");
    assert_eq!(state_json["status"], "created");
}

/// CRITICAL: Test basic FIFO redirection - output captured
#[test]
fn test_basic_fifo_stdout_redirection() {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "hello world"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let io_dir = TempDir::new().expect("Failed to create I/O dir");
    let stdout_fifo_path = io_dir.path().join("stdout").to_string_lossy().to_string();

    // Create FIFO
    create_fifo(&stdout_fifo_path).expect("Failed to create FIFO");

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create container with stdout FIFO
    let create_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-fifo-stdout")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--stdout")
        .arg(&stdout_fifo_path)
        .output()
        .expect("Failed to run create command");

    assert!(create_output.status.success());

    // Spawn a thread to read from FIFO (must happen before start writes)
    let fifo_path_for_reader = stdout_fifo_path.clone();
    let fifo_content = Arc::new(Mutex::new(String::new()));
    let fifo_content_clone = Arc::clone(&fifo_content);

    let reader_thread = thread::spawn(move || match std::fs::File::open(&fifo_path_for_reader) {
        Ok(mut file) => {
            let mut content = String::new();
            let _ = file.read_to_string(&mut content);
            *fifo_content_clone.lock().unwrap() = content;
        }
        Err(e) => {
            eprintln!("Failed to open FIFO for reading: {}", e);
        }
    });

    // Give reader time to open FIFO
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Start container - this will write to the FIFO
    let _start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-fifo-stdout")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to run start command");

    // Wait for reader thread
    let _ = reader_thread.join();

    let content = fifo_content.lock().unwrap();
    assert!(
        content.contains("hello world"),
        "FIFO should contain 'hello world', got: {}",
        content
    );
}

/// CRITICAL: Test multi-line output to FIFO
#[test]
fn test_multiline_output_to_fifo() {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Command that outputs multiple lines
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", "echo 'line 1'; echo 'line 2'; echo 'line 3'"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let io_dir = TempDir::new().expect("Failed to create I/O dir");
    let stdout_fifo_path = io_dir.path().join("stdout").to_string_lossy().to_string();

    create_fifo(&stdout_fifo_path).expect("Failed to create FIFO");

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-multiline")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--stdout")
        .arg(&stdout_fifo_path)
        .output()
        .expect("Failed to create");

    // Spawn reader thread
    let fifo_path_for_reader = stdout_fifo_path.clone();
    let fifo_content = Arc::new(Mutex::new(String::new()));
    let fifo_content_clone = Arc::clone(&fifo_content);

    let reader_thread = thread::spawn(move || {
        if let Ok(mut file) = std::fs::File::open(&fifo_path_for_reader) {
            let mut content = String::new();
            let _ = file.read_to_string(&mut content);
            *fifo_content_clone.lock().unwrap() = content;
        }
    });

    std::thread::sleep(std::time::Duration::from_millis(100));

    let _start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-multiline")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    let _ = reader_thread.join();
    let content = fifo_content.lock().unwrap();

    // Verify all lines are present
    assert!(content.contains("line 1"));
    assert!(content.contains("line 2"));
    assert!(content.contains("line 3"));
}

/// HIGH: Test fallback to inherited stdio when FIFO doesn't exist
#[test]
fn test_fifo_nonexistent_fallback_to_inherit() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "fallback test"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create with non-existent FIFO path
    let nonexistent_fifo = "/run/this-fifo-does-not-exist-12345";

    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-fallback")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--stdout")
        .arg(nonexistent_fifo)
        .output()
        .expect("Failed to create");

    // Start should still succeed (fallback to inherit)
    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-fallback")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    // Start should succeed even though FIFO doesn't exist
    assert!(
        start_output.status.success(),
        "start should succeed even with nonexistent FIFO (fallback to inherit): {}",
        String::from_utf8_lossy(&start_output.stderr)
    );

    // Verify process completed
    let state_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("state")
        .arg("test-fallback")
        .output()
        .expect("Failed to get state");

    let state_json = String::from_utf8_lossy(&state_output.stdout);
    let state: serde_json::Value =
        serde_json::from_str(&state_json).expect("Failed to parse state JSON");

    // Process should have completed
    assert!(
        state["status"].as_str() == Some("running") || state["status"].as_str() == Some("stopped"),
        "Process should be running or stopped"
    );
}

/// HIGH: Test stderr redirection
#[test]
fn test_stderr_redirection() {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    // Command that writes to stderr
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", "echo 'error message' >&2"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let io_dir = TempDir::new().expect("Failed to create I/O dir");
    let stderr_fifo_path = io_dir.path().join("stderr").to_string_lossy().to_string();

    create_fifo(&stderr_fifo_path).expect("Failed to create stderr FIFO");

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-stderr")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--stderr")
        .arg(&stderr_fifo_path)
        .output()
        .expect("Failed to create");

    // Spawn reader thread
    let fifo_path_for_reader = stderr_fifo_path.clone();
    let fifo_content = Arc::new(Mutex::new(String::new()));
    let fifo_content_clone = Arc::clone(&fifo_content);

    let reader_thread = thread::spawn(move || {
        if let Ok(mut file) = std::fs::File::open(&fifo_path_for_reader) {
            let mut content = String::new();
            let _ = file.read_to_string(&mut content);
            *fifo_content_clone.lock().unwrap() = content;
        }
    });

    std::thread::sleep(std::time::Duration::from_millis(100));

    let _start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-stderr")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    let _ = reader_thread.join();
    let content = fifo_content.lock().unwrap();

    assert!(
        content.contains("error message"),
        "stderr FIFO should contain 'error message', got: {}",
        content
    );
}

/// HIGH: Test permission denied on FIFO (graceful error handling)
#[test]
fn test_permission_denied_on_fifo() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "permission test"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let io_dir = TempDir::new().expect("Failed to create I/O dir");
    let stdout_fifo_path = io_dir.path().join("stdout").to_string_lossy().to_string();

    // Create FIFO with no permissions
    create_fifo(&stdout_fifo_path).expect("Failed to create FIFO");
    fs::set_permissions(&stdout_fifo_path, fs::Permissions::from_mode(0o000))
        .expect("Failed to set permissions");

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-permission")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--stdout")
        .arg(&stdout_fifo_path)
        .output()
        .expect("Failed to create");

    // Start should succeed (graceful fallback to inherit)
    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-permission")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    // Process should still run
    assert!(
        start_output.status.success(),
        "start should succeed even with permission denied (fallback to inherit): {}",
        String::from_utf8_lossy(&start_output.stderr)
    );

    // Restore permissions for cleanup
    let _ = fs::set_permissions(&stdout_fifo_path, fs::Permissions::from_mode(0o666));
}

/// MEDIUM: Test state serialization skips None I/O fields
#[test]
fn test_state_skips_none_io_fields() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "test"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create without I/O paths
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-no-io")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to create");

    // Check state file
    let state_file_path = PathBuf::from(&state_root)
        .join("test-no-io")
        .join("state.json");

    let state_content = fs::read_to_string(&state_file_path).expect("Failed to read state file");
    let state_json: serde_json::Value =
        serde_json::from_str(&state_content).expect("Failed to parse state JSON");

    // Verify None fields are not present in JSON (due to skip_serializing_if)
    assert!(
        state_json.get("stdout").is_none(),
        "None stdout should not be in JSON"
    );
    assert!(
        state_json.get("stderr").is_none(),
        "None stderr should not be in JSON"
    );
    assert!(
        state_json.get("stdin").is_none(),
        "None stdin should not be in JSON"
    );

    // But fields that are set should still be there
    assert!(state_json.get("id").is_some(), "id should be in JSON");
    assert!(
        state_json.get("status").is_some(),
        "status should be in JSON"
    );
}

/// MEDIUM: Test empty I/O paths fallback to inherit
#[test]
fn test_empty_io_paths_fallback() {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");
    let bundle_path = bundle_dir.path();

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "empty path test"],
            "cwd": "/tmp",
        }
    });

    let config_path = bundle_path.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("Failed to write config.json");

    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();

    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    // Create with empty string I/O paths
    Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("create")
        .arg("test-empty-io")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--stdout")
        .arg("")
        .output()
        .expect("Failed to create");

    // Start should succeed (empty path falls back to inherit)
    let start_output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .arg("start")
        .arg("test-empty-io")
        .arg("--bundle")
        .arg(bundle_path)
        .output()
        .expect("Failed to start");

    assert!(
        start_output.status.success(),
        "start should succeed with empty I/O path: {}",
        String::from_utf8_lossy(&start_output.stderr)
    );
}
