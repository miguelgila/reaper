use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
    Client, ResourceExt,
};
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use reaper::crds::{ReaperOverlay, ReaperOverlayNodeStatus, ReaperOverlayStatus};

const FINALIZER: &str = "reaper.io/overlay-cleanup";
const AGENT_LABEL: &str = "app.kubernetes.io/component=agent,app.kubernetes.io/name=reaper";

/// Shared state for the overlay controller.
pub struct Context {
    pub client: Client,
}

/// Main entry point: run the ReaperOverlay controller loop.
pub async fn run(client: Client) -> Result<()> {
    let overlays: Api<ReaperOverlay> = Api::all(client.clone());

    let context = Arc::new(Context {
        client: client.clone(),
    });

    info!("starting ReaperOverlay controller");

    Controller::new(overlays, Default::default())
        .run(reconcile, error_policy, context)
        .for_each(|result| async move {
            match result {
                Ok((obj, _action)) => {
                    debug!(name = %obj.name, namespace = obj.namespace.as_deref().unwrap_or(""), "reconciled overlay");
                }
                Err(e) => {
                    warn!(error = %e, "overlay reconcile error");
                }
            }
        })
        .await;

    Ok(())
}

/// Reconcile a single ReaperOverlay.
async fn reconcile(overlay: Arc<ReaperOverlay>, ctx: Arc<Context>) -> Result<Action, kube::Error> {
    let name = overlay.name_any();
    let namespace = overlay.namespace().unwrap_or_else(|| "default".to_string());

    debug!(name = %name, namespace = %namespace, "reconciling ReaperOverlay");

    let overlay_api: Api<ReaperOverlay> = Api::namespaced(ctx.client.clone(), &namespace);

    // Step 1: Ensure finalizer is present
    if !has_finalizer(&overlay) {
        add_finalizer(&overlay_api, &name).await?;
        return Ok(Action::requeue(std::time::Duration::from_secs(1)));
    }

    // Step 2: Handle deletion (finalizer cleanup)
    if overlay.metadata.deletion_timestamp.is_some() {
        return handle_deletion(&overlay, &overlay_api, &name, &namespace, &ctx).await;
    }

    let spec_gen = overlay.spec.reset_generation;
    let observed_gen = overlay
        .status
        .as_ref()
        .map(|s| s.observed_reset_generation)
        .unwrap_or(0);

    // Step 3: Handle reset if generation has advanced
    if spec_gen > observed_gen {
        return handle_reset(&overlay_api, &name, &namespace, spec_gen, &ctx).await;
    }

    // Step 4: Update status — query agents for overlay state
    update_overlay_status(&overlay, &overlay_api, &name, &namespace, &ctx).await?;

    // Re-check every 60s for status updates
    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

fn has_finalizer(overlay: &ReaperOverlay) -> bool {
    overlay
        .metadata
        .finalizers
        .as_ref()
        .is_some_and(|f| f.iter().any(|s| s == FINALIZER))
}

async fn add_finalizer(api: &Api<ReaperOverlay>, name: &str) -> Result<(), kube::Error> {
    let patch = json!({
        "metadata": {
            "finalizers": [FINALIZER]
        }
    });
    api.patch(
        name,
        &PatchParams::apply("reaper-controller"),
        &Patch::Merge(patch),
    )
    .await?;
    info!(name = %name, "added finalizer to ReaperOverlay");
    Ok(())
}

async fn remove_finalizer(api: &Api<ReaperOverlay>, name: &str) -> Result<(), kube::Error> {
    let patch = json!({
        "metadata": {
            "finalizers": []
        }
    });
    api.patch(
        name,
        &PatchParams::apply("reaper-controller"),
        &Patch::Merge(patch),
    )
    .await?;
    info!(name = %name, "removed finalizer from ReaperOverlay");
    Ok(())
}

/// Handle ReaperOverlay deletion: reset overlay on all nodes, then remove finalizer.
async fn handle_deletion(
    _overlay: &ReaperOverlay,
    overlay_api: &Api<ReaperOverlay>,
    name: &str,
    namespace: &str,
    ctx: &Context,
) -> Result<Action, kube::Error> {
    info!(name = %name, namespace = %namespace, "ReaperOverlay being deleted, cleaning up overlays on nodes");

    patch_status(
        overlay_api,
        name,
        &ReaperOverlayStatus {
            phase: Some("Deleting".to_string()),
            message: Some("Cleaning up overlay on all nodes".to_string()),
            ..Default::default()
        },
    )
    .await?;

    let agents = discover_agents(&ctx.client).await?;
    let mut all_cleaned = true;

    for (node_name, pod_ip) in &agents {
        match call_agent_delete_overlay(pod_ip, namespace, name).await {
            Ok(()) => {
                info!(node = %node_name, "overlay cleaned on node");
            }
            Err(e) => {
                // 404 means overlay doesn't exist on this node — that's fine
                if e.to_string().contains("404") {
                    debug!(node = %node_name, "overlay not present on node (404)");
                } else {
                    warn!(node = %node_name, error = %e, "failed to clean overlay on node");
                    all_cleaned = false;
                }
            }
        }
    }

    if all_cleaned {
        remove_finalizer(overlay_api, name).await?;
        Ok(Action::await_change())
    } else {
        // Retry cleanup
        Ok(Action::requeue(std::time::Duration::from_secs(10)))
    }
}

/// Handle a reset: tell all agents to delete the overlay, then update observed generation.
async fn handle_reset(
    overlay_api: &Api<ReaperOverlay>,
    name: &str,
    namespace: &str,
    target_generation: i64,
    ctx: &Context,
) -> Result<Action, kube::Error> {
    info!(
        name = %name,
        namespace = %namespace,
        target_generation = target_generation,
        "resetting overlay on all nodes"
    );

    patch_status(
        overlay_api,
        name,
        &ReaperOverlayStatus {
            phase: Some("Resetting".to_string()),
            message: Some(format!(
                "Resetting overlay to generation {}",
                target_generation
            )),
            ..Default::default()
        },
    )
    .await?;

    let agents = discover_agents(&ctx.client).await?;
    let mut all_reset = true;
    let mut node_statuses = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();

    for (node_name, pod_ip) in &agents {
        match call_agent_delete_overlay(pod_ip, namespace, name).await {
            Ok(()) => {
                info!(node = %node_name, "overlay reset on node");
                node_statuses.push(ReaperOverlayNodeStatus {
                    node_name: node_name.clone(),
                    ready: true,
                    last_reset_time: Some(now.clone()),
                });
            }
            Err(e) => {
                // 404 means overlay doesn't exist — still counts as "reset" (clean state)
                if e.to_string().contains("404") {
                    debug!(node = %node_name, "overlay not present on node (404), treating as clean");
                    node_statuses.push(ReaperOverlayNodeStatus {
                        node_name: node_name.clone(),
                        ready: true,
                        last_reset_time: Some(now.clone()),
                    });
                } else {
                    warn!(node = %node_name, error = %e, "failed to reset overlay on node");
                    node_statuses.push(ReaperOverlayNodeStatus {
                        node_name: node_name.clone(),
                        ready: false,
                        last_reset_time: None,
                    });
                    all_reset = false;
                }
            }
        }
    }

    if all_reset {
        patch_status(
            overlay_api,
            name,
            &ReaperOverlayStatus {
                phase: Some("Ready".to_string()),
                observed_reset_generation: target_generation,
                nodes: node_statuses,
                message: None,
            },
        )
        .await?;
        Ok(Action::requeue(std::time::Duration::from_secs(60)))
    } else {
        patch_status(
            overlay_api,
            name,
            &ReaperOverlayStatus {
                phase: Some("Failed".to_string()),
                observed_reset_generation: target_generation - 1,
                nodes: node_statuses,
                message: Some("Reset failed on some nodes, will retry".to_string()),
            },
        )
        .await?;
        Ok(Action::requeue(std::time::Duration::from_secs(10)))
    }
}

/// Update overlay status by querying agents for overlay presence.
async fn update_overlay_status(
    overlay: &ReaperOverlay,
    overlay_api: &Api<ReaperOverlay>,
    name: &str,
    namespace: &str,
    ctx: &Context,
) -> Result<(), kube::Error> {
    let agents = discover_agents(&ctx.client).await?;
    let mut node_statuses = Vec::new();

    for (node_name, pod_ip) in &agents {
        let ready = check_agent_overlay_exists(pod_ip, namespace, name).await;
        node_statuses.push(ReaperOverlayNodeStatus {
            node_name: node_name.clone(),
            ready,
            last_reset_time: None,
        });
    }

    let current_gen = overlay
        .status
        .as_ref()
        .map(|s| s.observed_reset_generation)
        .unwrap_or(0);

    // Merge last_reset_time from existing status
    if let Some(ref existing_status) = overlay.status {
        for ns in &mut node_statuses {
            if let Some(existing) = existing_status
                .nodes
                .iter()
                .find(|n| n.node_name == ns.node_name)
            {
                ns.last_reset_time = existing.last_reset_time.clone();
            }
        }
    }

    patch_status(
        overlay_api,
        name,
        &ReaperOverlayStatus {
            phase: Some("Ready".to_string()),
            observed_reset_generation: current_gen,
            nodes: node_statuses,
            message: None,
        },
    )
    .await?;

    Ok(())
}

/// Discover all reaper-agent pods and return (node_name, pod_ip) pairs.
async fn discover_agents(client: &Client) -> Result<Vec<(String, String)>, kube::Error> {
    let pods_api: Api<Pod> = Api::all(client.clone());
    let agents = pods_api
        .list(&ListParams::default().labels(AGENT_LABEL))
        .await?;

    let mut result = Vec::new();
    for pod in &agents.items {
        let node_name = pod
            .spec
            .as_ref()
            .and_then(|s| s.node_name.clone())
            .unwrap_or_default();
        let pod_ip = pod
            .status
            .as_ref()
            .and_then(|s| s.pod_ip.clone())
            .unwrap_or_default();

        if !pod_ip.is_empty() {
            result.push((node_name, pod_ip));
        }
    }

    debug!(count = result.len(), "discovered reaper-agent pods");
    Ok(result)
}

/// Call agent DELETE /api/v1/overlays/{namespace}/{name} to remove overlay on a node.
async fn call_agent_delete_overlay(
    pod_ip: &str,
    namespace: &str,
    name: &str,
) -> Result<(), anyhow::Error> {
    let url = format!(
        "http://{}:9100/api/v1/overlays/{}/{}",
        pod_ip, namespace, name
    );
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;

    if resp.status().is_success() || resp.status().as_u16() == 404 {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{} response from agent: {}", status, body);
    }
}

/// Check if an overlay exists on a node by calling GET /api/v1/overlays/{namespace}/{name}.
async fn check_agent_overlay_exists(pod_ip: &str, namespace: &str, name: &str) -> bool {
    let url = format!(
        "http://{}:9100/api/v1/overlays/{}/{}",
        pod_ip, namespace, name
    );
    let client = reqwest::Client::new();
    match client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Patch the ReaperOverlay status subresource.
async fn patch_status(
    api: &Api<ReaperOverlay>,
    name: &str,
    status: &ReaperOverlayStatus,
) -> Result<(), kube::Error> {
    let patch = json!({ "status": status });
    api.patch_status(
        name,
        &PatchParams::apply("reaper-controller"),
        &Patch::Merge(patch),
    )
    .await?;
    Ok(())
}

/// Error policy: requeue with backoff on errors.
fn error_policy(_overlay: Arc<ReaperOverlay>, error: &kube::Error, _ctx: Arc<Context>) -> Action {
    error!(error = %error, "overlay reconcile error, will retry");
    Action::requeue(std::time::Duration::from_secs(10))
}
