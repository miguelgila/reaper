use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::Node;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::{api::Api, Client};
use serde_json::json;
use tracing::{debug, error, info, warn};

use crate::health;
use crate::metrics::MetricsState;

const CONDITION_TYPE: &str = "ReaperReady";

/// Patch the Node's status conditions with the current ReaperReady state.
///
/// Uses a strategic merge patch on the `/status` subresource so that the
/// `ReaperReady` condition is upserted by its `type` field without
/// disturbing other conditions (Ready, MemoryPressure, etc.).
async fn patch_node_condition(
    api: &Api<Node>,
    node_name: &str,
    healthy: bool,
    details: &[String],
) -> Result<()> {
    let now = Time(chrono::Utc::now());

    let (status, reason, message) = if healthy {
        (
            "True",
            "ReaperHealthy",
            "Reaper binaries and state directory are present and healthy".to_string(),
        )
    } else {
        let msg = if details.is_empty() {
            "Reaper health check failed".to_string()
        } else {
            details.join("; ")
        };
        ("False", "ReaperUnhealthy", msg)
    };

    let patch = json!({
        "status": {
            "conditions": [{
                "type": CONDITION_TYPE,
                "status": status,
                "lastHeartbeatTime": now,
                "lastTransitionTime": now,
                "reason": reason,
                "message": message,
            }]
        }
    });

    let patch_params = kube::api::PatchParams::default();
    api.patch_status(
        node_name,
        &patch_params,
        &kube::api::Patch::Strategic(patch),
    )
    .await
    .with_context(|| format!("patching node {} status condition", node_name))?;

    debug!(
        node = node_name,
        status = status,
        "patched ReaperReady condition"
    );

    Ok(())
}

/// Run a single node condition update cycle.
pub async fn update_node_condition(
    client: &Client,
    node_name: &str,
    shim_path: &str,
    runtime_path: &str,
    state_dir: &str,
    metrics: &MetricsState,
) {
    let result = health::check_health(shim_path, runtime_path, state_dir);
    let api: Api<Node> = Api::all(client.clone());

    match patch_node_condition(&api, node_name, result.healthy, &result.details).await {
        Ok(()) => {
            metrics.inc_node_condition_updates();
            metrics.set_node_condition_healthy(result.healthy);
        }
        Err(e) => {
            warn!(error = %e, node = node_name, "failed to patch node condition");
        }
    }
}

/// Run the node condition reporting loop at the configured interval.
pub async fn node_condition_loop(
    node_name: &str,
    shim_path: &str,
    runtime_path: &str,
    state_dir: &str,
    interval_secs: u64,
    metrics: &MetricsState,
) {
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to create Kubernetes client, node condition reporting disabled");
            return;
        }
    };

    info!(
        node = node_name,
        interval_secs = interval_secs,
        "node condition reporting starting"
    );

    // Initial patch
    update_node_condition(
        &client,
        node_name,
        shim_path,
        runtime_path,
        state_dir,
        metrics,
    )
    .await;

    let interval = tokio::time::Duration::from_secs(interval_secs);
    loop {
        tokio::time::sleep(interval).await;
        update_node_condition(
            &client,
            node_name,
            shim_path,
            runtime_path,
            state_dir,
            metrics,
        )
        .await;
    }
}
