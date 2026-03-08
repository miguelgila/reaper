use crate::jobs::{JobRequest, JobState, JobStatusResponse};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

/// Tracks a running or completed job.
struct JobEntry {
    state: JobState,
    /// Child process handle (while running).
    child: Option<tokio::process::Child>,
    exit_code: Option<i32>,
    message: Option<String>,
    /// Path to MPI hostfile to clean up when job completes.
    hostfile_path: Option<String>,
}

/// Manages bare-metal job execution on a single node.
#[derive(Clone)]
pub struct JobManager {
    jobs: Arc<Mutex<HashMap<String, JobEntry>>>,
}

impl JobManager {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Submit a new job for execution.
    pub async fn submit(&self, request: JobRequest) -> Result<String, String> {
        let job_id = uuid::Uuid::new_v4().to_string();

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(&request.script);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Set environment
        cmd.env_clear();
        // Inherit essential vars
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", &path);
        }
        for (key, value) in &request.environment {
            cmd.env(key, value);
        }

        // Set working directory
        if let Some(ref dir) = request.working_dir {
            cmd.current_dir(dir);
        }

        // Privilege dropping via pre_exec (Unix only)
        #[cfg(unix)]
        {
            let uid = request.uid;
            let gid = request.gid;
            let supplemental_groups = request.supplemental_groups.clone();

            if uid.is_some() || gid.is_some() {
                unsafe {
                    cmd.pre_exec(move || {
                        // Order matters: setgroups → setgid → setuid (uid last, irreversible)
                        if let Some(ref groups) = supplemental_groups {
                            let gids: Vec<libc::gid_t> =
                                groups.iter().map(|&g| g as libc::gid_t).collect();
                            let ret = libc::setgroups(gids.len() as libc::c_int, gids.as_ptr());
                            if ret != 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                        } else if gid.is_some() {
                            // Clear supplementary groups when setting gid without explicit groups
                            let ret = libc::setgroups(0 as libc::c_int, std::ptr::null());
                            if ret != 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                        }

                        if let Some(g) = gid {
                            let ret = libc::setgid(g as libc::gid_t);
                            if ret != 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                        }

                        if let Some(u) = uid {
                            let ret = libc::setuid(u as libc::uid_t);
                            if ret != 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                        }

                        Ok(())
                    });
                }
            }
        }

        // Write MPI hostfile to disk if provided
        if let (Some(ref content), Some(ref path)) = (&request.hostfile, &request.hostfile_path) {
            if let Some(parent) = std::path::Path::new(path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            tokio::fs::write(path, content)
                .await
                .map_err(|e| format!("failed to write hostfile to {path}: {e}"))?;
            debug!(path = %path, "wrote MPI hostfile");
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn job: {e}"))?;

        info!(job_id = %job_id, "job submitted and running");

        let entry = JobEntry {
            state: JobState::Running,
            child: Some(child),
            exit_code: None,
            message: None,
            hostfile_path: request.hostfile_path.clone(),
        };

        let mut jobs = self.jobs.lock().await;
        jobs.insert(job_id.clone(), entry);

        Ok(job_id)
    }

    /// Get the status of a job, reaping the child if it has exited.
    pub async fn status(&self, job_id: &str) -> Option<JobStatusResponse> {
        let mut jobs = self.jobs.lock().await;
        let entry = jobs.get_mut(job_id)?;

        // If still running, check if the child has exited
        if entry.state == JobState::Running {
            if let Some(ref mut child) = entry.child {
                match child.try_wait() {
                    Ok(Some(exit_status)) => {
                        let code = exit_status.code().unwrap_or(-1);
                        entry.exit_code = Some(code);
                        entry.state = if code == 0 {
                            JobState::Succeeded
                        } else {
                            JobState::Failed
                        };
                        if code != 0 {
                            entry.message = Some(format!("exited with code {code}"));
                        }
                        entry.child = None;
                        // Clean up hostfile now that the job is done
                        if let Some(ref hf_path) = entry.hostfile_path {
                            let _ = std::fs::remove_file(hf_path);
                            entry.hostfile_path = None;
                        }
                        debug!(job_id = %job_id, code = code, "job completed");
                    }
                    Ok(None) => {
                        // Still running
                    }
                    Err(e) => {
                        error!(job_id = %job_id, error = %e, "failed to check job status");
                        entry.state = JobState::Failed;
                        entry.message = Some(format!("failed to check status: {e}"));
                        entry.child = None;
                        if let Some(ref hf_path) = entry.hostfile_path {
                            let _ = std::fs::remove_file(hf_path);
                            entry.hostfile_path = None;
                        }
                    }
                }
            }
        }

        Some(JobStatusResponse {
            job_id: job_id.to_string(),
            status: entry.state,
            exit_code: entry.exit_code,
            message: entry.message.clone(),
        })
    }

    /// Terminate a running job by sending SIGTERM, then SIGKILL after 5s.
    pub async fn terminate(&self, job_id: &str) -> bool {
        let mut jobs = self.jobs.lock().await;
        let entry = match jobs.get_mut(job_id) {
            Some(e) => e,
            None => return false,
        };

        if entry.state != JobState::Running {
            return true; // Already done
        }

        if let Some(ref mut child) = entry.child {
            // Try SIGTERM first
            #[cfg(unix)]
            {
                if let Some(pid) = child.id() {
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGTERM);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill().await;
            }

            // Give it 5 seconds to exit gracefully
            let timeout =
                tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;

            match timeout {
                Ok(Ok(status)) => {
                    let code = status.code().unwrap_or(-1);
                    entry.exit_code = Some(code);
                    entry.state = JobState::Failed;
                    entry.message = Some("terminated by request".to_string());
                }
                _ => {
                    // Force kill
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    entry.exit_code = Some(-1);
                    entry.state = JobState::Failed;
                    entry.message = Some("killed after timeout".to_string());
                }
            }
            entry.child = None;
            // Clean up hostfile on termination
            if let Some(ref hf_path) = entry.hostfile_path {
                let _ = std::fs::remove_file(hf_path);
                entry.hostfile_path = None;
            }
            info!(job_id = %job_id, "job terminated");
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(script: &str) -> JobRequest {
        JobRequest {
            script: script.to_string(),
            environment: HashMap::new(),
            working_dir: None,
            uid: None,
            gid: None,
            username: None,
            home_dir: None,
            supplemental_groups: None,
            hostfile: None,
            hostfile_path: None,
        }
    }

    #[tokio::test]
    async fn submit_and_status_success() {
        let manager = JobManager::new();
        let req = make_request("exit 0");
        let job_id = manager.submit(req).await.expect("submit should succeed");

        // Poll until completed (up to 2s)
        let mut final_status = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let s = manager.status(&job_id).await.unwrap();
            if s.status != JobState::Running {
                final_status = Some(s);
                break;
            }
        }

        let s = final_status.expect("job should have completed");
        assert_eq!(s.status, JobState::Succeeded);
        assert_eq!(s.exit_code, Some(0));
    }

    #[tokio::test]
    async fn submit_and_status_failure() {
        let manager = JobManager::new();
        let req = make_request("exit 1");
        let job_id = manager.submit(req).await.expect("submit should succeed");

        let mut final_status = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let s = manager.status(&job_id).await.unwrap();
            if s.status != JobState::Running {
                final_status = Some(s);
                break;
            }
        }

        let s = final_status.expect("job should have completed");
        assert_eq!(s.status, JobState::Failed);
        assert_eq!(s.exit_code, Some(1));
    }

    #[tokio::test]
    async fn status_unknown_job_returns_none() {
        let manager = JobManager::new();
        let result = manager.status("nonexistent-job-id").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn terminate_unknown_job_returns_false() {
        let manager = JobManager::new();
        assert!(!manager.terminate("no-such-job").await);
    }

    #[tokio::test]
    async fn terminate_running_job() {
        let manager = JobManager::new();
        // Long-running job
        let req = make_request("sleep 60");
        let job_id = manager.submit(req).await.expect("submit should succeed");

        // Give it a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let terminated = manager.terminate(&job_id).await;
        assert!(terminated);

        let s = manager.status(&job_id).await.unwrap();
        assert_eq!(s.status, JobState::Failed);
    }

    #[tokio::test]
    async fn environment_variables_passed_to_job() {
        let manager = JobManager::new();
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "hello_world".to_string());
        let req = JobRequest {
            script: r#"test "$TEST_VAR" = "hello_world""#.to_string(),
            environment: env,
            working_dir: None,
            uid: None,
            gid: None,
            username: None,
            home_dir: None,
            supplemental_groups: None,
            hostfile: None,
            hostfile_path: None,
        };
        let job_id = manager.submit(req).await.expect("submit should succeed");

        let mut final_status = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let s = manager.status(&job_id).await.unwrap();
            if s.status != JobState::Running {
                final_status = Some(s);
                break;
            }
        }

        let s = final_status.expect("job should have completed");
        assert_eq!(
            s.status,
            JobState::Succeeded,
            "env var was not passed correctly"
        );
    }

    #[tokio::test]
    async fn test_submit_writes_hostfile() {
        let manager = JobManager::new();
        let hostfile_path = format!("/tmp/reaper-test-hostfile-{}", uuid::Uuid::new_v4());
        let request = JobRequest {
            script: "true".to_string(),
            environment: HashMap::new(),
            working_dir: None,
            uid: None,
            gid: None,
            username: None,
            home_dir: None,
            supplemental_groups: None,
            hostfile: Some("node-0 slots=1\nnode-1 slots=1".to_string()),
            hostfile_path: Some(hostfile_path.clone()),
        };

        let job_id = manager.submit(request).await.unwrap();

        // Hostfile should have been written
        let content = tokio::fs::read_to_string(&hostfile_path).await.unwrap();
        assert_eq!(content, "node-0 slots=1\nnode-1 slots=1");

        // Wait for job to complete
        let mut final_status = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let s = manager.status(&job_id).await.unwrap();
            if s.status != JobState::Running {
                final_status = Some(s);
                break;
            }
        }
        let s = final_status.expect("job should have completed");
        assert_eq!(s.status, JobState::Succeeded);

        // Hostfile should be cleaned up after job completes
        assert!(
            !std::path::Path::new(&hostfile_path).exists(),
            "hostfile should be cleaned up"
        );
    }

    #[tokio::test]
    async fn test_submit_without_hostfile() {
        let manager = JobManager::new();
        let request = JobRequest {
            script: "echo hello".to_string(),
            environment: HashMap::new(),
            working_dir: None,
            uid: None,
            gid: None,
            username: None,
            home_dir: None,
            supplemental_groups: None,
            hostfile: None,
            hostfile_path: None,
        };

        let job_id = manager.submit(request).await.unwrap();
        assert!(!job_id.is_empty());

        // Should succeed without hostfile
        let mut final_status = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let s = manager.status(&job_id).await.unwrap();
            if s.status != JobState::Running {
                final_status = Some(s);
                break;
            }
        }
        let s = final_status.expect("job should have completed");
        assert_eq!(s.status, JobState::Succeeded);
    }
}
