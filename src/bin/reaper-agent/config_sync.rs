use anyhow::{Context, Result};
use futures::TryStreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    api::Api,
    runtime::{
        watcher::{self},
        WatchStreamExt,
    },
    Client,
};
use std::fs;
use std::path::Path;
use std::pin::pin;
use tracing::{error, info, warn};

use crate::metrics::MetricsState;

/// Key within the ConfigMap that holds the config file contents.
const CONFIG_KEY: &str = "reaper.conf";

/// Write config content to disk atomically (write tmp + rename).
fn atomic_write(path: &str, content: &str) -> Result<()> {
    let target = Path::new(path);

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating config dir {:?}", parent))?;
    }

    let tmp_path = format!("{}.tmp", path);
    fs::write(&tmp_path, content)
        .with_context(|| format!("writing temp config to {}", tmp_path))?;
    fs::rename(&tmp_path, path).with_context(|| format!("renaming {} to {}", tmp_path, path))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("setting permissions on {}", path))?;
    }

    Ok(())
}

/// Extract config content from a ConfigMap and write it to disk.
fn sync_configmap(cm: &ConfigMap, config_path: &str) -> Result<bool> {
    let data = match &cm.data {
        Some(d) => d,
        None => {
            warn!("ConfigMap has no data section, skipping sync");
            return Ok(false);
        }
    };

    let content = match data.get(CONFIG_KEY) {
        Some(c) => c,
        None => {
            warn!(
                key = CONFIG_KEY,
                "ConfigMap missing expected key, skipping sync"
            );
            return Ok(false);
        }
    };

    atomic_write(config_path, content)?;
    info!(path = config_path, "config file updated from ConfigMap");
    Ok(true)
}

/// Run the config sync loop: watch a ConfigMap and write changes to host.
///
/// Falls back gracefully on API errors — never deletes an existing config file.
pub async fn config_sync_loop(
    namespace: &str,
    name: &str,
    config_path: &str,
    metrics: &MetricsState,
) -> Result<()> {
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to create Kubernetes client, config sync disabled");
            return Err(e.into());
        }
    };

    let api: Api<ConfigMap> = Api::namespaced(client, namespace);

    // Initial sync: try to read the ConfigMap once
    match api.get(name).await {
        Ok(cm) => {
            if sync_configmap(&cm, config_path)? {
                metrics.inc_config_syncs();
            }
        }
        Err(e) => {
            warn!(error = %e, "initial ConfigMap read failed, keeping existing config");
        }
    }

    // Watch for changes
    let watcher_config = watcher::Config::default().fields(&format!("metadata.name={}", name));
    let stream = watcher::watcher(api, watcher_config).applied_objects();
    let mut stream = pin!(stream);

    info!(
        namespace = namespace,
        name = name,
        "watching ConfigMap for changes"
    );

    while let Some(cm) = stream.try_next().await? {
        match sync_configmap(&cm, config_path) {
            Ok(true) => metrics.inc_config_syncs(),
            Ok(false) => {}
            Err(e) => {
                error!(error = %e, "failed to sync config from ConfigMap");
            }
        }
    }

    warn!("ConfigMap watch stream ended unexpectedly");
    Ok(())
}
