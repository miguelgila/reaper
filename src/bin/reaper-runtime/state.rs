use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    pub id: String,
    pub bundle: PathBuf,
    pub status: String, // created | running | stopped
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

impl ContainerState {
    pub fn new(id: String, bundle: PathBuf) -> Self {
        Self {
            id,
            bundle,
            status: "created".into(),
            pid: None,
            exit_code: None,
            stdin: None,
            stdout: None,
            stderr: None,
        }
    }
}

pub fn state_dir() -> PathBuf {
    std::env::var("REAPER_RUNTIME_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/reaper"))
}

pub fn container_dir(id: &str) -> PathBuf {
    state_dir().join(id)
}

pub fn state_path(id: &str) -> PathBuf {
    container_dir(id).join("state.json")
}

pub fn pid_path(id: &str) -> PathBuf {
    container_dir(id).join("pid")
}

pub fn save_state(state: &ContainerState) -> anyhow::Result<()> {
    let dir = container_dir(&state.id);
    fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(&state)?;
    fs::write(state_path(&state.id), json)?;
    Ok(())
}

pub fn load_state(id: &str) -> anyhow::Result<ContainerState> {
    let data = fs::read(state_path(id))?;
    let state: ContainerState = serde_json::from_slice(&data)?;
    Ok(state)
}

pub fn save_pid(id: &str, pid: i32) -> anyhow::Result<()> {
    let dir = container_dir(id);
    fs::create_dir_all(&dir)?;
    let mut f = fs::File::create(pid_path(id))?;
    writeln!(f, "{}", pid)?;
    Ok(())
}

pub fn load_pid(id: &str) -> anyhow::Result<i32> {
    let s = fs::read_to_string(pid_path(id))?;
    let pid: i32 = s.trim().parse()?;
    Ok(pid)
}

pub fn delete(id: &str) -> anyhow::Result<()> {
    let dir = container_dir(id);
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn setup_test_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("Failed to create temp dir")
    }

    fn with_test_root<F>(f: F)
    where
        F: FnOnce(String),
    {
        let temp = setup_test_root();
        let root = temp.path().to_string_lossy().to_string();
        std::env::set_var("REAPER_RUNTIME_ROOT", &root);
        f(root);
        std::env::remove_var("REAPER_RUNTIME_ROOT");
        // temp is dropped here automatically
    }

    #[test]
    #[serial]
    fn test_state_dir_with_env() {
        with_test_root(|root| {
            let dir = state_dir();
            assert_eq!(dir.to_string_lossy(), root);
        });
    }

    #[test]
    #[serial]
    fn test_state_dir_default() {
        std::env::remove_var("REAPER_RUNTIME_ROOT");
        let dir = state_dir();
        assert_eq!(dir, PathBuf::from("/run/reaper"));
    }

    #[test]
    #[serial]
    fn test_container_dir() {
        with_test_root(|_root| {
            let dir = container_dir("my-container");
            assert!(dir.to_string_lossy().contains("my-container"));
        });
    }

    #[test]
    #[serial]
    fn test_state_path() {
        with_test_root(|_root| {
            let path = state_path("my-container");
            assert!(path.to_string_lossy().contains("state.json"));
            assert!(path.to_string_lossy().contains("my-container"));
        });
    }

    #[test]
    #[serial]
    fn test_pid_path() {
        with_test_root(|_root| {
            let path = pid_path("my-container");
            assert!(path.to_string_lossy().contains("pid"));
            assert!(path.to_string_lossy().contains("my-container"));
        });
    }

    #[test]
    #[serial]
    fn test_save_and_load_state() {
        with_test_root(|_| {
            let state = ContainerState {
                id: "test-container".to_string(),
                bundle: PathBuf::from("/bundle/path"),
                status: "running".to_string(),
                pid: Some(1234),
                exit_code: None,
                stdin: None,
                stdout: None,
                stderr: None,
            };

            // Save state
            save_state(&state).expect("Failed to save state");

            // Load state
            let loaded = load_state("test-container").expect("Failed to load state");
            assert_eq!(loaded.id, state.id);
            assert_eq!(loaded.bundle, state.bundle);
            assert_eq!(loaded.status, state.status);
            assert_eq!(loaded.pid, state.pid);
        });
    }

    #[test]
    #[serial]
    fn test_save_and_load_pid() {
        with_test_root(|_| {
            let id = "test-container";
            let pid = 5678;

            // Save pid
            save_pid(id, pid).expect("Failed to save pid");

            // Load pid
            let loaded_pid = load_pid(id).expect("Failed to load pid");
            assert_eq!(loaded_pid, pid);
        });
    }

    #[test]
    #[serial]
    fn test_delete_state() {
        with_test_root(|_| {
            let state = ContainerState::new("delete-test".to_string(), PathBuf::from("/bundle"));
            save_state(&state).expect("Failed to save state");

            let dir = container_dir("delete-test");
            assert!(dir.exists(), "Container dir should exist after save");

            delete("delete-test").expect("Failed to delete");

            assert!(!dir.exists(), "Container dir should not exist after delete");
        });
    }

    #[test]
    #[serial]
    fn test_delete_nonexistent() {
        with_test_root(|_| {
            // Should not error on deleting nonexistent container
            delete("nonexistent").expect("Delete should not fail on nonexistent container");
        });
    }

    #[test]
    fn test_container_state_new() {
        let state = ContainerState::new("my-id".to_string(), PathBuf::from("/my/bundle"));
        assert_eq!(state.id, "my-id");
        assert_eq!(state.bundle, PathBuf::from("/my/bundle"));
        assert_eq!(state.status, "created");
        assert_eq!(state.pid, None);
    }
}
