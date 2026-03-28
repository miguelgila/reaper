use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Node;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::{
    api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams},
    runtime::controller::{Action, Controller},
    Client, ResourceExt,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use reaper::crds::{
    DaemonJobNodeStatus, ReaperDaemonJob, ReaperDaemonJobStatus, ReaperPod, ReaperPodSpec,
};

const DAEMONJOB_LABEL: &str = "reaper.giar.dev/daemon-job";
const NODE_LABEL: &str = "reaper.giar.dev/daemon-job-node";

/// Shared state for the controller.
pub struct Context {
    pub client: Client,
}

/// Main entry point: run the ReaperDaemonJob controller loop.
pub async fn run(client: Client) -> Result<()> {
    let daemon_jobs: Api<ReaperDaemonJob> = Api::all(client.clone());
    let reaper_pods: Api<ReaperPod> = Api::all(client.clone());

    let context = Arc::new(Context {
        client: client.clone(),
    });

    info!("starting ReaperDaemonJob controller");

    Controller::new(daemon_jobs, Default::default())
        .owns(reaper_pods, Default::default())
        .run(reconcile, error_policy, context)
        .for_each(|result| async move {
            match result {
                Ok((obj, _action)) => {
                    debug!(name = %obj.name, namespace = obj.namespace.as_deref().unwrap_or(""), "reconciled ReaperDaemonJob");
                }
                Err(e) => {
                    warn!(error = %e, "ReaperDaemonJob reconcile error");
                }
            }
        })
        .await;

    Ok(())
}

/// Reconcile a single ReaperDaemonJob.
async fn reconcile(dj: Arc<ReaperDaemonJob>, ctx: Arc<Context>) -> Result<Action, kube::Error> {
    let name = dj.name_any();
    let namespace = dj.namespace().unwrap_or_else(|| "default".to_string());
    let generation = dj.metadata.generation.unwrap_or(0);

    debug!(name = %name, namespace = %namespace, "reconciling ReaperDaemonJob");

    let dj_api: Api<ReaperDaemonJob> = Api::namespaced(ctx.client.clone(), &namespace);
    let rp_api: Api<ReaperPod> = Api::namespaced(ctx.client.clone(), &namespace);
    let nodes_api: Api<Node> = Api::all(ctx.client.clone());

    // List matching nodes
    let target_nodes = list_ready_nodes(&nodes_api, dj.spec.node_selector.as_ref()).await?;
    let total_nodes = target_nodes.len() as i32;

    if target_nodes.is_empty() {
        let status = ReaperDaemonJobStatus {
            phase: Some("Pending".to_string()),
            total_nodes: 0,
            ready_nodes: 0,
            observed_generation: generation,
            message: Some("No matching ready nodes found".to_string()),
            ..Default::default()
        };
        patch_status(&dj_api, &name, &status).await?;
        return Ok(Action::requeue(std::time::Duration::from_secs(30)));
    }

    // Check if spec changed (generation differs from observed) — re-trigger all
    let observed_gen = dj
        .status
        .as_ref()
        .map(|s| s.observed_generation)
        .unwrap_or(0);
    let spec_changed = generation != observed_gen;

    // List existing ReaperPods owned by this DaemonJob
    let owned_pods = rp_api
        .list(&ListParams::default().labels(&format!("{}={}", DAEMONJOB_LABEL, name)))
        .await?;

    let owned_map: BTreeMap<String, &ReaperPod> = owned_pods
        .items
        .iter()
        .filter_map(|rp| {
            rp.metadata
                .labels
                .as_ref()
                .and_then(|l| l.get(NODE_LABEL))
                .map(|node| (node.clone(), rp))
        })
        .collect();

    // Check dependencies (after: [...])
    if !dj.spec.after.is_empty() {
        for dep_name in &dj.spec.after {
            match dj_api.get(dep_name).await {
                Ok(dep) => {
                    let dep_phase = dep
                        .status
                        .as_ref()
                        .and_then(|s| s.phase.as_deref())
                        .unwrap_or("Pending");
                    if dep_phase != "Completed" {
                        info!(
                            name = %name,
                            dependency = %dep_name,
                            dep_phase = %dep_phase,
                            "dependency not Completed, keeping Pending"
                        );
                        let status = ReaperDaemonJobStatus {
                            phase: Some("Pending".to_string()),
                            total_nodes,
                            observed_generation: generation,
                            message: Some(format!(
                                "Waiting for dependency '{}' to complete (current: {})",
                                dep_name, dep_phase
                            )),
                            ..Default::default()
                        };
                        patch_status(&dj_api, &name, &status).await?;
                        return Ok(Action::requeue(std::time::Duration::from_secs(10)));
                    }
                }
                Err(kube::Error::Api(ref resp)) if resp.code == 404 => {
                    info!(name = %name, dependency = %dep_name, "dependency not found, keeping Pending");
                    let status = ReaperDaemonJobStatus {
                        phase: Some("Pending".to_string()),
                        total_nodes,
                        observed_generation: generation,
                        message: Some(format!(
                            "Waiting for dependency '{}' to be created",
                            dep_name
                        )),
                        ..Default::default()
                    };
                    patch_status(&dj_api, &name, &status).await?;
                    return Ok(Action::requeue(std::time::Duration::from_secs(10)));
                }
                Err(e) => {
                    warn!(name = %name, dependency = %dep_name, error = %e, "failed to check dependency");
                }
            }
        }
    }

    // For each target node, ensure a ReaperPod exists
    let mut node_statuses: Vec<DaemonJobNodeStatus> = Vec::new();
    let mut ready_count = 0i32;
    let mut any_running = false;
    let mut any_failed = false;

    for node_name in &target_nodes {
        let rp_name = format!("{}-{}", name, node_name);

        if let Some(existing_rp) = owned_map.get(node_name) {
            let rp_phase = existing_rp
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .unwrap_or("Pending");

            let exit_code = existing_rp.status.as_ref().and_then(|s| s.exit_code);
            let start_time = existing_rp
                .status
                .as_ref()
                .and_then(|s| s.start_time.clone());
            let completion_time = existing_rp
                .status
                .as_ref()
                .and_then(|s| s.completion_time.clone());

            // Find existing node status for retry count
            let prev_retry = dj
                .status
                .as_ref()
                .and_then(|s| s.node_statuses.iter().find(|ns| ns.node_name == *node_name))
                .map(|ns| ns.retry_count)
                .unwrap_or(0);

            match rp_phase {
                "Succeeded" => {
                    ready_count += 1;
                    node_statuses.push(DaemonJobNodeStatus {
                        node_name: node_name.clone(),
                        phase: "Succeeded".to_string(),
                        reaper_pod_name: Some(rp_name),
                        exit_code,
                        retry_count: prev_retry,
                        last_start_time: start_time,
                        last_completion_time: completion_time,
                    });
                }
                "Failed" => {
                    // Check if we should retry
                    if prev_retry < dj.spec.retry_limit {
                        info!(
                            name = %name, node = %node_name,
                            retry = prev_retry + 1, limit = dj.spec.retry_limit,
                            "retrying failed job on node"
                        );
                        // Delete the failed ReaperPod to trigger re-creation
                        let _ = rp_api
                            .delete(&existing_rp.name_any(), &DeleteParams::default())
                            .await;
                        node_statuses.push(DaemonJobNodeStatus {
                            node_name: node_name.clone(),
                            phase: "Pending".to_string(),
                            reaper_pod_name: None,
                            exit_code: None,
                            retry_count: prev_retry + 1,
                            last_start_time: None,
                            last_completion_time: None,
                        });
                        any_running = true;
                    } else {
                        any_failed = true;
                        node_statuses.push(DaemonJobNodeStatus {
                            node_name: node_name.clone(),
                            phase: "Failed".to_string(),
                            reaper_pod_name: Some(rp_name),
                            exit_code,
                            retry_count: prev_retry,
                            last_start_time: start_time,
                            last_completion_time: completion_time,
                        });
                    }
                }
                _ => {
                    // Pending or Running
                    if spec_changed && rp_phase != "Running" {
                        // Spec changed — delete and re-create
                        info!(name = %name, node = %node_name, "spec changed, re-creating ReaperPod");
                        let _ = rp_api
                            .delete(&existing_rp.name_any(), &DeleteParams::default())
                            .await;
                        node_statuses.push(DaemonJobNodeStatus {
                            node_name: node_name.clone(),
                            phase: "Pending".to_string(),
                            reaper_pod_name: None,
                            exit_code: None,
                            retry_count: 0,
                            last_start_time: None,
                            last_completion_time: None,
                        });
                    } else {
                        any_running = true;
                        node_statuses.push(DaemonJobNodeStatus {
                            node_name: node_name.clone(),
                            phase: rp_phase.to_string(),
                            reaper_pod_name: Some(rp_name),
                            exit_code: None,
                            retry_count: prev_retry,
                            last_start_time: start_time,
                            last_completion_time: None,
                        });
                    }
                }
            }
        } else {
            // No ReaperPod for this node — create one
            let rp = build_reaper_pod(&dj, &rp_name, node_name);
            info!(name = %name, node = %node_name, rp_name = %rp_name, "creating ReaperPod for node");
            match rp_api.create(&PostParams::default(), &rp).await {
                Ok(_) => {
                    any_running = true;
                    let prev_retry = dj
                        .status
                        .as_ref()
                        .and_then(|s| s.node_statuses.iter().find(|ns| ns.node_name == *node_name))
                        .map(|ns| ns.retry_count)
                        .unwrap_or(0);
                    node_statuses.push(DaemonJobNodeStatus {
                        node_name: node_name.clone(),
                        phase: "Pending".to_string(),
                        reaper_pod_name: Some(rp_name),
                        exit_code: None,
                        retry_count: prev_retry,
                        last_start_time: None,
                        last_completion_time: None,
                    });
                }
                Err(kube::Error::Api(ref resp)) if resp.code == 409 => {
                    // Already exists (race condition) — will reconcile next time
                    debug!(rp_name = %rp_name, "ReaperPod already exists");
                    any_running = true;
                    node_statuses.push(DaemonJobNodeStatus {
                        node_name: node_name.clone(),
                        phase: "Pending".to_string(),
                        reaper_pod_name: Some(rp_name),
                        exit_code: None,
                        retry_count: 0,
                        last_start_time: None,
                        last_completion_time: None,
                    });
                }
                Err(e) => return Err(e),
            }
        }
    }

    // Determine overall phase
    let phase = if ready_count == total_nodes {
        "Completed"
    } else if any_failed && !any_running {
        "PartiallyFailed"
    } else {
        "Running"
    };

    let message = Some(format!("{}/{} nodes completed", ready_count, total_nodes));

    let status = ReaperDaemonJobStatus {
        phase: Some(phase.to_string()),
        ready_nodes: ready_count,
        total_nodes,
        node_statuses,
        observed_generation: generation,
        message,
    };
    patch_status(&dj_api, &name, &status).await?;

    // Requeue: faster when running, slower when completed
    let requeue_secs = if phase == "Completed" { 60 } else { 10 };
    Ok(Action::requeue(std::time::Duration::from_secs(
        requeue_secs,
    )))
}

/// List Ready nodes matching the optional node selector.
async fn list_ready_nodes(
    nodes_api: &Api<Node>,
    node_selector: Option<&BTreeMap<String, String>>,
) -> Result<Vec<String>, kube::Error> {
    let label_selector = node_selector
        .map(|sel| {
            sel.iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();

    let lp = if label_selector.is_empty() {
        ListParams::default()
    } else {
        ListParams::default().labels(&label_selector)
    };

    let nodes = nodes_api.list(&lp).await?;

    Ok(nodes
        .items
        .into_iter()
        .filter(|node| is_node_ready(node))
        .filter_map(|node| node.metadata.name)
        .collect())
}

/// Check if a Node has condition Ready=True.
fn is_node_ready(node: &Node) -> bool {
    node.status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .map(|conditions| {
            conditions
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
        .unwrap_or(false)
}

/// Build a ReaperPod for a specific node from the DaemonJob spec.
fn build_reaper_pod(dj: &ReaperDaemonJob, rp_name: &str, node_name: &str) -> ReaperPod {
    let dj_name = dj.name_any();
    let namespace = dj.namespace().unwrap_or_else(|| "default".to_string());

    let spec = ReaperPodSpec {
        command: dj.spec.command.clone(),
        args: dj.spec.args.clone(),
        env: dj.spec.env.clone(),
        working_dir: dj.spec.working_dir.clone(),
        node_name: Some(node_name.to_string()),
        node_selector: None, // We pin to specific node via node_name
        dns_mode: dj.spec.dns_mode.clone(),
        overlay_name: dj.spec.overlay_name.clone(),
        run_as_user: dj.spec.run_as_user,
        run_as_group: dj.spec.run_as_group,
        supplemental_groups: None,
        volumes: dj.spec.volumes.clone(),
        restart_policy: "Never".to_string(),
        tolerations: dj.spec.tolerations.clone(),
    };

    let owner_ref = OwnerReference {
        api_version: "reaper.giar.dev/v1alpha1".to_string(),
        kind: "ReaperDaemonJob".to_string(),
        name: dj_name.clone(),
        uid: dj.metadata.uid.clone().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };

    let mut labels = BTreeMap::new();
    labels.insert(DAEMONJOB_LABEL.to_string(), dj_name);
    labels.insert(NODE_LABEL.to_string(), node_name.to_string());

    ReaperPod::new(rp_name, ReaperPodSpec { ..spec }).with_metadata(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(rp_name.to_string()),
            namespace: Some(namespace),
            labels: Some(labels),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
    )
}

/// Patch the ReaperDaemonJob status subresource.
async fn patch_status(
    api: &Api<ReaperDaemonJob>,
    name: &str,
    status: &ReaperDaemonJobStatus,
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
fn error_policy(_dj: Arc<ReaperDaemonJob>, error: &kube::Error, _ctx: Arc<Context>) -> Action {
    error!(error = %error, "ReaperDaemonJob reconcile error, will retry");
    Action::requeue(std::time::Duration::from_secs(10))
}

/// Helper to construct a ReaperPod with custom metadata.
trait WithMetadata {
    fn with_metadata(
        self,
        meta: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    ) -> Self;
}

impl WithMetadata for ReaperPod {
    fn with_metadata(
        mut self,
        meta: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    ) -> Self {
        self.metadata = meta;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{NodeCondition, NodeStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn make_node(name: &str, ready: bool) -> Node {
        Node {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            status: Some(NodeStatus {
                conditions: Some(vec![NodeCondition {
                    type_: "Ready".to_string(),
                    status: if ready { "True" } else { "False" }.to_string(),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_is_node_ready_true() {
        let node = make_node("worker-1", true);
        assert!(is_node_ready(&node));
    }

    #[test]
    fn test_is_node_ready_false() {
        let node = make_node("worker-2", false);
        assert!(!is_node_ready(&node));
    }

    #[test]
    fn test_is_node_ready_no_conditions() {
        let node = Node {
            metadata: ObjectMeta {
                name: Some("worker-3".to_string()),
                ..Default::default()
            },
            status: Some(NodeStatus {
                conditions: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!is_node_ready(&node));
    }

    #[test]
    fn test_is_node_ready_no_status() {
        let node = Node {
            metadata: ObjectMeta {
                name: Some("worker-4".to_string()),
                ..Default::default()
            },
            status: None,
            ..Default::default()
        };
        assert!(!is_node_ready(&node));
    }

    #[test]
    fn test_build_reaper_pod_basic() {
        let dj = ReaperDaemonJob::new(
            "my-job",
            ReaperDaemonJobSpec {
                command: vec!["/bin/sh".to_string(), "-c".to_string()],
                args: vec!["echo hello".to_string()],
                overlay_name: Some("shared".to_string()),
                ..Default::default()
            },
        );

        let rp = build_reaper_pod(&dj, "my-job-worker-1", "worker-1");
        assert_eq!(rp.metadata.name.as_deref(), Some("my-job-worker-1"));
        assert_eq!(rp.spec.command, vec!["/bin/sh", "-c"]);
        assert_eq!(rp.spec.args, vec!["echo hello"]);
        assert_eq!(rp.spec.node_name.as_deref(), Some("worker-1"));
        assert_eq!(rp.spec.overlay_name.as_deref(), Some("shared"));
        assert_eq!(rp.spec.restart_policy, "Never");

        // Check labels
        let labels = rp.metadata.labels.as_ref().unwrap();
        assert_eq!(
            labels.get(DAEMONJOB_LABEL).map(|s| s.as_str()),
            Some("my-job")
        );
        assert_eq!(labels.get(NODE_LABEL).map(|s| s.as_str()), Some("worker-1"));

        // Check owner reference
        let owner_refs = rp.metadata.owner_references.as_ref().unwrap();
        assert_eq!(owner_refs.len(), 1);
        assert_eq!(owner_refs[0].kind, "ReaperDaemonJob");
        assert_eq!(owner_refs[0].name, "my-job");
        assert_eq!(owner_refs[0].controller, Some(true));
    }

    #[test]
    fn test_build_reaper_pod_inherits_fields() {
        let dj = ReaperDaemonJob::new(
            "full-job",
            ReaperDaemonJobSpec {
                command: vec!["/usr/bin/ansible-playbook".to_string()],
                args: vec!["site.yml".to_string()],
                dns_mode: Some("kubernetes".to_string()),
                run_as_user: Some(1000),
                run_as_group: Some(1000),
                working_dir: Some("/opt/playbooks".to_string()),
                ..Default::default()
            },
        );

        let rp = build_reaper_pod(&dj, "full-job-node-a", "node-a");
        assert_eq!(rp.spec.dns_mode.as_deref(), Some("kubernetes"));
        assert_eq!(rp.spec.run_as_user, Some(1000));
        assert_eq!(rp.spec.run_as_group, Some(1000));
        assert_eq!(rp.spec.working_dir.as_deref(), Some("/opt/playbooks"));
    }
}
