use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Request body sent by Wren controller when submitting a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub script: String,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    // User identity fields — populated when Wren resolves a WrenUser CRD
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplemental_groups: Option<Vec<u32>>,
    /// MPI hostfile content — written to `hostfile_path` before running the script.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostfile: Option<String>,
    /// Path where the hostfile should be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostfile_path: Option<String>,
}

/// Response returned after submitting a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResponse {
    pub job_id: String,
    pub status: JobState,
}

/// Status of a job as reported by the status endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatusResponse {
    pub job_id: String,
    pub status: JobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Possible states a job can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_request_serde_roundtrip_full() {
        let req = JobRequest {
            script: "#!/bin/bash\necho hello".to_string(),
            environment: [("FOO".to_string(), "bar".to_string())].into(),
            working_dir: Some("/tmp".to_string()),
            uid: Some(1000),
            gid: Some(1000),
            username: Some("alice".to_string()),
            home_dir: Some("/home/alice".to_string()),
            supplemental_groups: Some(vec![100, 200]),
            hostfile: None,
            hostfile_path: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        let decoded: JobRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.script, req.script);
        assert_eq!(decoded.uid, Some(1000));
        assert_eq!(decoded.gid, Some(1000));
        assert_eq!(decoded.username.as_deref(), Some("alice"));
        assert_eq!(decoded.home_dir.as_deref(), Some("/home/alice"));
        assert_eq!(decoded.supplemental_groups, Some(vec![100, 200]));
    }

    #[test]
    fn job_request_none_fields_omitted() {
        let req = JobRequest {
            script: "echo hi".to_string(),
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

        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("uid"));
        assert!(!json.contains("gid"));
        assert!(!json.contains("username"));
        assert!(!json.contains("home_dir"));
        assert!(!json.contains("supplemental_groups"));
        assert!(!json.contains("working_dir"));
        assert!(!json.contains("hostfile"));
    }

    #[test]
    fn job_request_minimal_deserialization() {
        let json = r#"{"script": "echo hello"}"#;
        let req: JobRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.script, "echo hello");
        assert!(req.environment.is_empty());
        assert!(req.uid.is_none());
        assert!(req.gid.is_none());
        assert!(req.working_dir.is_none());
    }

    #[test]
    fn job_response_roundtrip() {
        let resp = JobResponse {
            job_id: "abc-123".to_string(),
            status: JobState::Running,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: JobResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.job_id, "abc-123");
        assert_eq!(decoded.status, JobState::Running);
    }

    #[test]
    fn job_status_response_none_fields_omitted() {
        let resp = JobStatusResponse {
            job_id: "xyz".to_string(),
            status: JobState::Running,
            exit_code: None,
            message: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("exit_code"));
        assert!(!json.contains("message"));
    }

    #[test]
    fn job_status_response_with_exit_code() {
        let resp = JobStatusResponse {
            job_id: "xyz".to_string(),
            status: JobState::Failed,
            exit_code: Some(1),
            message: Some("exited with code 1".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: JobStatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.exit_code, Some(1));
        assert_eq!(decoded.message.as_deref(), Some("exited with code 1"));
    }

    #[test]
    fn job_request_hostfile_roundtrip() {
        let req = JobRequest {
            script: "mpirun ./app".to_string(),
            environment: HashMap::new(),
            working_dir: None,
            uid: None,
            gid: None,
            username: None,
            home_dir: None,
            supplemental_groups: None,
            hostfile: Some("node-0 slots=4\nnode-1 slots=4".to_string()),
            hostfile_path: Some("/tmp/hostfile".to_string()),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("hostfile"));
        assert!(json.contains("hostfile_path"));

        let decoded: JobRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            decoded.hostfile.as_deref(),
            Some("node-0 slots=4\nnode-1 slots=4")
        );
        assert_eq!(decoded.hostfile_path.as_deref(), Some("/tmp/hostfile"));
    }

    #[test]
    fn job_request_hostfile_only_no_path() {
        let json = r#"{"script": "mpirun ./app", "hostfile": "node-0 slots=2"}"#;
        let req: JobRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.hostfile.as_deref(), Some("node-0 slots=2"));
        assert!(req.hostfile_path.is_none());
    }

    #[test]
    fn job_request_hostfile_path_only_no_content() {
        let json = r#"{"script": "mpirun ./app", "hostfile_path": "/tmp/hf"}"#;
        let req: JobRequest = serde_json::from_str(json).unwrap();
        assert!(req.hostfile.is_none());
        assert_eq!(req.hostfile_path.as_deref(), Some("/tmp/hf"));
    }

    #[test]
    fn job_state_all_variants_serde() {
        let cases = [
            (JobState::Pending, "\"pending\""),
            (JobState::Running, "\"running\""),
            (JobState::Succeeded, "\"succeeded\""),
            (JobState::Failed, "\"failed\""),
            (JobState::Unknown, "\"unknown\""),
        ];
        for (state, expected) in cases {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, expected);
            let decoded: JobState = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, state);
        }
    }
}
