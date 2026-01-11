use containerd_shim::{
    asynchronous::{run, spawn, ExitSignal, Shim},
    publisher::RemotePublisher,
    Config, Error, Flags, StartOpts, TtrpcResult,
};
use containerd_shim_protos::{
    api, api::DeleteResponse, shim_async::Task, ttrpc::r#async::TtrpcContext,
};
use std::sync::Arc;
use tokio::process::Command;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct ReaperShim {
    exit: Arc<ExitSignal>,
    runtime_path: String,
}

#[async_trait::async_trait]
impl Shim for ReaperShim {
    type T = ReaperTask;

    async fn new(runtime_id: &str, args: &Flags, _config: &mut Config) -> Self {
        // Look for reaper-runtime in PATH or default location
        let runtime_path = std::env::var("REAPER_RUNTIME_PATH")
            .unwrap_or_else(|_| "/usr/local/bin/reaper-runtime".to_string());

        info!(
            "ReaperShim::new() called - runtime_id={}, runtime_path={}",
            runtime_id, runtime_path
        );
        info!(
            "Flags: namespace={:?}, address={:?}, publish_binary={:?}",
            args.namespace, args.address, args.publish_binary
        );

        // Verify runtime binary exists
        if let Err(e) = std::fs::metadata(&runtime_path) {
            tracing::error!("Runtime binary not found at {}: {}", runtime_path, e);
        } else {
            info!("Runtime binary verified at: {}", runtime_path);
        }

        ReaperShim {
            exit: Arc::new(ExitSignal::default()),
            runtime_path,
        }
    }

    async fn start_shim(&mut self, opts: StartOpts) -> Result<String, Error> {
        info!(
            "start_shim() called with opts: id={}, namespace={:?}",
            opts.id, opts.namespace
        );
        let grouping = opts.id.clone();
        info!("Calling spawn() with grouping={}", grouping);

        let address = spawn(opts, &grouping, Vec::new()).await.map_err(|e| {
            tracing::error!("spawn() failed: {:?}", e);
            e
        })?;

        info!("spawn() succeeded, address={}", address);
        Ok(address)
    }

    async fn delete_shim(&mut self) -> Result<DeleteResponse, Error> {
        info!("delete_shim() called - shim is shutting down");
        Ok(DeleteResponse::new())
    }

    async fn wait(&mut self) {
        info!("wait() called - blocking until exit signal");
        self.exit.wait().await;
        info!("wait() unblocked - exit signal received");
    }

    async fn create_task_service(&self, _publisher: RemotePublisher) -> Self::T {
        info!("create_task_service() called - creating ReaperTask");
        ReaperTask {
            runtime_path: self.runtime_path.clone(),
        }
    }
}

#[derive(Clone)]
struct ReaperTask {
    runtime_path: String,
}

#[async_trait::async_trait]
impl Task for ReaperTask {
    async fn create(
        &self,
        _ctx: &TtrpcContext,
        req: api::CreateTaskRequest,
    ) -> TtrpcResult<api::CreateTaskResponse> {
        info!(
            "create() called - container_id={}, bundle={}",
            req.id, req.bundle
        );

        // Call reaper-runtime create <container-id> --bundle <bundle-path>
        let output = Command::new(&self.runtime_path)
            .arg("create")
            .arg(&req.id)
            .arg("--bundle")
            .arg(&req.bundle)
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime create: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to execute reaper-runtime create: {}", e),
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime create failed: {}", stderr);
            return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("reaper-runtime create failed: {}", stderr),
            )));
        }

        let mut resp = api::CreateTaskResponse::new();
        resp.set_pid(0); // PID will be set on start
        info!("create() succeeded - container_id={}", req.id);
        Ok(resp)
    }

    async fn start(
        &self,
        _ctx: &TtrpcContext,
        req: api::StartRequest,
    ) -> TtrpcResult<api::StartResponse> {
        info!("start() called - container_id={}", req.id);

        // Call reaper-runtime start <container-id>
        let output = Command::new(&self.runtime_path)
            .arg("start")
            .arg(&req.id)
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime start: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to execute reaper-runtime start: {}", e),
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime start failed: {}", stderr);
            return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("reaper-runtime start failed: {}", stderr),
            )));
        }

        // Get the PID by calling reaper-runtime state
        let state_output = Command::new(&self.runtime_path)
            .arg("state")
            .arg(&req.id)
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime state: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to execute reaper-runtime state: {}", e),
                ))
            })?;

        let state: serde_json::Value =
            serde_json::from_slice(&state_output.stdout).map_err(|e| {
                tracing::error!("Failed to parse state output: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to parse state output: {}", e),
                ))
            })?;

        let pid = state["pid"].as_u64().unwrap_or(0) as u32;

        let mut resp = api::StartResponse::new();
        resp.set_pid(pid);
        info!("start() succeeded - container_id={}, pid={}", req.id, pid);
        Ok(resp)
    }

    async fn delete(
        &self,
        _ctx: &TtrpcContext,
        req: api::DeleteRequest,
    ) -> TtrpcResult<api::DeleteResponse> {
        info!("delete() called - container_id={}", req.id);

        // Call reaper-runtime delete <container-id>
        let output = Command::new(&self.runtime_path)
            .arg("delete")
            .arg(&req.id)
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime delete: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to execute reaper-runtime delete: {}", e),
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime delete failed: {}", stderr);
        }

        let mut resp = api::DeleteResponse::new();
        resp.set_pid(0);
        resp.set_exit_status(0);
        info!("delete() succeeded - container_id={}", req.id);
        Ok(resp)
    }

    async fn kill(&self, _ctx: &TtrpcContext, req: api::KillRequest) -> TtrpcResult<api::Empty> {
        info!(
            "kill() called - container_id={}, signal={}, all={}",
            req.id, req.signal, req.all
        );

        // Call reaper-runtime kill <container-id> <signal>
        let output = Command::new(&self.runtime_path)
            .arg("kill")
            .arg(&req.id)
            .arg(req.signal.to_string())
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime kill: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to execute reaper-runtime kill: {}", e),
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime kill failed: {}", stderr);
            return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("reaper-runtime kill failed: {}", stderr),
            )));
        }

        info!(
            "kill() succeeded - container_id={}, signal={}",
            req.id, req.signal
        );
        Ok(api::Empty::new())
    }

    async fn wait(
        &self,
        _ctx: &TtrpcContext,
        req: api::WaitRequest,
    ) -> TtrpcResult<api::WaitResponse> {
        info!(
            "wait() task called - container_id={}, exec_id={:?}",
            req.id, req.exec_id
        );

        // Poll reaper-runtime state until container stops
        loop {
            let output = Command::new(&self.runtime_path)
                .arg("state")
                .arg(&req.id)
                .output()
                .await
                .map_err(|e| {
                    tracing::error!("Failed to execute reaper-runtime state: {}", e);
                    ttrpc::Error::RpcStatus(ttrpc::get_status(
                        ttrpc::Code::INTERNAL,
                        format!("Failed to execute reaper-runtime state: {}", e),
                    ))
                })?;

            if !output.status.success() {
                // Container might be deleted, return
                break;
            }

            let state: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|e| {
                tracing::error!("Failed to parse state output: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to parse state output: {}", e),
                ))
            })?;

            let status = state["status"].as_str().unwrap_or("");
            if status == "stopped" {
                info!("wait() - container stopped, container_id={}", req.id);
                break;
            }

            // Wait a bit before polling again
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        let mut resp = api::WaitResponse::new();
        resp.set_exit_status(0);
        info!("wait() task completed - container_id={}", req.id);
        Ok(resp)
    }

    async fn state(
        &self,
        _ctx: &TtrpcContext,
        req: api::StateRequest,
    ) -> TtrpcResult<api::StateResponse> {
        info!(
            "state() called - container_id={}, exec_id={:?}",
            req.id, req.exec_id
        );

        // Query runtime for actual state
        let output = Command::new(&self.runtime_path)
            .arg("state")
            .arg(&req.id)
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime state: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to execute reaper-runtime state: {}", e),
                ))
            })?;

        if !output.status.success() {
            // If runtime returns error, container might not exist
            let mut resp = api::StateResponse::new();
            resp.id = req.id;
            resp.status = ::protobuf::EnumOrUnknown::new(api::Status::UNKNOWN);
            return Ok(resp);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let state: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
            tracing::error!("Failed to parse state output: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to parse state output: {}", e),
            ))
        })?;

        let mut resp = api::StateResponse::new();
        resp.id = state["id"].as_str().unwrap_or(&req.id).to_string();
        resp.bundle = state["bundle"].as_str().unwrap_or("").to_string();
        resp.pid = state["pid"].as_u64().unwrap_or(0) as u32;

        let status_str = state["status"].as_str().unwrap_or("unknown");
        resp.status = match status_str {
            "created" => ::protobuf::EnumOrUnknown::new(api::Status::CREATED),
            "running" => ::protobuf::EnumOrUnknown::new(api::Status::RUNNING),
            "stopped" => ::protobuf::EnumOrUnknown::new(api::Status::STOPPED),
            _ => ::protobuf::EnumOrUnknown::new(api::Status::UNKNOWN),
        };

        info!(
            "state() succeeded - container_id={}, status={:?}, pid={}",
            req.id, status_str, resp.pid
        );
        Ok(resp)
    }

    async fn pids(
        &self,
        _ctx: &TtrpcContext,
        req: api::PidsRequest,
    ) -> TtrpcResult<api::PidsResponse> {
        info!("pids() called - container_id={}", req.id);

        // Query runtime for state to get PID
        let output = Command::new(&self.runtime_path)
            .arg("state")
            .arg(&req.id)
            .output()
            .await
            .map_err(|e| {
                tracing::error!("Failed to execute reaper-runtime state: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to execute reaper-runtime state: {}", e),
                ))
            })?;

        let mut resp = api::PidsResponse::new();

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(state) = serde_json::from_str::<serde_json::Value>(&stdout) {
                if let Some(pid) = state["pid"].as_u64() {
                    let mut process = api::ProcessInfo::new();
                    process.pid = pid as u32;
                    resp.processes.push(process);
                }
            }
        }

        info!(
            "pids() succeeded - container_id={}, count={}",
            req.id,
            resp.processes.len()
        );
        Ok(resp)
    }

    async fn exec(
        &self,
        _ctx: &TtrpcContext,
        req: api::ExecProcessRequest,
    ) -> TtrpcResult<api::Empty> {
        info!("exec() called - container_id={}, exec_id={}, stdin={}, stdout={}, stderr={}, terminal={}",
            req.id, req.exec_id, req.stdin.is_empty(), req.stdout.is_empty(), req.stderr.is_empty(), req.terminal);

        // For now, we don't support exec since each container runs independently
        // In the future, this could spawn additional processes via runtime
        Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
            ttrpc::Code::UNIMPLEMENTED,
            "Exec not supported - each container runs independently".to_string(),
        )))
    }

    async fn stats(
        &self,
        _ctx: &TtrpcContext,
        req: api::StatsRequest,
    ) -> TtrpcResult<api::StatsResponse> {
        info!("stats() called - container_id={}", req.id);

        // For now, return basic stats - in a real implementation we'd collect actual metrics
        let resp = api::StatsResponse::new();
        // TODO: Implement actual resource monitoring (CPU, memory, etc.)
        // For Milestone 4, we provide basic placeholder stats

        Ok(resp)
    }

    async fn resize_pty(
        &self,
        _ctx: &TtrpcContext,
        req: api::ResizePtyRequest,
    ) -> TtrpcResult<api::Empty> {
        info!(
            "resize_pty() called - container_id={}, width={}, height={}",
            req.id, req.width, req.height
        );

        // For now, we don't support interactive resizing since containers run non-interactively
        Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
            ttrpc::Code::UNIMPLEMENTED,
            "ResizePty not supported for non-interactive containers".to_string(),
        )))
    }
}

#[tokio::main]
async fn main() {
    // Setup tracing to log to a file instead of stdout/stderr
    // Containerd communicates with shims via stdout/stderr, so we can't pollute those streams

    // ALWAYS initialize tracing to prevent info! from defaulting to stdout/stderr
    if let Ok(log_path) = std::env::var("REAPER_SHIM_LOG") {
        // If REAPER_SHIM_LOG is set, log to that file
        if let Ok(log_file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_ansi(false) // No color codes in log files
                .with_writer(std::sync::Mutex::new(log_file))
                .init();

            info!("===== Reaper Shim v2 Starting =====");
            info!("Log file: {}", log_path);
        } else {
            // Failed to open log file - use null writer to discard logs safely
            let null_writer = std::io::sink();
            tracing_subscriber::fmt()
                .with_writer(std::sync::Mutex::new(null_writer))
                .with_ansi(false)
                .init();
        }
    } else {
        // REAPER_SHIM_LOG not set - use null writer to discard all logs safely
        // This ensures info! calls never go to stdout/stderr
        let null_writer = std::io::sink();
        tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(null_writer))
            .with_ansi(false)
            .init();
    }

    info!("Calling containerd_shim::run()...");
    run::<ReaperShim>("io.containerd.reaper.v2", None).await;
    info!("containerd_shim::run() completed normally");
}
