use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    pub id: String,
    pub bundle: PathBuf,
    pub status: String, // created | running | stopped
    pub pid: Option<i32>,
}

impl ContainerState {
    pub fn new(id: String, bundle: PathBuf) -> Self {
        Self {
            id,
            bundle,
            status: "created".into(),
            pid: None,
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
