use anyhow::Result;
use k8s_openapi::api::core::v1::{
    ConfigMapKeySelector, ConfigMapVolumeSource, Container, EmptyDirVolumeSource, EnvVar,
    EnvVarSource, HostPathVolumeSource, Pod, PodSecurityContext, PodSpec, SecretKeySelector,
    SecretVolumeSource, SecurityContext, Toleration, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use reaper::crds::{ReaperEnvVar, ReaperPod, ReaperToleration, ReaperVolume};
use std::collections::BTreeMap;

/// Label applied to all Pods created by the controller.
pub const OWNER_LABEL: &str = "reaper.io/owner";

/// Placeholder image — pulled by kubelet but ignored by Reaper runtime.
const PLACEHOLDER_IMAGE: &str = "busybox:latest";

/// Build a Kubernetes Pod from a ReaperPod custom resource.
pub fn build_pod(rp: &ReaperPod) -> Result<Pod> {
    let name = rp.metadata.name.as_deref().unwrap_or("unknown");
    let namespace = rp.metadata.namespace.as_deref().unwrap_or("default");
    let uid = rp.metadata.uid.as_deref().unwrap_or_default();
    let spec = &rp.spec;

    // Build annotations for Reaper-specific settings
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    if let Some(ref dns_mode) = spec.dns_mode {
        annotations.insert("reaper.runtime/dns-mode".to_string(), dns_mode.clone());
    }
    if let Some(ref overlay_name) = spec.overlay_name {
        annotations.insert(
            "reaper.runtime/overlay-name".to_string(),
            overlay_name.clone(),
        );
    }

    // Build volumes and volume mounts from the simplified spec
    let (volumes, volume_mounts) = build_volumes(&spec.volumes);

    // Build env vars
    let env_vars = build_env_vars(&spec.env);

    // Build security context if user/group specified
    let security_context = if spec.run_as_user.is_some() || spec.run_as_group.is_some() {
        Some(SecurityContext {
            run_as_user: spec.run_as_user,
            run_as_group: spec.run_as_group,
            ..Default::default()
        })
    } else {
        None
    };

    // Build the container
    let container = Container {
        name: "reaper".to_string(),
        image: Some(PLACEHOLDER_IMAGE.to_string()),
        command: Some(spec.command.clone()),
        args: if spec.args.is_empty() {
            None
        } else {
            Some(spec.args.clone())
        },
        env: if env_vars.is_empty() {
            None
        } else {
            Some(env_vars)
        },
        working_dir: spec.working_dir.clone(),
        volume_mounts: if volume_mounts.is_empty() {
            None
        } else {
            Some(volume_mounts)
        },
        security_context,
        ..Default::default()
    };

    // Build Pod labels
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert(OWNER_LABEL.to_string(), name.to_string());

    // Owner reference for garbage collection
    let owner_ref = OwnerReference {
        api_version: "reaper.io/v1alpha1".to_string(),
        kind: "ReaperPod".to_string(),
        name: name.to_string(),
        uid: uid.to_string(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };

    // Build tolerations
    let tolerations = build_tolerations(&spec.tolerations);

    // Pod-level security context for supplemental groups
    let pod_security_context = spec
        .supplemental_groups
        .as_ref()
        .map(|groups| PodSecurityContext {
            supplemental_groups: Some(groups.clone()),
            ..Default::default()
        });

    let pod = Pod {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            annotations: if annotations.is_empty() {
                None
            } else {
                Some(annotations)
            },
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(PodSpec {
            runtime_class_name: Some("reaper-v2".to_string()),
            containers: vec![container],
            restart_policy: Some(spec.restart_policy.clone()),
            node_name: spec.node_name.clone(),
            node_selector: spec.node_selector.clone(),
            security_context: pod_security_context,
            tolerations: if tolerations.is_empty() {
                None
            } else {
                Some(tolerations)
            },
            volumes: if volumes.is_empty() {
                None
            } else {
                Some(volumes)
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    Ok(pod)
}

/// Convert ReaperEnvVar list into k8s EnvVar list.
fn build_env_vars(reaper_envs: &[ReaperEnvVar]) -> Vec<EnvVar> {
    reaper_envs
        .iter()
        .map(|e| {
            let value_from = if let Some(ref secret_ref) = e.secret_key_ref {
                Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: secret_ref.name.clone(),
                        key: secret_ref.key.clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            } else {
                e.config_map_key_ref.as_ref().map(|cm_ref| EnvVarSource {
                    config_map_key_ref: Some(ConfigMapKeySelector {
                        name: cm_ref.name.clone(),
                        key: cm_ref.key.clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            };

            EnvVar {
                name: e.name.clone(),
                value: e.value.clone(),
                value_from,
            }
        })
        .collect()
}

/// Convert ReaperVolume list into (Vec<Volume>, Vec<VolumeMount>).
fn build_volumes(reaper_volumes: &[ReaperVolume]) -> (Vec<Volume>, Vec<VolumeMount>) {
    let mut volumes = Vec::new();
    let mut mounts = Vec::new();

    for rv in reaper_volumes {
        let mut volume = Volume {
            name: rv.name.clone(),
            ..Default::default()
        };

        if let Some(ref cm_name) = rv.config_map {
            volume.config_map = Some(ConfigMapVolumeSource {
                name: cm_name.clone(),
                ..Default::default()
            });
        } else if let Some(ref secret_name) = rv.secret {
            volume.secret = Some(SecretVolumeSource {
                secret_name: Some(secret_name.clone()),
                ..Default::default()
            });
        } else if let Some(ref path) = rv.host_path {
            volume.host_path = Some(HostPathVolumeSource {
                path: path.clone(),
                type_: None,
            });
        } else if rv.empty_dir {
            volume.empty_dir = Some(EmptyDirVolumeSource::default());
        }

        volumes.push(volume);

        let mount = VolumeMount {
            name: rv.name.clone(),
            mount_path: rv.mount_path.clone(),
            read_only: if rv.read_only { Some(true) } else { None },
            ..Default::default()
        };
        mounts.push(mount);
    }

    (volumes, mounts)
}

/// Convert ReaperToleration list into k8s Toleration list.
fn build_tolerations(reaper_tolerations: &[ReaperToleration]) -> Vec<Toleration> {
    reaper_tolerations
        .iter()
        .map(|t| Toleration {
            key: t.key.clone(),
            operator: t.operator.clone(),
            value: t.value.clone(),
            effect: t.effect.clone(),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reaper::crds::{KeyRef, ReaperPodSpec};

    fn make_reaper_pod(name: &str, spec: ReaperPodSpec) -> ReaperPod {
        ReaperPod {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                uid: Some("test-uid-123".to_string()),
                ..Default::default()
            },
            spec,
            status: None,
        }
    }

    #[test]
    fn test_basic_pod_creation() {
        let rp = make_reaper_pod(
            "my-task",
            ReaperPodSpec {
                command: vec!["/bin/sh".into(), "-c".into(), "echo hello".into()],
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let pod_spec = pod.spec.unwrap();

        assert_eq!(pod_spec.runtime_class_name.as_deref(), Some("reaper-v2"));
        assert_eq!(pod_spec.restart_policy.as_deref(), Some("Never"));
        assert_eq!(pod_spec.containers.len(), 1);
        assert_eq!(
            pod_spec.containers[0].command.as_ref().unwrap(),
            &vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hello".to_string()
            ]
        );
        assert_eq!(
            pod_spec.containers[0].image.as_deref(),
            Some(PLACEHOLDER_IMAGE)
        );

        // Owner reference
        let owner_refs = pod.metadata.owner_references.unwrap();
        assert_eq!(owner_refs.len(), 1);
        assert_eq!(owner_refs[0].kind, "ReaperPod");
        assert_eq!(owner_refs[0].name, "my-task");

        // Label
        let labels = pod.metadata.labels.unwrap();
        assert_eq!(labels.get(OWNER_LABEL).unwrap(), "my-task");
    }

    #[test]
    fn test_reaper_annotations() {
        let rp = make_reaper_pod(
            "dns-test",
            ReaperPodSpec {
                command: vec!["echo".into()],
                dns_mode: Some("kubernetes".into()),
                overlay_name: Some("my-group".into()),
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let annotations = pod.metadata.annotations.unwrap();

        assert_eq!(
            annotations.get("reaper.runtime/dns-mode").unwrap(),
            "kubernetes"
        );
        assert_eq!(
            annotations.get("reaper.runtime/overlay-name").unwrap(),
            "my-group"
        );
    }

    #[test]
    fn test_security_context() {
        let rp = make_reaper_pod(
            "user-test",
            ReaperPodSpec {
                command: vec!["whoami".into()],
                run_as_user: Some(1000),
                run_as_group: Some(1000),
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let pod_spec = pod.spec.unwrap();
        let sc = pod_spec.containers[0].security_context.as_ref().unwrap();

        assert_eq!(sc.run_as_user, Some(1000));
        assert_eq!(sc.run_as_group, Some(1000));
    }

    #[test]
    fn test_volumes() {
        let rp = make_reaper_pod(
            "vol-test",
            ReaperPodSpec {
                command: vec!["cat".into(), "/config/app.conf".into()],
                volumes: vec![ReaperVolume {
                    name: "config".into(),
                    mount_path: "/config".into(),
                    read_only: true,
                    config_map: Some("my-config".into()),
                    secret: None,
                    host_path: None,
                    empty_dir: false,
                }],
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let pod_spec = pod.spec.unwrap();

        let vols = pod_spec.volumes.unwrap();
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].name, "config");
        assert!(vols[0].config_map.is_some());
        assert_eq!(&vols[0].config_map.as_ref().unwrap().name, "my-config");

        let mounts = pod_spec.containers[0].volume_mounts.as_ref().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name, "config");
        assert_eq!(mounts[0].mount_path, "/config");
        assert_eq!(mounts[0].read_only, Some(true));
    }

    #[test]
    fn test_node_targeting() {
        let rp = make_reaper_pod(
            "pinned",
            ReaperPodSpec {
                command: vec!["hostname".into()],
                node_name: Some("worker-1".into()),
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        assert_eq!(pod.spec.unwrap().node_name.as_deref(), Some("worker-1"));
    }

    #[test]
    fn test_pod_name_matches_reaperpod() {
        let rp = make_reaper_pod(
            "my-task",
            ReaperPodSpec {
                command: vec!["echo".into()],
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        assert_eq!(pod.metadata.name.as_deref(), Some("my-task"));
        assert!(pod.metadata.generate_name.is_none());
    }

    #[test]
    fn test_env_vars_literal() {
        let rp = make_reaper_pod(
            "env-test",
            ReaperPodSpec {
                command: vec!["env".into()],
                env: vec![ReaperEnvVar {
                    name: "MY_VAR".into(),
                    value: Some("hello".into()),
                    secret_key_ref: None,
                    config_map_key_ref: None,
                }],
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let pod_spec = pod.spec.unwrap();
        let env = pod_spec.containers[0].env.as_ref().unwrap();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].name, "MY_VAR");
        assert_eq!(env[0].value.as_deref(), Some("hello"));
        assert!(env[0].value_from.is_none());
    }

    #[test]
    fn test_env_vars_secret_ref() {
        let rp = make_reaper_pod(
            "env-secret",
            ReaperPodSpec {
                command: vec!["env".into()],
                env: vec![ReaperEnvVar {
                    name: "DB_PASS".into(),
                    value: None,
                    secret_key_ref: Some(KeyRef {
                        name: "db-creds".into(),
                        key: "password".into(),
                    }),
                    config_map_key_ref: None,
                }],
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let pod_spec = pod.spec.unwrap();
        let env = pod_spec.containers[0].env.as_ref().unwrap();
        assert_eq!(env[0].name, "DB_PASS");
        assert!(env[0].value.is_none());
        let vf = env[0].value_from.as_ref().unwrap();
        let sr = vf.secret_key_ref.as_ref().unwrap();
        assert_eq!(&sr.name, "db-creds");
        assert_eq!(sr.key, "password");
    }

    #[test]
    fn test_supplemental_groups() {
        let rp = make_reaper_pod(
            "groups-test",
            ReaperPodSpec {
                command: vec!["id".into()],
                run_as_user: Some(1000),
                run_as_group: Some(1000),
                supplemental_groups: Some(vec![1000, 5000, 9999]),
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let pod_spec = pod.spec.unwrap();

        // Container-level security context has uid/gid
        let sc = pod_spec.containers[0].security_context.as_ref().unwrap();
        assert_eq!(sc.run_as_user, Some(1000));
        assert_eq!(sc.run_as_group, Some(1000));

        // Pod-level security context has supplemental groups
        let psc = pod_spec.security_context.as_ref().unwrap();
        assert_eq!(
            psc.supplemental_groups.as_ref().unwrap(),
            &vec![1000, 5000, 9999]
        );
    }

    #[test]
    fn test_no_supplemental_groups_when_none() {
        let rp = make_reaper_pod(
            "no-groups",
            ReaperPodSpec {
                command: vec!["echo".into()],
                run_as_user: Some(1000),
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let pod_spec = pod.spec.unwrap();
        assert!(pod_spec.security_context.is_none());
    }

    #[test]
    fn test_tolerations() {
        let rp = make_reaper_pod(
            "tolerate-test",
            ReaperPodSpec {
                command: vec!["echo".into()],
                tolerations: vec![ReaperToleration {
                    key: Some("node-role.kubernetes.io/control-plane".into()),
                    operator: Some("Exists".into()),
                    value: None,
                    effect: Some("NoSchedule".into()),
                }],
                ..Default::default()
            },
        );
        let pod = build_pod(&rp).unwrap();
        let tolerations = pod.spec.unwrap().tolerations.unwrap();
        assert_eq!(tolerations.len(), 1);
        assert_eq!(
            tolerations[0].key.as_deref(),
            Some("node-role.kubernetes.io/control-plane")
        );
        assert_eq!(tolerations[0].operator.as_deref(), Some("Exists"));
        assert_eq!(tolerations[0].effect.as_deref(), Some("NoSchedule"));
    }
}
