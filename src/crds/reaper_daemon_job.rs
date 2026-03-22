use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{ReaperEnvVar, ReaperToleration, ReaperVolume};

/// ReaperDaemonJob runs a command to completion on every matching node and
/// re-triggers on node events (join, reboot). Designed for node configuration
/// tasks like Ansible playbooks that compose via shared overlays.
///
/// Controller layering: ReaperDaemonJob → ReaperPod → Pod.
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "reaper.io",
    version = "v1alpha1",
    kind = "ReaperDaemonJob",
    namespaced,
    status = "ReaperDaemonJobStatus",
    printcolumn = r#"{"name":"Phase", "type":"string", "jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Ready", "type":"string", "jsonPath":".status.readyNodes"}"#,
    printcolumn = r#"{"name":"Total", "type":"string", "jsonPath":".status.totalNodes"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "jsonPath":".metadata.creationTimestamp"}"#,
    shortname = "rdjob"
)]
#[serde(rename_all = "camelCase")]
pub struct ReaperDaemonJobSpec {
    /// Command to execute on each node.
    pub command: Vec<String>,

    /// Arguments to the command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Environment variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<ReaperEnvVar>,

    /// Working directory for the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,

    /// Named overlay group for shared overlay filesystem.
    /// Multiple ReaperDaemonJobs with the same overlayName share an overlay,
    /// enabling composable vServices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_name: Option<String>,

    /// Select nodes by labels. If empty, targets all nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<BTreeMap<String, String>>,

    /// DNS resolution mode: "host" (default) or "kubernetes".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_mode: Option<String>,

    /// Run the process as this UID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_user: Option<i64>,

    /// Run the process as this GID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_group: Option<i64>,

    /// Volumes with inline mount paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<ReaperVolume>,

    /// Tolerations passed through to the underlying Pods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tolerations: Vec<ReaperToleration>,

    /// Events that trigger (re-)execution on nodes.
    /// Valid values: "NodeReady" (default), "Manual".
    /// NodeReady: run when a node becomes Ready (join or reboot).
    /// Manual: only run when the spec changes or the resource is created.
    #[serde(default = "default_trigger_on")]
    pub trigger_on: String,

    /// Dependency ordering: names of other ReaperDaemonJobs in the same
    /// namespace that must complete on a node before this job runs there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,

    /// Maximum number of retries per node on failure. 0 means no retries.
    #[serde(default)]
    pub retry_limit: i32,

    /// What to do if the job is already running on a node when a new trigger fires.
    /// "Skip" (default): skip the new trigger. "Replace": terminate and re-run.
    #[serde(
        default = "default_concurrency_policy",
        skip_serializing_if = "is_default_concurrency_policy"
    )]
    pub concurrency_policy: String,
}

impl Default for ReaperDaemonJobSpec {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            args: Vec::new(),
            env: Vec::new(),
            working_dir: None,
            overlay_name: None,
            node_selector: None,
            dns_mode: None,
            run_as_user: None,
            run_as_group: None,
            volumes: Vec::new(),
            tolerations: Vec::new(),
            trigger_on: default_trigger_on(),
            after: Vec::new(),
            retry_limit: 0,
            concurrency_policy: default_concurrency_policy(),
        }
    }
}

fn default_trigger_on() -> String {
    "NodeReady".to_string()
}

fn default_concurrency_policy() -> String {
    "Skip".to_string()
}

fn is_default_concurrency_policy(s: &str) -> bool {
    s == "Skip"
}

/// Status of a ReaperDaemonJob.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReaperDaemonJobStatus {
    /// Current phase: Pending, Running, Completed, PartiallyFailed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// Number of nodes that have completed successfully.
    #[serde(default)]
    pub ready_nodes: i32,

    /// Total number of targeted nodes.
    #[serde(default)]
    pub total_nodes: i32,

    /// Per-node execution status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_statuses: Vec<DaemonJobNodeStatus>,

    /// Spec generation that was last reconciled (detects spec changes for re-trigger).
    #[serde(default)]
    pub observed_generation: i64,

    /// Human-readable message about the current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Per-node execution status within a ReaperDaemonJob.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DaemonJobNodeStatus {
    /// Name of the node.
    pub node_name: String,

    /// Per-node phase: Pending, Running, Succeeded, Failed.
    #[serde(default = "default_node_phase")]
    pub phase: String,

    /// Name of the ReaperPod created for this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaper_pod_name: Option<String>,

    /// Exit code of the job on this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,

    /// Number of retry attempts so far.
    #[serde(default)]
    pub retry_count: i32,

    /// ISO 8601 timestamp of last execution start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_start_time: Option<String>,

    /// ISO 8601 timestamp of last completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completion_time: Option<String>,
}

fn default_node_phase() -> String {
    "Pending".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_default_values() {
        let spec = ReaperDaemonJobSpec::default();
        assert_eq!(spec.trigger_on, "NodeReady");
        assert_eq!(spec.concurrency_policy, "Skip");
        assert_eq!(spec.retry_limit, 0);
        assert!(spec.after.is_empty());
        assert!(spec.command.is_empty());
    }

    #[test]
    fn test_spec_roundtrip() {
        let original = ReaperDaemonJobSpec {
            command: vec!["/usr/bin/ansible-playbook".to_string()],
            args: vec!["site.yml".to_string()],
            overlay_name: Some("vservice-base".to_string()),
            trigger_on: "Manual".to_string(),
            after: vec!["mount-fs".to_string(), "install-deps".to_string()],
            retry_limit: 3,
            ..Default::default()
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ReaperDaemonJobSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.command, original.command);
        assert_eq!(decoded.args, original.args);
        assert_eq!(decoded.overlay_name, original.overlay_name);
        assert_eq!(decoded.trigger_on, "Manual");
        assert_eq!(decoded.after, original.after);
        assert_eq!(decoded.retry_limit, 3);
    }

    #[test]
    fn test_spec_deserialize_minimal() {
        let json = r#"{"command": ["/bin/sh", "-c", "echo hello"]}"#;
        let spec: ReaperDaemonJobSpec = serde_json::from_str(json).expect("deserialize");
        assert_eq!(spec.command, vec!["/bin/sh", "-c", "echo hello"]);
        assert_eq!(spec.trigger_on, "NodeReady");
        assert_eq!(spec.concurrency_policy, "Skip");
    }

    #[test]
    fn test_spec_concurrency_policy_skip_omitted() {
        let spec = ReaperDaemonJobSpec::default();
        let json = serde_json::to_value(&spec).expect("serialize");
        assert!(
            json.get("concurrencyPolicy").is_none(),
            "default concurrencyPolicy should be omitted"
        );
    }

    #[test]
    fn test_spec_concurrency_policy_replace_serialized() {
        let spec = ReaperDaemonJobSpec {
            concurrency_policy: "Replace".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_value(&spec).expect("serialize");
        assert_eq!(json["concurrencyPolicy"], "Replace");
    }

    #[test]
    fn test_spec_node_selector() {
        let mut selector = BTreeMap::new();
        selector.insert("role".to_string(), "compute".to_string());
        let spec = ReaperDaemonJobSpec {
            command: vec!["/bin/true".to_string()],
            node_selector: Some(selector.clone()),
            ..Default::default()
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        let decoded: ReaperDaemonJobSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.node_selector, Some(selector));
    }

    // --- Status tests ---

    #[test]
    fn test_status_default_is_empty() {
        let status = ReaperDaemonJobStatus::default();
        assert!(status.phase.is_none());
        assert_eq!(status.ready_nodes, 0);
        assert_eq!(status.total_nodes, 0);
        assert!(status.node_statuses.is_empty());
        assert!(status.message.is_none());
    }

    #[test]
    fn test_status_roundtrip() {
        let original = ReaperDaemonJobStatus {
            phase: Some("Running".to_string()),
            ready_nodes: 2,
            total_nodes: 5,
            node_statuses: vec![DaemonJobNodeStatus {
                node_name: "worker-1".to_string(),
                phase: "Succeeded".to_string(),
                reaper_pod_name: Some("my-job-worker-1".to_string()),
                exit_code: Some(0),
                retry_count: 0,
                last_start_time: Some("2026-03-21T10:00:00Z".to_string()),
                last_completion_time: Some("2026-03-21T10:01:00Z".to_string()),
            }],
            observed_generation: 1,
            message: Some("2/5 nodes completed".to_string()),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ReaperDaemonJobStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.phase.as_deref(), Some("Running"));
        assert_eq!(decoded.ready_nodes, 2);
        assert_eq!(decoded.total_nodes, 5);
        assert_eq!(decoded.node_statuses.len(), 1);
        assert_eq!(decoded.node_statuses[0].node_name, "worker-1");
        assert_eq!(decoded.node_statuses[0].exit_code, Some(0));
    }

    // --- NodeStatus tests ---

    #[test]
    fn test_node_status_defaults() {
        let json = r#"{"nodeName": "node-a"}"#;
        let ns: DaemonJobNodeStatus = serde_json::from_str(json).expect("deserialize");
        assert_eq!(ns.node_name, "node-a");
        assert_eq!(ns.phase, "Pending");
        assert_eq!(ns.retry_count, 0);
        assert!(ns.reaper_pod_name.is_none());
        assert!(ns.exit_code.is_none());
    }

    #[test]
    fn test_node_status_optional_fields_omitted() {
        let ns = DaemonJobNodeStatus {
            node_name: "node-b".to_string(),
            phase: "Pending".to_string(),
            reaper_pod_name: None,
            exit_code: None,
            retry_count: 0,
            last_start_time: None,
            last_completion_time: None,
        };
        let json = serde_json::to_value(&ns).expect("serialize");
        assert!(json.get("reaperPodName").is_none());
        assert!(json.get("exitCode").is_none());
        assert!(json.get("lastStartTime").is_none());
        assert!(json.get("lastCompletionTime").is_none());
    }
}
