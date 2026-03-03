use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use prometheus_client::{
    encoding::text::encode,
    metrics::{counter::Counter, gauge::Gauge},
    registry::Registry,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tracing::info;

use crate::health;

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

        Self {
            inner: Arc::new(MetricsInner {
                registry: Mutex::new(registry),
                containers_created,
                containers_running,
                containers_stopped,
                config_syncs_total,
                gc_runs_total,
                healthy,
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

/// Start the HTTP server for health and metrics endpoints.
pub async fn serve(
    addr: SocketAddr,
    metrics: MetricsState,
    shim_path: &str,
    runtime_path: &str,
    state_dir: &str,
) -> anyhow::Result<()> {
    let state = AppState {
        metrics,
        shim_path: shim_path.to_string(),
        runtime_path: runtime_path.to_string(),
        state_dir: state_dir.to_string(),
    };

    let app = Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    info!(addr = %addr, "starting HTTP server");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
