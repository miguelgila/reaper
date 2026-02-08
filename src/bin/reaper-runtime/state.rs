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
    #[serde(default)]
    pub terminal: bool,
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
            terminal: false,
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

/// State for an exec process within a container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecState {
    pub container_id: String,
    pub exec_id: String,
    pub status: String, // created | running | stopped
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub terminal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

pub fn exec_state_path(container_id: &str, exec_id: &str) -> PathBuf {
    container_dir(container_id).join(format!("exec-{}.json", exec_id))
}

pub fn save_exec_state(state: &ExecState) -> anyhow::Result<()> {
    let dir = container_dir(&state.container_id);
    fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(&state)?;
    fs::write(exec_state_path(&state.container_id, &state.exec_id), json)?;
    Ok(())
}

pub fn load_exec_state(container_id: &str, exec_id: &str) -> anyhow::Result<ExecState> {
    let path = exec_state_path(container_id, exec_id);
    let data = fs::read(&path)?;
    let state: ExecState = serde_json::from_slice(&data)?;
    Ok(state)
}

// pub fn delete_exec_state(container_id: &str, exec_id: &str) -> anyhow::Result<()> {
//     let path = exec_state_path(container_id, exec_id);
//     if path.exists() {
//         fs::remove_file(path)?;
//     }
//     Ok(())
// }

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
                terminal: false,
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

    #[test]
    #[serial]
    fn test_exec_state_path() {
        with_test_root(|_root| {
            let path = exec_state_path("my-container", "exec1");
            assert!(path.to_string_lossy().contains("exec-exec1.json"));
            assert!(path.to_string_lossy().contains("my-container"));
        });
    }

    #[test]
    #[serial]
    fn test_save_and_load_exec_state() {
        with_test_root(|_| {
            let exec_state = ExecState {
                container_id: "test-container".to_string(),
                exec_id: "exec1".to_string(),
                status: "running".to_string(),
                pid: Some(9999),
                exit_code: None,
                args: vec!["/bin/sh".to_string()],
                env: Some(vec!["PATH=/usr/bin".to_string()]),
                cwd: Some("/".to_string()),
                terminal: true,
                stdin: Some("/path/to/stdin".to_string()),
                stdout: Some("/path/to/stdout".to_string()),
                stderr: Some("/path/to/stderr".to_string()),
            };

            // Save exec state
            save_exec_state(&exec_state).expect("Failed to save exec state");

            // Load exec state
            let loaded =
                load_exec_state("test-container", "exec1").expect("Failed to load exec state");
            assert_eq!(loaded.container_id, exec_state.container_id);
            assert_eq!(loaded.exec_id, exec_state.exec_id);
            assert_eq!(loaded.status, exec_state.status);
            assert_eq!(loaded.pid, exec_state.pid);
            assert_eq!(loaded.args, exec_state.args);
            assert_eq!(loaded.terminal, exec_state.terminal);
        });
    }

    // #[test]
    // #[serial]
    // fn test_delete_exec_state() {
    //     with_test_root(|_| {
    //         let exec_state = ExecState {
    //             container_id: "test-container".to_string(),
    //             exec_id: "exec1".to_string(),
    //             status: "created".to_string(),
    //             pid: None,
    //             exit_code: None,
    //             args: vec!["/bin/echo".to_string(), "hello".to_string()],
    //             env: None,
    //             cwd: None,
    //             terminal: false,
    //             stdin: None,
    //             stdout: None,
    //             stderr: None,
    //         };

    //         save_exec_state(&exec_state).expect("Failed to save exec state");
    //         let path = exec_state_path("test-container", "exec1");
    //         assert!(path.exists(), "Exec state file should exist after save");

    //         delete_exec_state("test-container", "exec1").expect("Failed to delete exec state");
    //         assert!(
    //             !path.exists(),
    //             "Exec state file should not exist after delete"
    //         );
    //     });
    // }

    // #[test]
    // #[serial]
    // fn test_delete_nonexistent_exec_state() {
    //     with_test_root(|_| {
    //         // Should not error on deleting nonexistent exec state
    //         delete_exec_state("nonexistent", "exec1")
    //             .expect("Delete should not fail on nonexistent exec");
    //     });
    // }
}
