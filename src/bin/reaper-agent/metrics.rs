use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use prometheus_client::{
    encoding::text::encode,
    metrics::{counter::Counter, gauge::Gauge},
    registry::Registry,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tracing::info;

use crate::executor::JobManager;
use crate::health;
use crate::jobs::{JobRequest, JobResponse, JobState};
use crate::overlay_api;

/// Shared metrics state used across all agent tasks.
#[derive(Clone)]
pub struct MetricsState {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    registry: Mutex<Registry>,

    // Container counts by status
    containers_created: Gauge,
    containers_running: Gauge,
    containers_stopped: Gauge,

    // Operational counters
    config_syncs_total: Counter,
    gc_runs_total: Counter,

    // Health gauge
    healthy: Gauge,

    // Overlay GC metrics
    overlay_gc_runs_total: Counter,
    overlay_gc_cleaned_total: Counter,
    overlay_namespaces: Gauge,

    // Namespace cleanup metrics
    ns_cleanup_runs_total: Counter,
    #[allow(dead_code)] // used on Linux only (run_ns_cleanup)
    ns_cleaned_total: Counter,

    // Node condition reporting metrics
    node_condition_updates_total: Counter,
    node_condition_healthy: Gauge,
}

impl MetricsState {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let containers_created = Gauge::default();
        let containers_running = Gauge::default();
        let containers_stopped = Gauge::default();
        let config_syncs_total = Counter::default();
        let gc_runs_total = Counter::default();
        let healthy = Gauge::default();
        let overlay_gc_runs_total = Counter::default();
        let overlay_gc_cleaned_total = Counter::default();
        let overlay_namespaces = Gauge::default();
        let ns_cleanup_runs_total = Counter::default();
        let ns_cleaned_total = Counter::default();
        let node_condition_updates_total = Counter::default();
        let node_condition_healthy = Gauge::default();

        registry.register(
            "reaper_containers_created",
            "Number of containers in created state",
            containers_created.clone(),
        );
        registry.register(
            "reaper_containers_running",
            "Number of containers in running state",
            containers_running.clone(),
        );
        registry.register(
            "reaper_containers_stopped",
            "Number of containers in stopped state",
            containers_stopped.clone(),
        );
        registry.register(
            "reaper_agent_config_syncs_total",
            "Total number of config file syncs from ConfigMap",
            config_syncs_total.clone(),
        );
        registry.register(
            "reaper_agent_gc_runs_total",
            "Total number of GC scan cycles",
            gc_runs_total.clone(),
        );
        registry.register(
            "reaper_agent_healthy",
            "Whether the agent considers the node healthy (1=healthy, 0=unhealthy)",
            healthy.clone(),
        );
        registry.register(
            "reaper_agent_overlay_gc_runs_total",
            "Total number of overlay GC reconciliation cycles",
            overlay_gc_runs_total.clone(),
        );
        registry.register(
            "reaper_agent_overlay_gc_cleaned_total",
            "Total number of overlay namespaces cleaned up",
            overlay_gc_cleaned_total.clone(),
        );
        registry.register(
            "reaper_agent_overlay_namespaces",
            "Current number of on-disk overlay namespace directories",
            overlay_namespaces.clone(),
        );
        registry.register(
            "reaper_agent_ns_cleanup_runs_total",
            "Total number of mount namespace cleanup passes",
            ns_cleanup_runs_total.clone(),
        );
        registry.register(
            "reaper_agent_ns_cleaned_total",
            "Total number of stale namespace bind-mount files removed",
            ns_cleaned_total.clone(),
        );
        registry.register(
            "reaper_agent_node_condition_updates_total",
            "Total number of node condition patch operations",
            node_condition_updates_total.clone(),
        );
        registry.register(
            "reaper_agent_node_condition_healthy",
            "Whether the last node condition patch reported healthy (1=healthy, 0=unhealthy)",
            node_condition_healthy.clone(),
        );

        Self {
            inner: Arc::new(MetricsInner {
                registry: Mutex::new(registry),
                containers_created,
                containers_running,
                containers_stopped,
                config_syncs_total,
                gc_runs_total,
                healthy,
                overlay_gc_runs_total,
                overlay_gc_cleaned_total,
                overlay_namespaces,
                ns_cleanup_runs_total,
                ns_cleaned_total,
                node_condition_updates_total,
                node_condition_healthy,
            }),
        }
    }

    pub fn set_containers(&self, created: u64, running: u64, stopped: u64) {
        self.inner.containers_created.set(created as i64);
        self.inner.containers_running.set(running as i64);
        self.inner.containers_stopped.set(stopped as i64);
    }

    pub fn inc_config_syncs(&self) {
        self.inner.config_syncs_total.inc();
    }

    pub fn inc_gc_runs(&self) {
        self.inner.gc_runs_total.inc();
    }

    pub fn set_healthy(&self, healthy: bool) {
        self.inner.healthy.set(if healthy { 1 } else { 0 });
    }

    #[allow(dead_code)]
    pub fn is_healthy(&self) -> bool {
        self.inner.healthy.get() == 1
    }

    pub fn inc_overlay_gc_runs(&self) {
        self.inner.overlay_gc_runs_total.inc();
    }

    pub fn inc_overlay_gc_cleaned(&self, count: u64) {
        for _ in 0..count {
            self.inner.overlay_gc_cleaned_total.inc();
        }
    }

    pub fn inc_ns_cleanup_runs(&self) {
        self.inner.ns_cleanup_runs_total.inc();
    }

    #[allow(dead_code)] // used on Linux only (run_ns_cleanup)
    pub fn inc_ns_cleaned(&self, count: u64) {
        for _ in 0..count {
            self.inner.ns_cleaned_total.inc();
        }
    }

    pub fn inc_node_condition_updates(&self) {
        self.inner.node_condition_updates_total.inc();
    }

    pub fn set_node_condition_healthy(&self, healthy: bool) {
        self.inner
            .node_condition_healthy
            .set(if healthy { 1 } else { 0 });
    }

    pub fn set_overlay_namespaces(&self, count: u64) {
        self.inner.overlay_namespaces.set(count as i64);
    }

    pub fn encode(&self) -> String {
        let mut buf = String::new();
        let registry = self.inner.registry.lock().unwrap();
        encode(&mut buf, &registry).unwrap();
        buf
    }
}

/// App state shared with axum handlers.
#[derive(Clone)]
struct AppState {
    metrics: MetricsState,
    shim_path: String,
    runtime_path: String,
    state_dir: String,
    job_manager: JobManager,
}

async fn healthz_handler(State(state): State<AppState>) -> impl IntoResponse {
    let result = health::check_health(&state.shim_path, &state.runtime_path, &state.state_dir);
    state.metrics.set_healthy(result.healthy);

    if result.healthy {
        (StatusCode::OK, "ok\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "unhealthy\n")
    }
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    let body = state.metrics.encode();
    (
        StatusCode::OK,
        [(
            "content-type",
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        body,
    )
}

async fn readyz_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn submit_job_handler(
    State(state): State<AppState>,
    axum::Json(request): axum::Json<JobRequest>,
) -> impl IntoResponse {
    match state.job_manager.submit(request).await {
        Ok(job_id) => (
            StatusCode::CREATED,
            axum::Json(JobResponse {
                job_id,
                status: JobState::Running,
            }),
        )
            .into_response(),
        Err(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

async fn job_status_handler(
    State(state): State<AppState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.job_manager.status(&job_id).await {
        Some(status) => (StatusCode::OK, axum::Json(status)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn terminate_job_handler(
    State(state): State<AppState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if state.job_manager.terminate(&job_id).await {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn list_overlays_handler(State(state): State<AppState>) -> impl IntoResponse {
    let overlays = overlay_api::list_overlays(&state.state_dir);
    (StatusCode::OK, axum::Json(overlays))
}

async fn get_overlay_handler(
    State(state): State<AppState>,
    axum::extract::Path((namespace, name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    match overlay_api::get_overlay(&state.state_dir, &namespace, &name) {
        Some(detail) => (StatusCode::OK, axum::Json(detail)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn delete_overlay_handler(
    State(state): State<AppState>,
    axum::extract::Path((namespace, name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    match overlay_api::delete_overlay(&state.state_dir, &namespace, &name) {
        Ok(true) => (StatusCode::OK, "overlay deleted\n").into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(msg) => (StatusCode::CONFLICT, msg).into_response(),
    }
}

/// Start the HTTP server for health, metrics, and job execution endpoints.
pub async fn serve(
    addr: SocketAddr,
    metrics: MetricsState,
    shim_path: &str,
    runtime_path: &str,
    state_dir: &str,
    job_manager: JobManager,
) -> anyhow::Result<()> {
    let state = AppState {
        metrics,
        shim_path: shim_path.to_string(),
        runtime_path: runtime_path.to_string(),
        state_dir: state_dir.to_string(),
        job_manager,
    };

    let app = Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .route("/metrics", get(metrics_handler))
        .route("/api/v1/jobs", axum::routing::post(submit_job_handler))
        .route(
            "/api/v1/jobs/{id}",
            get(job_status_handler).delete(terminate_job_handler),
        )
        .route("/api/v1/overlays", get(list_overlays_handler))
        .route(
            "/api/v1/overlays/{namespace}/{name}",
            get(get_overlay_handler).delete(delete_overlay_handler),
        )
        .with_state(state);

    info!(addr = %addr, "starting HTTP server");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
