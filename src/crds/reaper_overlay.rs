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

#[cfg(test)]
mod tests {
    use super::*;

    // --- ReaperOverlaySpec ---

    #[test]
    fn test_spec_default_values() {
        let spec = ReaperOverlaySpec::default();
        assert_eq!(spec.reset_policy, "Manual");
        assert_eq!(spec.reset_generation, 0);
    }

    #[test]
    fn test_spec_serializes_with_defaults() {
        let spec = ReaperOverlaySpec::default();
        let json = serde_json::to_value(&spec).expect("serialize spec");
        assert_eq!(json["resetPolicy"], "Manual");
        assert_eq!(json["resetGeneration"], 0);
    }

    #[test]
    fn test_spec_custom_reset_policy() {
        let spec = ReaperOverlaySpec {
            reset_policy: "OnFailure".to_string(),
            reset_generation: 3,
        };
        let json = serde_json::to_value(&spec).expect("serialize spec");
        assert_eq!(json["resetPolicy"], "OnFailure");
        assert_eq!(json["resetGeneration"], 3);
    }

    #[test]
    fn test_spec_roundtrip() {
        let original = ReaperOverlaySpec {
            reset_policy: "OnDelete".to_string(),
            reset_generation: 7,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ReaperOverlaySpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.reset_policy, "OnDelete");
        assert_eq!(decoded.reset_generation, 7);
    }

    #[test]
    fn test_spec_deserialize_missing_fields_uses_defaults() {
        // Both fields have serde defaults, so an empty object should deserialize fine.
        let spec: ReaperOverlaySpec = serde_json::from_str("{}").expect("deserialize empty");
        assert_eq!(spec.reset_policy, "Manual");
        assert_eq!(spec.reset_generation, 0);
    }

    // --- ReaperOverlayStatus ---

    #[test]
    fn test_status_default_is_empty() {
        let status = ReaperOverlayStatus::default();
        assert!(status.phase.is_none());
        assert_eq!(status.observed_reset_generation, 0);
        assert!(status.nodes.is_empty());
        assert!(status.message.is_none());
    }

    #[test]
    fn test_status_optional_fields_omitted_when_absent() {
        let status = ReaperOverlayStatus::default();
        let json = serde_json::to_value(&status).expect("serialize status");
        assert!(json.get("phase").is_none(), "phase should be omitted");
        assert!(json.get("message").is_none(), "message should be omitted");
        assert!(json.get("nodes").is_none(), "empty nodes should be omitted");
        // observedResetGeneration has serde(default) but no skip_serializing_if
        assert_eq!(json["observedResetGeneration"], 0);
    }

    #[test]
    fn test_status_roundtrip_with_all_fields() {
        let original = ReaperOverlayStatus {
            phase: Some("Ready".to_string()),
            observed_reset_generation: 5,
            nodes: vec![ReaperOverlayNodeStatus {
                node_name: "worker-1".to_string(),
                ready: true,
                last_reset_time: Some("2026-03-19T12:00:00Z".to_string()),
            }],
            message: Some("all nodes healthy".to_string()),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ReaperOverlayStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.phase.as_deref(), Some("Ready"));
        assert_eq!(decoded.observed_reset_generation, 5);
        assert_eq!(decoded.nodes.len(), 1);
        assert_eq!(decoded.nodes[0].node_name, "worker-1");
        assert!(decoded.nodes[0].ready);
        assert_eq!(
            decoded.nodes[0].last_reset_time.as_deref(),
            Some("2026-03-19T12:00:00Z")
        );
        assert_eq!(decoded.message.as_deref(), Some("all nodes healthy"));
    }

    // --- ReaperOverlayNodeStatus ---

    #[test]
    fn test_node_status_without_last_reset_time() {
        let ns = ReaperOverlayNodeStatus {
            node_name: "node-a".to_string(),
            ready: false,
            last_reset_time: None,
        };
        let json = serde_json::to_value(&ns).expect("serialize node status");
        assert_eq!(json["nodeName"], "node-a");
        assert_eq!(json["ready"], false);
        assert!(
            json.get("lastResetTime").is_none(),
            "lastResetTime should be omitted when None"
        );
    }

    #[test]
    fn test_node_status_with_last_reset_time() {
        let ns = ReaperOverlayNodeStatus {
            node_name: "node-b".to_string(),
            ready: true,
            last_reset_time: Some("2026-03-19T08:30:00Z".to_string()),
        };
        let json = serde_json::to_value(&ns).expect("serialize node status");
        assert_eq!(json["nodeName"], "node-b");
        assert_eq!(json["ready"], true);
        assert_eq!(json["lastResetTime"], "2026-03-19T08:30:00Z");
    }

    #[test]
    fn test_node_status_roundtrip() {
        let original = ReaperOverlayNodeStatus {
            node_name: "node-c".to_string(),
            ready: true,
            last_reset_time: Some("2026-01-01T00:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ReaperOverlayNodeStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.node_name, "node-c");
        assert!(decoded.ready);
        assert_eq!(
            decoded.last_reset_time.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
    }

    #[test]
    fn test_node_status_ready_defaults_false_when_missing() {
        let json = r#"{"nodeName": "node-d"}"#;
        let ns: ReaperOverlayNodeStatus = serde_json::from_str(json).expect("deserialize");
        assert_eq!(ns.node_name, "node-d");
        assert!(!ns.ready, "ready should default to false");
        assert!(ns.last_reset_time.is_none());
    }
}
