use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// ReaperOverlay is a PVC-like resource that manages named overlay filesystem
/// lifecycles independently from ReaperPod workloads. It provides Kubernetes-native
/// overlay creation, inspection, reset, and deletion without requiring node access.
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "reaper.io",
    version = "v1alpha1",
    kind = "ReaperOverlay",
    namespaced,
    status = "ReaperOverlayStatus",
    printcolumn = r#"{"name":"Phase", "type":"string", "jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Reset Gen", "type":"integer", "jsonPath":".spec.resetGeneration"}"#,
    printcolumn = r#"{"name":"Observed", "type":"integer", "jsonPath":".status.observedResetGeneration"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ReaperOverlaySpec {
    /// When to automatically reset the overlay.
    /// - Manual: only reset when resetGeneration is incremented (default)
    /// - OnFailure: reset when a ReaperPod using this overlay fails
    /// - OnDelete: reset when the ReaperOverlay is deleted and recreated
    #[serde(default = "default_reset_policy")]
    pub reset_policy: String,

    /// Monotonically increasing counter. Increment to trigger a reset on all nodes.
    /// Controller compares against status.observedResetGeneration to detect pending resets.
    #[serde(default)]
    pub reset_generation: i64,
}

impl Default for ReaperOverlaySpec {
    fn default() -> Self {
        Self {
            reset_policy: default_reset_policy(),
            reset_generation: 0,
        }
    }
}

fn default_reset_policy() -> String {
    "Manual".to_string()
}

/// Status of a ReaperOverlay.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReaperOverlayStatus {
    /// Current phase: Pending, Ready, Resetting, Failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// Last resetGeneration that was fully applied across all nodes.
    #[serde(default)]
    pub observed_reset_generation: i64,

    /// Per-node overlay state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<ReaperOverlayNodeStatus>,

    /// Human-readable message about the current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Per-node overlay status.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReaperOverlayNodeStatus {
    /// Name of the node.
    pub node_name: String,

    /// Whether the overlay is available on this node.
    #[serde(default)]
    pub ready: bool,

    /// ISO 8601 timestamp of last reset on this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reset_time: Option<String>,
}
