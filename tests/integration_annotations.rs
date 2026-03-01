use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Helper: create a bundle dir with config.json containing given annotations.
fn create_bundle_with_annotations(annotations: Option<serde_json::Value>) -> TempDir {
    let bundle_dir = TempDir::new().expect("Failed to create temp bundle dir");

    let mut config = serde_json::json!({
        "process": {
            "args": ["/bin/echo", "annotation-test"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin:/usr/local/bin"]
        }
    });

    if let Some(annots) = annotations {
        config
            .as_object_mut()
            .unwrap()
            .insert("annotations".to_string(), annots);
    }

    fs::write(
        bundle_dir.path().join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .expect("Failed to write config.json");

    bundle_dir
}

/// Helper: read the state.json for a container and return parsed JSON.
fn read_state_json(state_root: &str, container_id: &str) -> serde_json::Value {
    let state_file = std::path::Path::new(state_root)
        .join(container_id)
        .join("state.json");
    let data = fs::read_to_string(&state_file).expect("Failed to read state.json");
    serde_json::from_str(&data).expect("Failed to parse state.json")
}

/// Test that annotations passed via --annotation are stored in state.
#[test]
fn test_create_with_annotations_stores_in_state() {
    let bundle = create_bundle_with_annotations(None);
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    let output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-ann-create")
        .arg("--bundle")
        .arg(bundle.path())
        .arg("--annotation")
        .arg("dns-mode=kubernetes")
        .output()
        .expect("Failed to run create");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let state = read_state_json(&state_root, "test-ann-create");
    let annotations = state.get("annotations").expect("annotations field missing");
    assert_eq!(
        annotations.get("dns-mode"),
        Some(&serde_json::json!("kubernetes"))
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-ann-create")
        .output();
}

/// Test that multiple annotations are stored correctly.
#[test]
fn test_create_with_multiple_annotations() {
    let bundle = create_bundle_with_annotations(None);
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    let output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-ann-multi")
        .arg("--bundle")
        .arg(bundle.path())
        .arg("--annotation")
        .arg("dns-mode=host")
        .arg("--annotation")
        .arg("future-key=future-value")
        .output()
        .expect("Failed to run create");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let state = read_state_json(&state_root, "test-ann-multi");
    let annotations = state.get("annotations").expect("annotations field missing");
    assert_eq!(
        annotations.get("dns-mode"),
        Some(&serde_json::json!("host"))
    );
    assert_eq!(
        annotations.get("future-key"),
        Some(&serde_json::json!("future-value"))
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-ann-multi")
        .output();
}

/// Test that no annotations = no annotations field in state (backward compatible).
#[test]
fn test_create_without_annotations_backward_compatible() {
    let bundle = create_bundle_with_annotations(None);
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    let output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-ann-compat")
        .arg("--bundle")
        .arg(bundle.path())
        .output()
        .expect("Failed to run create");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let state = read_state_json(&state_root, "test-ann-compat");
    // annotations field should be absent (skip_serializing_if = "Option::is_none")
    assert!(
        state.get("annotations").is_none(),
        "annotations field should not be present when no annotations provided"
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-ann-compat")
        .output();
}

/// Test that annotations work together with namespace.
#[test]
fn test_create_with_annotations_and_namespace() {
    let bundle = create_bundle_with_annotations(None);
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    let output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-ann-ns")
        .arg("--bundle")
        .arg(bundle.path())
        .arg("--namespace")
        .arg("production")
        .arg("--annotation")
        .arg("dns-mode=kubernetes")
        .output()
        .expect("Failed to run create");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let state = read_state_json(&state_root, "test-ann-ns");
    assert_eq!(
        state.get("namespace"),
        Some(&serde_json::json!("production"))
    );
    let annotations = state.get("annotations").expect("annotations field missing");
    assert_eq!(
        annotations.get("dns-mode"),
        Some(&serde_json::json!("kubernetes"))
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-ann-ns")
        .output();
}

/// Test that overlay-name annotation is stored in state.
#[test]
fn test_create_with_overlay_name_stores_in_state() {
    let bundle = create_bundle_with_annotations(None);
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    let output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-ann-overlay")
        .arg("--bundle")
        .arg(bundle.path())
        .arg("--annotation")
        .arg("overlay-name=pippo")
        .output()
        .expect("Failed to run create");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let state = read_state_json(&state_root, "test-ann-overlay");
    let annotations = state.get("annotations").expect("annotations field missing");
    assert_eq!(
        annotations.get("overlay-name"),
        Some(&serde_json::json!("pippo"))
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-ann-overlay")
        .output();
}

/// Test that overlay-name and namespace work together.
#[test]
fn test_create_with_overlay_name_and_namespace() {
    let bundle = create_bundle_with_annotations(None);
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    let output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("create")
        .arg("test-ann-overlay-ns")
        .arg("--bundle")
        .arg(bundle.path())
        .arg("--namespace")
        .arg("production")
        .arg("--annotation")
        .arg("overlay-name=pippo")
        .arg("--annotation")
        .arg("dns-mode=kubernetes")
        .output()
        .expect("Failed to run create");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let state = read_state_json(&state_root, "test-ann-overlay-ns");
    assert_eq!(
        state.get("namespace"),
        Some(&serde_json::json!("production"))
    );
    let annotations = state.get("annotations").expect("annotations field missing");
    assert_eq!(
        annotations.get("overlay-name"),
        Some(&serde_json::json!("pippo"))
    );
    assert_eq!(
        annotations.get("dns-mode"),
        Some(&serde_json::json!("kubernetes"))
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-ann-overlay-ns")
        .output();
}

/// Test that REAPER_ANNOTATIONS_ENABLED=false causes annotations to be ignored
/// during do_start() (annotations are still stored in state, but not applied).
/// This test verifies the create path stores annotations regardless of the enabled flag,
/// since filtering happens at start time.
#[test]
fn test_annotations_stored_even_when_disabled() {
    let bundle = create_bundle_with_annotations(None);
    let state_dir = TempDir::new().expect("Failed to create state dir");
    let state_root = state_dir.path().to_string_lossy().to_string();
    let reaper_bin = env!("CARGO_BIN_EXE_reaper-runtime");

    let output = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .env("REAPER_ANNOTATIONS_ENABLED", "false")
        .arg("create")
        .arg("test-ann-disabled")
        .arg("--bundle")
        .arg(bundle.path())
        .arg("--annotation")
        .arg("dns-mode=kubernetes")
        .output()
        .expect("Failed to run create");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Annotations are stored in state (create doesn't filter)
    let state = read_state_json(&state_root, "test-ann-disabled");
    let annotations = state
        .get("annotations")
        .expect("annotations should be stored");
    assert_eq!(
        annotations.get("dns-mode"),
        Some(&serde_json::json!("kubernetes"))
    );

    // Cleanup
    let _ = Command::new(reaper_bin)
        .env("REAPER_RUNTIME_ROOT", &state_root)
        .env("REAPER_NO_OVERLAY", "1")
        .arg("delete")
        .arg("test-ann-disabled")
        .output();
}
