use containerd_shim::{
    asynchronous::{run, spawn, ExitSignal, Shim},
    publisher::RemotePublisher,
    Config, Error, Flags, StartOpts, TtrpcResult,
};
use containerd_shim_protos::{
    api, api::DeleteResponse, shim_async::Task, ttrpc::r#async::TtrpcContext,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct ReaperShim {
    exit: Arc<ExitSignal>,
    containers: Arc<Mutex<HashMap<String, ContainerInfo>>>,
}

#[derive(Debug, Clone)]
struct ContainerInfo {
    id: String,
    bundle: String,
    pid: Option<u32>,
    status: ContainerStatus,
}

#[derive(Debug, Clone)]
enum ContainerStatus {
    Created,
    Running,
    Stopped,
}

#[async_trait::async_trait]
impl Shim for ReaperShim {
    type T = ReaperTask;

    async fn new(_runtime_id: &str, _args: &Flags, _config: &mut Config) -> Self {
        ReaperShim {
            exit: Arc::new(ExitSignal::default()),
            containers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn start_shim(&mut self, opts: StartOpts) -> Result<String, Error> {
        let grouping = opts.id.clone();
        let address = spawn(opts, &grouping, Vec::new()).await?;
        Ok(address)
    }

    async fn delete_shim(&mut self) -> Result<DeleteResponse, Error> {
        Ok(DeleteResponse::new())
    }

    async fn wait(&mut self) {
        self.exit.wait().await;
    }

    async fn create_task_service(&self, _publisher: RemotePublisher) -> Self::T {
        ReaperTask {
            containers: self.containers.clone(),
        }
    }
}

#[derive(Clone)]
struct ReaperTask {
    containers: Arc<Mutex<HashMap<String, ContainerInfo>>>,
}

#[async_trait::async_trait]
impl Task for ReaperTask {
    async fn create(
        &self,
        _ctx: &TtrpcContext,
        req: api::CreateTaskRequest,
    ) -> TtrpcResult<api::CreateTaskResponse> {
        info!("create called for container: {}", req.id);

        // Call reaper-runtime create
        let output = Command::new("reaper-runtime")
            .arg("create")
            .arg(&req.id)
            .arg(&req.bundle)
            .env("REAPER_RUNTIME_ROOT", "/run/reaper")
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime create: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to create container: {}", e),
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime create failed: {}", stderr);
            return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Container creation failed: {}", stderr),
            )));
        }

        // Store container info
        let container_info = ContainerInfo {
            id: req.id.clone(),
            bundle: req.bundle.clone(),
            pid: None,
            status: ContainerStatus::Created,
        };

        {
            let mut containers = self.containers.lock().unwrap();
            containers.insert(req.id.clone(), container_info);
        }

        let mut resp = api::CreateTaskResponse::new();
        resp.set_pid(0); // PID will be set on start
        Ok(resp)
    }

    async fn start(
        &self,
        _ctx: &TtrpcContext,
        req: api::StartRequest,
    ) -> TtrpcResult<api::StartResponse> {
        info!("start called for container: {}", req.id);

        // Call reaper-runtime start
        let output = Command::new("reaper-runtime")
            .arg("start")
            .arg(&req.id)
            .env("REAPER_RUNTIME_ROOT", "/run/reaper")
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime start: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to start container: {}", e),
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime start failed: {}", stderr);
            return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Container start failed: {}", stderr),
            )));
        }

        // Parse PID from output (assuming reaper-runtime prints PID)
        let stdout = String::from_utf8_lossy(&output.stdout);
        let pid: u32 = stdout.trim().parse().unwrap_or(0);

        // Update container info
        {
            let mut containers = self.containers.lock().unwrap();
            if let Some(container) = containers.get_mut(&req.id) {
                container.pid = Some(pid);
                container.status = ContainerStatus::Running;
            }
        }

        let mut resp = api::StartResponse::new();
        resp.set_pid(pid);
        Ok(resp)
    }

    async fn delete(
        &self,
        _ctx: &TtrpcContext,
        req: api::DeleteRequest,
    ) -> TtrpcResult<api::DeleteResponse> {
        info!("delete called for container: {}", req.id);

        // Call reaper-runtime delete
        let output = Command::new("reaper-runtime")
            .arg("delete")
            .arg(&req.id)
            .env("REAPER_RUNTIME_ROOT", "/run/reaper")
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime delete: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to delete container: {}", e),
                ))
            })?;

        let exit_status = output.status.code().unwrap_or(0);

        // Remove from our tracking
        {
            let mut containers = self.containers.lock().unwrap();
            containers.remove(&req.id);
        }

        let mut resp = api::DeleteResponse::new();
        resp.set_pid(0); // We don't track the reaper-runtime PID
        resp.set_exit_status(exit_status as u32);
        Ok(resp)
    }

    async fn kill(&self, _ctx: &TtrpcContext, req: api::KillRequest) -> TtrpcResult<api::Empty> {
        info!(
            "kill called for container: {} signal: {}",
            req.id, req.signal
        );

        // Call reaper-runtime kill
        let output = Command::new("reaper-runtime")
            .arg("kill")
            .arg(&req.id)
            .arg(req.signal.to_string())
            .env("REAPER_RUNTIME_ROOT", "/run/reaper")
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime kill: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to kill container: {}", e),
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime kill failed: {}", stderr);
            return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Container kill failed: {}", stderr),
            )));
        }

        // Update status
        {
            let mut containers = self.containers.lock().unwrap();
            if let Some(container) = containers.get_mut(&req.id) {
                container.status = ContainerStatus::Stopped;
            }
        }

        Ok(api::Empty::new())
    }

    async fn wait(
        &self,
        _ctx: &TtrpcContext,
        req: api::WaitRequest,
    ) -> TtrpcResult<api::WaitResponse> {
        info!("wait called for container: {}", req.id);

        // For now, just return success - in a real implementation we'd wait for the process
        // This is a simplified version for the milestone
        let exit_status = 0;

        let mut resp = api::WaitResponse::new();
        resp.set_exit_status(exit_status);
        Ok(resp)
    }

    async fn state(
        &self,
        _ctx: &TtrpcContext,
        req: api::StateRequest,
    ) -> TtrpcResult<api::StateResponse> {
        info!("state called for container: {}", req.id);

        let containers = self.containers.lock().unwrap();
        let container = containers.get(&req.id);

        let mut resp = api::StateResponse::new();
        if let Some(container) = container {
            resp.id = container.id.clone();
            resp.bundle = container.bundle.clone();
            resp.pid = container.pid.unwrap_or(0);
            resp.status = match container.status {
                ContainerStatus::Created => ::protobuf::EnumOrUnknown::new(api::Status::CREATED),
                ContainerStatus::Running => ::protobuf::EnumOrUnknown::new(api::Status::RUNNING),
                ContainerStatus::Stopped => ::protobuf::EnumOrUnknown::new(api::Status::STOPPED),
            };
        } else {
            resp.id = req.id;
            resp.status = ::protobuf::EnumOrUnknown::new(api::Status::UNKNOWN);
        }

        Ok(resp)
    }

    async fn pids(
        &self,
        _ctx: &TtrpcContext,
        req: api::PidsRequest,
    ) -> TtrpcResult<api::PidsResponse> {
        info!("pids called for container: {}", req.id);

        let containers = self.containers.lock().unwrap();
        let container = containers.get(&req.id);

        let mut resp = api::PidsResponse::new();
        if let Some(container) = container {
            if let Some(pid) = container.pid {
                let mut process = api::ProcessInfo::new();
                process.pid = pid;
                resp.processes.push(process);
            }
        }

        Ok(resp)
    }
}

#[tokio::main]
async fn main() {
    // Setup tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    run::<ReaperShim>("io.containerd.reaper.v2", None).await;
}
