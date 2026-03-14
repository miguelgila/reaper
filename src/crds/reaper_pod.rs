use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// ReaperPod is a simplified, Reaper-native way to run workloads on Kubernetes.
/// The controller translates it into a real Pod with runtimeClassName: reaper-v2.
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "reaper.io",
    version = "v1alpha1",
    kind = "ReaperPod",
    namespaced,
    status = "ReaperPodStatus",
    printcolumn = r#"{"name":"Phase", "type":"string", "jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Node", "type":"string", "jsonPath":".status.nodeName"}"#,
    printcolumn = r#"{"name":"Exit Code", "type":"integer", "jsonPath":".status.exitCode"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ReaperPodSpec {
    /// Command to execute on the node.
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

    /// Pin to a specific node by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,

    /// Select nodes by labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<BTreeMap<String, String>>,

    /// DNS resolution mode: "host" (default) or "kubernetes".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_mode: Option<String>,

    /// Named overlay group for shared overlay filesystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_name: Option<String>,

    /// Run the process as this UID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_user: Option<i64>,

    /// Run the process as this GID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_group: Option<i64>,

    /// Supplemental group IDs for the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplemental_groups: Option<Vec<i64>>,

    /// Volumes with inline mount paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<ReaperVolume>,

    /// Restart policy for the underlying Pod. Defaults to "Never".
    #[serde(
        default = "default_restart_policy",
        skip_serializing_if = "is_default_restart_policy"
    )]
    pub restart_policy: String,

    /// Tolerations passed through to the underlying Pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tolerations: Vec<ReaperToleration>,
}

impl Default for ReaperPodSpec {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            args: Vec::new(),
            env: Vec::new(),
            working_dir: None,
            node_name: None,
            node_selector: None,
            dns_mode: None,
            overlay_name: None,
            run_as_user: None,
            run_as_group: None,
            supplemental_groups: None,
            volumes: Vec::new(),
            restart_policy: default_restart_policy(),
            tolerations: Vec::new(),
        }
    }
}

fn default_restart_policy() -> String {
    "Never".to_string()
}

fn is_default_restart_policy(s: &str) -> bool {
    s == "Never"
}

/// Simplified environment variable (name + literal value or secret/configmap ref).
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReaperEnvVar {
    /// Environment variable name.
    pub name: String,

    /// Literal value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,

    /// Reference to a Secret key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key_ref: Option<KeyRef>,

    /// Reference to a ConfigMap key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map_key_ref: Option<KeyRef>,
}

/// Reference to a key in a Secret or ConfigMap.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct KeyRef {
    /// Name of the Secret or ConfigMap.
    pub name: String,
    /// Key within the Secret or ConfigMap.
    pub key: String,
}

/// A simplified volume definition with inline mountPath.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReaperVolume {
    /// Volume name (used internally).
    pub name: String,

    /// Path inside the overlay where this volume is mounted.
    pub mount_path: String,

    /// Mount as read-only.
    #[serde(default)]
    pub read_only: bool,

    /// ConfigMap name to mount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map: Option<String>,

    /// Secret name to mount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,

    /// Host path to mount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<String>,

    /// Use an emptyDir volume.
    #[serde(default)]
    pub empty_dir: bool,
}

/// Simplified toleration.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReaperToleration {
    /// Toleration key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Operator: "Exists" or "Equal".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,

    /// Value to match (when operator is "Equal").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,

    /// Effect: "NoSchedule", "PreferNoSchedule", or "NoExecute".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
}

/// Status of a ReaperPod.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReaperPodStatus {
    /// Current phase: Pending, Running, Succeeded, Failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// Name of the underlying Pod created by the controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_name: Option<String>,

    /// Node where the Pod was scheduled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,

    /// When the Pod started running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,

    /// When the Pod completed (succeeded or failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_time: Option<String>,

    /// Exit code of the main process (set when phase is Succeeded or Failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,

    /// Human-readable message about the current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
