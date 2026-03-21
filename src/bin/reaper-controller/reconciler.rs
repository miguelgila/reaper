use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, ListParams, Patch, PatchParams, PostParams},
    runtime::controller::{Action, Controller},
    Client, ResourceExt,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use reaper::crds::{ReaperOverlay, ReaperPod, ReaperPodStatus};

use crate::pod_builder::{self, OWNER_LABEL};

/// Shared state for the controller.
pub struct Context {
    pub client: Client,
}

/// Main entry point: run the controller loop.
pub async fn run(client: Client) -> Result<()> {
    let reaper_pods: Api<ReaperPod> = Api::all(client.clone());
    let pods: Api<Pod> = Api::all(client.clone());

    let context = Arc::new(Context {
        client: client.clone(),
    });

    info!("starting ReaperPod controller");

    Controller::new(reaper_pods, Default::default())
        .owns(pods, Default::default())
        .run(reconcile, error_policy, context)
        .for_each(|result| async move {
            match result {
                Ok((obj, _action)) => {
                    debug!(name = %obj.name, namespace = obj.namespace.as_deref().unwrap_or(""), "reconciled");
                }
                Err(e) => {
                    warn!(error = %e, "reconcile error");
                }
            }
        })
        .await;

    Ok(())
}

/// Reconcile a single ReaperPod.
async fn reconcile(rp: Arc<ReaperPod>, ctx: Arc<Context>) -> Result<Action, kube::Error> {
    let name = rp.name_any();
    let namespace = rp.namespace().unwrap_or_else(|| "default".to_string());

    debug!(name = %name, namespace = %namespace, "reconciling ReaperPod");

    let pods_api: Api<Pod> = Api::namespaced(ctx.client.clone(), &namespace);
    let rp_api: Api<ReaperPod> = Api::namespaced(ctx.client.clone(), &namespace);

    // Check if we already created a Pod for this ReaperPod
    let owned_pods = pods_api
        .list(&ListParams::default().labels(&format!("{}={}", OWNER_LABEL, name)))
        .await?;

    if owned_pods.items.is_empty() {
        // PVC-like check: if overlayName is set, require a Ready ReaperOverlay
        if let Some(ref overlay_name) = rp.spec.overlay_name {
            let overlay_api: Api<ReaperOverlay> = Api::namespaced(ctx.client.clone(), &namespace);
            match overlay_api.get(overlay_name).await {
                Ok(overlay) => {
                    let phase = overlay
                        .status
                        .as_ref()
                        .and_then(|s| s.phase.as_deref())
                        .unwrap_or("Pending");
                    if phase != "Ready" {
                        info!(
                            name = %name,
                            overlay = %overlay_name,
                            overlay_phase = %phase,
                            "ReaperOverlay not Ready, keeping ReaperPod Pending"
                        );
                        let status = ReaperPodStatus {
                            phase: Some("Pending".to_string()),
                            message: Some(format!(
                                "Waiting for ReaperOverlay '{}' to be Ready (current: {})",
                                overlay_name, phase
                            )),
                            ..Default::default()
                        };
                        patch_status(&rp_api, &name, &status).await?;
                        return Ok(Action::requeue(std::time::Duration::from_secs(5)));
                    }
                }
                Err(kube::Error::Api(ref resp)) if resp.code == 404 => {
                    info!(
                        name = %name,
                        overlay = %overlay_name,
                        "ReaperOverlay not found, keeping ReaperPod Pending"
                    );
                    let status = ReaperPodStatus {
                        phase: Some("Pending".to_string()),
                        message: Some(format!(
                            "Waiting for ReaperOverlay '{}' to be created",
                            overlay_name
                        )),
                        ..Default::default()
                    };
                    patch_status(&rp_api, &name, &status).await?;
                    return Ok(Action::requeue(std::time::Duration::from_secs(5)));
                }
                Err(e) => {
                    warn!(
                        name = %name,
                        overlay = %overlay_name,
                        error = %e,
                        "failed to check ReaperOverlay, proceeding anyway"
                    );
                }
            }
        }

        // No Pod yet — create one
        let pod = pod_builder::build_pod(&rp)
            .map_err(|e| kube::Error::Service(std::io::Error::other(e).into()))?;

        info!(name = %name, namespace = %namespace, "creating Pod for ReaperPod");
        let created = pods_api.create(&PostParams::default(), &pod).await?;
        let pod_name = created.metadata.name.unwrap_or_default();

        // Update status: PodCreated
        let status = ReaperPodStatus {
            phase: Some("Pending".to_string()),
            pod_name: Some(pod_name),
            ..Default::default()
        };
        patch_status(&rp_api, &name, &status).await?;
    } else {
        // Pod exists — mirror its status back
        let pod = &owned_pods.items[0];
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let pod_status = pod.status.as_ref();

        let phase = pod_status
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Pending".to_string());

        let node_name = pod.spec.as_ref().and_then(|s| s.node_name.clone());
        let start_time = pod_status
            .and_then(|s| s.start_time.as_ref())
            .map(|t| t.0.to_rfc3339());

        // Extract exit code from container status
        let (exit_code, completion_time) = extract_exit_info(pod);

        let reaper_phase = match phase.as_str() {
            "Succeeded" => "Succeeded",
            "Failed" => "Failed",
            "Running" => "Running",
            _ => "Pending",
        };

        let status = ReaperPodStatus {
            phase: Some(reaper_phase.to_string()),
            pod_name: Some(pod_name),
            node_name,
            start_time,
            completion_time,
            exit_code,
            message: pod_status.and_then(|s| s.message.clone()),
        };
        patch_status(&rp_api, &name, &status).await?;
    }

    // Re-check every 30s for status updates (or on Pod change via owns)
    Ok(Action::requeue(std::time::Duration::from_secs(30)))
}

/// Extract exit code and completion time from a Pod's container statuses.
fn extract_exit_info(pod: &Pod) -> (Option<i32>, Option<String>) {
    let container_statuses = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref());

    if let Some(statuses) = container_statuses {
        for cs in statuses {
            if let Some(ref state) = cs.state {
                if let Some(ref terminated) = state.terminated {
                    return (
                        Some(terminated.exit_code),
                        terminated.finished_at.as_ref().map(|t| t.0.to_rfc3339()),
                    );
                }
            }
        }
    }

    (None, None)
}

/// Patch the ReaperPod status subresource.
async fn patch_status(
    api: &Api<ReaperPod>,
    name: &str,
    status: &ReaperPodStatus,
) -> Result<(), kube::Error> {
    let patch = serde_json::json!({ "status": status });
    api.patch_status(
        name,
        &PatchParams::apply("reaper-controller"),
        &Patch::Merge(patch),
    )
    .await?;
    Ok(())
}

/// Error policy: requeue with backoff on errors.
fn error_policy(_rp: Arc<ReaperPod>, error: &kube::Error, _ctx: Arc<Context>) -> Action {
    error!(error = %error, "reconcile error, will retry");
    Action::requeue(std::time::Duration::from_secs(10))
}
