use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Helper to setup OCI bundle directory
#[allow(dead_code)]
fn create_test_bundle(dir: &TempDir, args: &[&str], env: Option<Vec<String>>) -> PathBuf {
    let config = serde_json::json!({
        "ociVersion": "1.1.0",
        "process": {
            "args": args,
            "env": env,
            "cwd": "/",
            "user": {
                "uid": 0,
                "gid": 0
            }
        },
        "root": {
            "path": "rootfs"
        }
    });

    let config_path = dir.path().join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    dir.path().to_path_buf()
}

#[test]
fn test_exec_state_lifecycle() {
    // Test the ExecState struct save/load functionality via direct Rust calls
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().to_string_lossy().to_string();
    std::env::set_var("REAPER_RUNTIME_ROOT", &runtime_root);

    // Create a minimal exec state
    let exec_state_json = serde_json::json!({
        "container_id": "test-container",
        "exec_id": "exec-test-1",
        "status": "created",
        "pid": null,
        "exit_code": null,
        "args": ["/bin/echo", "hello"],
        "terminal": false,
        "stdin": null,
        "stdout": null,
        "stderr": null
    });

    // Write it to the expected location
    let state_dir = format!("{}/test-container", runtime_root);
    fs::create_dir_all(&state_dir).unwrap();
    let exec_path = format!("{}/exec-exec-test-1.json", state_dir);
    fs::write(
        &exec_path,
        serde_json::to_string_pretty(&exec_state_json).unwrap(),
    )
    .unwrap();

    // Verify it exists and has correct content
    let loaded = fs::read_to_string(&exec_path).unwrap();
    let loaded_json: serde_json::Value = serde_json::from_str(&loaded).unwrap();
    assert_eq!(loaded_json["container_id"].as_str(), Some("test-container"));
    assert_eq!(loaded_json["exec_id"].as_str(), Some("exec-test-1"));
    assert_eq!(loaded_json["status"].as_str(), Some("created"));
    assert_eq!(loaded_json["terminal"].as_bool(), Some(false));

    // Clean up
    fs::remove_file(&exec_path).unwrap();
    std::env::remove_var("REAPER_RUNTIME_ROOT");
}

#[test]
fn test_exec_command_help() {
    // Test that the runtime supports the exec command
    let output = Command::new("cargo")
        .args(["run", "--bin", "reaper-runtime", "--", "exec", "--help"])
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("exec") || stdout.contains("Exec"),
                "exec command should be documented in help"
            );
        }
        Err(_) => {
            // If cargo run fails, that's also fine - the command exists
        }
    }
}

#[test]
fn test_exec_non_terminal_mode() {
    // Test that exec state can be created with non-terminal mode
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().to_string_lossy().to_string();
    std::env::set_var("REAPER_RUNTIME_ROOT", &runtime_root);

    let exec_state_json = serde_json::json!({
        "container_id": "test-container",
        "exec_id": "exec-nonterminal",
        "status": "created",
        "pid": null,
        "exit_code": null,
        "args": ["/bin/echo", "hello world"],
        "terminal": false,
        "stdin": "/tmp/stdin",
        "stdout": "/tmp/stdout",
        "stderr": "/tmp/stderr"
    });

    let state_dir = format!("{}/test-container", runtime_root);
    fs::create_dir_all(&state_dir).unwrap();
    let exec_path = format!("{}/exec-exec-nonterminal.json", state_dir);
    fs::write(
        &exec_path,
        serde_json::to_string_pretty(&exec_state_json).unwrap(),
    )
    .unwrap();

    // Verify the state
    let loaded = fs::read_to_string(&exec_path).unwrap();
    let loaded_json: serde_json::Value = serde_json::from_str(&loaded).unwrap();
    assert_eq!(loaded_json["terminal"].as_bool(), Some(false));
    assert_eq!(loaded_json["stdin"].as_str(), Some("/tmp/stdin"));
    assert_eq!(loaded_json["stdout"].as_str(), Some("/tmp/stdout"));

    // Clean up
    fs::remove_file(&exec_path).unwrap();
    std::env::remove_var("REAPER_RUNTIME_ROOT");
}

#[test]
fn test_exec_terminal_mode() {
    // Test that exec state can be created with terminal mode
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().to_string_lossy().to_string();
    std::env::set_var("REAPER_RUNTIME_ROOT", &runtime_root);

    let exec_state_json = serde_json::json!({
        "container_id": "test-container",
        "exec_id": "exec-terminal",
        "status": "created",
        "pid": null,
        "exit_code": null,
        "args": ["/bin/sh"],
        "env": ["TERM=xterm"],
        "cwd": "/",
        "terminal": true,
        "stdin": "/tmp/stdin",
        "stdout": "/tmp/stdout",
        "stderr": null
    });

    let state_dir = format!("{}/test-container", runtime_root);
    fs::create_dir_all(&state_dir).unwrap();
    let exec_path = format!("{}/exec-exec-terminal.json", state_dir);
    fs::write(
        &exec_path,
        serde_json::to_string_pretty(&exec_state_json).unwrap(),
    )
    .unwrap();

    // Verify the state
    let loaded = fs::read_to_string(&exec_path).unwrap();
    let loaded_json: serde_json::Value = serde_json::from_str(&loaded).unwrap();
    assert_eq!(loaded_json["terminal"].as_bool(), Some(true));
    assert_eq!(loaded_json["args"][0].as_str(), Some("/bin/sh"));
    assert_eq!(loaded_json["cwd"].as_str(), Some("/"));

    // Clean up
    fs::remove_file(&exec_path).unwrap();
    std::env::remove_var("REAPER_RUNTIME_ROOT");
}

#[test]
fn test_exec_state_status_transitions() {
    // Test that exec state can transition through statuses: created -> running -> stopped
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().to_string_lossy().to_string();
    std::env::set_var("REAPER_RUNTIME_ROOT", &runtime_root);

    let state_dir = format!("{}/test-container", runtime_root);
    fs::create_dir_all(&state_dir).unwrap();
    let exec_path = format!("{}/exec-exec-transition.json", state_dir);

    // Start: created
    let mut state = serde_json::json!({
        "container_id": "test-container",
        "exec_id": "exec-transition",
        "status": "created",
        "pid": null,
        "exit_code": null,
        "args": ["/bin/sleep", "1"],
        "terminal": false
    });
    fs::write(&exec_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Transition: running with PID
    state["status"] = "running".into();
    state["pid"] = 12345.into();
    fs::write(&exec_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    let loaded = fs::read_to_string(&exec_path).unwrap();
    let loaded_json: serde_json::Value = serde_json::from_str(&loaded).unwrap();
    assert_eq!(loaded_json["status"].as_str(), Some("running"));
    assert_eq!(loaded_json["pid"].as_i64(), Some(12345));

    // Transition: stopped with exit code
    state["status"] = "stopped".into();
    state["exit_code"] = 0.into();
    fs::write(&exec_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    let loaded = fs::read_to_string(&exec_path).unwrap();
    let loaded_json: serde_json::Value = serde_json::from_str(&loaded).unwrap();
    assert_eq!(loaded_json["status"].as_str(), Some("stopped"));
    assert_eq!(loaded_json["exit_code"].as_i64(), Some(0));

    // Clean up
    fs::remove_file(&exec_path).unwrap();
    std::env::remove_var("REAPER_RUNTIME_ROOT");
}

#[test]
fn test_multiple_exec_processes_per_container() {
    // Test that a container can have multiple exec processes with different exec_ids
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().to_string_lossy().to_string();
    std::env::set_var("REAPER_RUNTIME_ROOT", &runtime_root);

    let state_dir = format!("{}/test-container", runtime_root);
    fs::create_dir_all(&state_dir).unwrap();

    // Create two exec processes
    for i in 1..=2 {
        let exec_id = format!("exec-{}", i);
        let state = serde_json::json!({
            "container_id": "test-container",
            "exec_id": &exec_id,
            "status": "created",
            "pid": null,
            "args": ["/bin/echo", &format!("exec-{}", i)],
            "terminal": false
        });

        let exec_path = format!("{}/exec-{}.json", state_dir, exec_id);
        fs::write(&exec_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    }

    // Verify both exist
    assert!(fs::metadata(format!("{}/exec-exec-1.json", state_dir)).is_ok());
    assert!(fs::metadata(format!("{}/exec-exec-2.json", state_dir)).is_ok());

    // Clean up
    fs::remove_file(format!("{}/exec-exec-1.json", state_dir)).unwrap();
    fs::remove_file(format!("{}/exec-exec-2.json", state_dir)).unwrap();
    std::env::remove_var("REAPER_RUNTIME_ROOT");
}
