use containerd_shim::{
    asynchronous::{run, spawn, ExitSignal, Shim},
    publisher::RemotePublisher,
    Config, Error, Flags, StartOpts, TtrpcResult,
};
use containerd_shim_protos::{
    api, api::DeleteResponse, shim_async::Task, ttrpc::r#async::TtrpcContext,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Helper function to execute a command and properly reap the child process
/// This is critical when forking happens inside the spawned process - we need to ensure
/// the parent process is fully reaped even if it exits before the child is ready
fn execute_and_reap_child(program: &str, args: Vec<&str>) -> std::io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new(program);
    for arg in args {
        cmd.arg(arg);
    }

    // Spawn and wait for the process
    let output = cmd.output()?;

    // Explicitly try to reap any orphaned child processes (grandchildren that have exited)
    // This helps clean up zombies if the direct child exited but had children
    use nix::sys::wait;
    let _ = wait::waitpid(None, Some(wait::WaitPidFlag::WNOHANG));

    Ok(output)
}

#[derive(Clone)]
struct ReaperShim {
    exit: Arc<ExitSignal>,
    runtime_path: String,
    namespace: String,
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
            "Flags: namespace={:?}, address={:?}, publish_binary={:?}, socket={:?}",
            args.namespace, args.address, args.publish_binary, args.socket
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
            namespace: args.namespace.clone(),
        }
    }

    async fn start_shim(&mut self, opts: StartOpts) -> Result<String, Error> {
        info!(
            "start_shim() called with opts: id={}, namespace={:?}, ttrpc_address={}",
            opts.id, opts.namespace, opts.ttrpc_address
        );
        let grouping = opts.id.clone();
        let ttrpc_address = opts.ttrpc_address.clone();
        info!(
            "Calling spawn() with grouping={}, passing TTRPC_ADDRESS={}",
            grouping, ttrpc_address
        );

        // Pass TTRPC_ADDRESS to child process - this is REQUIRED for bootstrap to work!
        let vars: Vec<(&str, &str)> = vec![("TTRPC_ADDRESS", ttrpc_address.as_str())];

        let address = spawn(opts, &grouping, vars).await.map_err(|e| {
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

    async fn create_task_service(&self, publisher: RemotePublisher) -> Self::T {
        info!("create_task_service() called - creating ReaperTask");
        ReaperTask {
            runtime_path: self.runtime_path.clone(),
            sandbox_state: Arc::new(Mutex::new(HashMap::new())),
            publisher: Arc::new(publisher),
            namespace: self.namespace.clone(),
        }
    }
}

#[derive(Clone)]
struct ReaperTask {
    runtime_path: String,
    // Track which containers are sandboxes (pause containers) vs real workloads
    // Key: container_id, Value: (is_sandbox, fake_pid)
    sandbox_state: Arc<Mutex<HashMap<String, (bool, u32)>>>,
    // Publisher for sending task lifecycle events to containerd
    publisher: Arc<RemotePublisher>,
    // Namespace for events
    namespace: String,
}

// Helper function to detect if a container is a sandbox/pause container
fn is_sandbox_container(bundle: &str) -> bool {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct OciConfig {
        #[serde(default)]
        process: Option<OciProcess>,
        #[serde(default)]
        annotations: Option<std::collections::HashMap<String, String>>,
    }

    #[derive(Deserialize)]
    struct OciProcess {
        #[serde(default)]
        args: Vec<String>,
    }

    let config_path = Path::new(bundle).join("config.json");
    let config_data = match std::fs::read_to_string(&config_path) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("Failed to read config.json: {}", e);
            return false;
        }
    };

    let config: OciConfig = match serde_json::from_str(&config_data) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to parse config.json: {}", e);
            return false;
        }
    };

    // Check for pause container indicators:
    // 1. Command is "/pause" or contains "pause"
    // 2. Annotation indicates it's a sandbox
    if let Some(process) = config.process {
        if let Some(cmd) = process.args.first() {
            if cmd.contains("pause") {
                return true;
            }
        }
    }

    if let Some(annotations) = config.annotations {
        // CRI annotation for sandbox containers
        if annotations.get("io.kubernetes.cri.container-type") == Some(&"sandbox".to_string()) {
            return true;
        }
    }

    false
}

impl ReaperTask {
    /// Publish a TaskExit event to containerd
    async fn publish_exit_event(&self, container_id: &str, pid: u32, exit_code: u32) {
        use containerd_shim_protos::events::task::TaskExit;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let mut timestamp = ::protobuf::well_known_types::timestamp::Timestamp::new();
        timestamp.seconds = now.as_secs() as i64;
        timestamp.nanos = now.subsec_nanos() as i32;

        let event = TaskExit {
            container_id: container_id.to_string(),
            id: container_id.to_string(),
            pid,
            exit_status: exit_code,
            exited_at: ::protobuf::MessageField::some(timestamp),
            ..Default::default()
        };

        info!(
            "Publishing TaskExit event: container_id={}, pid={}, exit_code={}",
            container_id, pid, exit_code
        );

        if let Err(e) = self
            .publisher
            .publish(
                ::containerd_shim::Context::default(),
                "/tasks/exit",
                &self.namespace,
                Box::new(event),
            )
            .await
        {
            tracing::error!("Failed to publish TaskExit event: {:?}", e);
        }
    }
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

        // Detect if this is a sandbox/pause container
        let is_sandbox = is_sandbox_container(&req.bundle);

        if is_sandbox {
            info!("create() - detected SANDBOX container, faking creation");
            // Track this as a sandbox with fake PID
            let mut state = self.sandbox_state.lock().unwrap();
            state.insert(req.id.clone(), (true, 1));

            info!("create() succeeded - container_id={} (sandbox)", req.id);
            return Ok(api::CreateTaskResponse {
                pid: 1,
                ..Default::default()
            });
        }

        // Real workload container - call reaper-runtime
        info!("create() - detected WORKLOAD container, calling reaper-runtime");

        // Track this as a real workload
        {
            let mut state = self.sandbox_state.lock().unwrap();
            state.insert(req.id.clone(), (false, 0));
        }

        info!(
            "create() - about to execute: {} create {} --bundle {} (stdin={}, stdout={}, stderr={})",
            self.runtime_path, req.id, req.bundle, req.stdin, req.stdout, req.stderr
        );

        // Call reaper-runtime create <container-id> --bundle <bundle-path>
        // with optional I/O paths for Kubernetes logging
        let runtime_path = self.runtime_path.clone();
        let container_id = req.id.clone();
        let bundle_path = req.bundle.clone();
        let stdin_path = req.stdin.clone();
        let stdout_path = req.stdout.clone();
        let stderr_path = req.stderr.clone();

        let output = tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(&runtime_path);
            cmd.arg("create")
                .arg(&container_id)
                .arg("--bundle")
                .arg(&bundle_path);

            // Pass I/O paths if provided by containerd
            if !stdin_path.is_empty() {
                cmd.arg("--stdin").arg(&stdin_path);
            }
            if !stdout_path.is_empty() {
                cmd.arg("--stdout").arg(&stdout_path);
            }
            if !stderr_path.is_empty() {
                cmd.arg("--stderr").arg(&stderr_path);
            }

            cmd.output()
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to spawn reaper-runtime task: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to spawn reaper-runtime task: {}", e),
            ))
        })?
        .map_err(|e| {
            tracing::error!("Failed to execute reaper-runtime create: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to execute reaper-runtime create: {}", e),
            ))
        })?;

        info!(
            "create() - command completed, status={}, stdout_len={}, stderr_len={}",
            output.status,
            output.stdout.len(),
            output.stderr.len()
        );

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

        // Check if this is a sandbox container
        let is_sandbox = {
            let state = self.sandbox_state.lock().unwrap();
            state
                .get(&req.id)
                .map(|(is_sand, _)| *is_sand)
                .unwrap_or(false)
        };

        if is_sandbox {
            info!("start() - SANDBOX container, returning fake PID");
            return Ok(api::StartResponse {
                pid: 1,
                ..Default::default()
            });
        }

        // Real workload - call reaper-runtime
        info!("start() - WORKLOAD container, calling reaper-runtime");

        // Use blocking context with std::process::Command for better process control
        // This avoids interference from tokio's async process management
        let runtime_path = self.runtime_path.clone();
        let container_id = req.id.clone();
        let output = tokio::task::spawn_blocking(move || {
            execute_and_reap_child(&runtime_path, vec!["start", &container_id])
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to spawn reaper-runtime task: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to spawn reaper-runtime task: {}", e),
            ))
        })?
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
        let runtime_path_state = self.runtime_path.clone();
        let container_id_state = req.id.clone();
        let state_output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&runtime_path_state)
                .arg("state")
                .arg(&container_id_state)
                .output()
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to spawn reaper-runtime task: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to spawn reaper-runtime task: {}", e),
            ))
        })?
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

        // Check if this is a sandbox container
        let is_sandbox = {
            let mut state = self.sandbox_state.lock().unwrap();
            let result = state
                .get(&req.id)
                .map(|(is_sand, _)| *is_sand)
                .unwrap_or(false);
            // Remove from state
            state.remove(&req.id);
            result
        };

        if is_sandbox {
            info!("delete() - SANDBOX container, cleaning up fake state");
            return Ok(api::DeleteResponse {
                pid: 1,
                exit_status: 0,
                ..Default::default()
            });
        }

        // Real workload - call reaper-runtime
        info!("delete() - WORKLOAD container, calling reaper-runtime");

        // Call reaper-runtime delete <container-id>
        let runtime_path = self.runtime_path.clone();
        let container_id = req.id.clone();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&runtime_path)
                .arg("delete")
                .arg(&container_id)
                .output()
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to spawn reaper-runtime task: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to spawn reaper-runtime task: {}", e),
            ))
        })?
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

        // Check if this is a sandbox container
        let is_sandbox = {
            let state = self.sandbox_state.lock().unwrap();
            state
                .get(&req.id)
                .map(|(is_sand, _)| *is_sand)
                .unwrap_or(false)
        };

        if is_sandbox {
            info!("kill() - SANDBOX container, ignoring signal");
            return Ok(api::Empty::new());
        }

        // Real workload - call reaper-runtime with timeout
        info!("kill() - WORKLOAD container, calling reaper-runtime");

        // Call reaper-runtime kill <container-id> <signal>
        // Must complete quickly - kubelet has a short timeout for kill operations
        let runtime_path = self.runtime_path.clone();
        let container_id = req.id.clone();
        let container_id_for_warning = container_id.clone();
        let signal = req.signal;

        // Use a 5-second timeout for kill operations (kubelet timeout is typically 2s per attempt)
        let kill_future = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&runtime_path)
                .arg("kill")
                .arg(&container_id)
                .arg(signal.to_string())
                .output()
        });

        let output =
            match tokio::time::timeout(std::time::Duration::from_secs(5), kill_future).await {
                Ok(result) => result
                    .map_err(|e| {
                        tracing::error!("Failed to spawn reaper-runtime task: {}", e);
                        ttrpc::Error::RpcStatus(ttrpc::get_status(
                            ttrpc::Code::INTERNAL,
                            format!("Failed to spawn reaper-runtime task: {}", e),
                        ))
                    })?
                    .map_err(|e| {
                        tracing::error!("Failed to execute reaper-runtime kill: {}", e);
                        ttrpc::Error::RpcStatus(ttrpc::get_status(
                            ttrpc::Code::INTERNAL,
                            format!("Failed to execute reaper-runtime kill: {}", e),
                        ))
                    })?,
                Err(_) => {
                    tracing::warn!(
                        "kill() timeout after 5s for container {} - returning success anyway",
                        container_id_for_warning
                    );
                    // Return success even if timeout - don't let kill operations block pod cleanup
                    return Ok(api::Empty::new());
                }
            };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("reaper-runtime kill failed: {}", stderr);
            // For kill, we're lenient - process might already be dead (ESRCH)
            // Just return success to avoid blocking pod cleanup
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

        // Check if this is a sandbox container
        let is_sandbox = {
            let state = self.sandbox_state.lock().unwrap();
            state
                .get(&req.id)
                .map(|(is_sand, _)| *is_sand)
                .unwrap_or(false)
        };

        if is_sandbox {
            info!("wait() - SANDBOX container, returning immediately with exit status 0");
            let mut resp = api::WaitResponse::new();
            resp.set_exit_status(0);
            // Set exited_at timestamp
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let mut timestamp = ::protobuf::well_known_types::timestamp::Timestamp::new();
            timestamp.seconds = now.as_secs() as i64;
            timestamp.nanos = now.subsec_nanos() as i32;
            resp.exited_at = ::protobuf::MessageField::some(timestamp);
            return Ok(resp);
        }

        // Real workload - poll state until monitoring daemon marks it stopped
        info!("wait() - WORKLOAD container, polling runtime state for completion");

        // Poll the runtime state until the container stops
        // The monitoring daemon forked by reaper-runtime will update the state when the process exits
        let container_id = req.id.clone();
        let runtime_path = self.runtime_path.clone();

        // Return both exit_code and pid with a timeout to prevent hanging during pod cleanup
        let (exit_code, pid) = tokio::task::spawn_blocking(move || {
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(60); // 60 second timeout for polling (CI environments may be slower)

            loop {
                // Check timeout
                if start.elapsed() > timeout {
                    tracing::warn!(
                        "wait() polling timeout after 30s for container {}",
                        container_id
                    );
                    return (1, 0); // Return error exit code on timeout
                }

                let output = std::process::Command::new(&runtime_path)
                    .arg("state")
                    .arg(&container_id)
                    .output();

                if let Ok(output) = output {
                    if output.status.success() {
                        if let Ok(state) =
                            serde_json::from_slice::<serde_json::Value>(&output.stdout)
                        {
                            if state["status"].as_str() == Some("stopped") {
                                let code = state["exit_code"].as_i64().unwrap_or(0) as i32;
                                let pid = state["pid"].as_u64().unwrap_or(0) as u32;
                                info!(
                                    "wait() - container {} stopped with exit_code={}, pid={}",
                                    container_id, code, pid
                                );
                                return (code, pid);
                            }
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })
        .await
        .unwrap_or((1, 0));

        // Publish TaskExit event to notify containerd
        self.publish_exit_event(&req.id, pid, exit_code as u32)
            .await;

        let mut resp = api::WaitResponse::new();
        resp.set_exit_status(exit_code as u32);

        // Set exited_at timestamp - required for containerd to recognize the exit
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let mut timestamp = ::protobuf::well_known_types::timestamp::Timestamp::new();
        timestamp.seconds = now.as_secs() as i64;
        timestamp.nanos = now.subsec_nanos() as i32;
        resp.exited_at = ::protobuf::MessageField::some(timestamp);

        info!(
            "wait() task completed - container_id={}, exit_code={}",
            req.id, exit_code
        );
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

        // Check if this is a sandbox container
        let is_sandbox = {
            let state = self.sandbox_state.lock().unwrap();
            state
                .get(&req.id)
                .map(|(is_sand, _)| *is_sand)
                .unwrap_or(false)
        };

        if is_sandbox {
            info!("state() - SANDBOX container, returning running state");
            return Ok(api::StateResponse {
                id: req.id,
                bundle: String::new(),
                pid: 1,
                status: ::protobuf::EnumOrUnknown::new(api::Status::RUNNING),
                ..Default::default()
            });
        }

        // Real workload - query reaper-runtime
        info!("state() - WORKLOAD container, querying reaper-runtime");

        // Query runtime for actual state
        let runtime_path = self.runtime_path.clone();
        let container_id = req.id.clone();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&runtime_path)
                .arg("state")
                .arg(&container_id)
                .output()
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to spawn reaper-runtime task: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to spawn reaper-runtime task: {}", e),
            ))
        })?
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

        // If stopped, include exit status and exited_at timestamp
        if status_str == "stopped" {
            resp.exit_status = state["exit_code"].as_u64().unwrap_or(0) as u32;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let mut timestamp = ::protobuf::well_known_types::timestamp::Timestamp::new();
            timestamp.seconds = now.as_secs() as i64;
            timestamp.nanos = now.subsec_nanos() as i32;
            resp.exited_at = ::protobuf::MessageField::some(timestamp);
        }

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
        let runtime_path = self.runtime_path.clone();
        let container_id = req.id.clone();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&runtime_path)
                .arg("state")
                .arg(&container_id)
                .output()
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to spawn reaper-runtime task: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to spawn reaper-runtime task: {}", e),
            ))
        })?
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

    async fn connect(
        &self,
        _ctx: &TtrpcContext,
        req: api::ConnectRequest,
    ) -> TtrpcResult<api::ConnectResponse> {
        info!("connect() called - container_id={}", req.id);

        // Check if this is a sandbox container
        let is_sandbox = {
            let state = self.sandbox_state.lock().unwrap();
            state
                .get(&req.id)
                .map(|(is_sand, _)| *is_sand)
                .unwrap_or(false)
        };

        if is_sandbox {
            info!("connect() - SANDBOX container, returning fake PID");
            let mut resp = api::ConnectResponse::new();
            resp.set_task_pid(1); // Fake PID for sandbox
            resp.set_shim_pid(std::process::id());
            return Ok(resp);
        }

        // Real workload - get PID from reaper-runtime
        info!("connect() - WORKLOAD container, querying reaper-runtime");

        let runtime_path = self.runtime_path.clone();
        let container_id = req.id.clone();
        let state_output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&runtime_path)
                .arg("state")
                .arg(&container_id)
                .output()
        })
        .await
        .map_err(|e| {
            tracing::error!("Failed to spawn reaper-runtime task: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to spawn reaper-runtime task: {}", e),
            ))
        })?
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
        let mut resp = api::ConnectResponse::new();
        resp.set_task_pid(pid);
        // shim_pid is the current process pid
        resp.set_shim_pid(std::process::id());

        info!(
            "connect() succeeded - container_id={}, task_pid={}, shim_pid={}",
            req.id,
            pid,
            std::process::id()
        );
        Ok(resp)
    }

    async fn shutdown(
        &self,
        _ctx: &TtrpcContext,
        req: api::ShutdownRequest,
    ) -> TtrpcResult<api::Empty> {
        info!(
            "shutdown() called - container_id={}, now={}",
            req.id, req.now
        );

        // For now, we acknowledge the shutdown request
        // The shim will exit when containerd disconnects
        info!("shutdown() succeeded - container_id={}", req.id);
        Ok(api::Empty::new())
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
                    EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| EnvFilter::new("info,containerd_shim=debug")),
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

    // Log environment to help debug why server might not start
    let args: Vec<String> = std::env::args().collect();
    info!("Process args: {:?}", args);
    info!("Working directory: {:?}", std::env::current_dir());

    // Check if address file exists (used by child process)
    if let Ok(address_content) = std::fs::read_to_string("address") {
        info!("Found address file with content: {}", address_content);
    } else {
        info!("No address file found in working directory");
    }

    // Create Config with no_setup_logger=true since we already set up tracing
    // Keep no_reaper=true to avoid interfering with tokio's async process management
    // (the containerd-shim reaper can interfere with tokio's Command spawning)
    // Instead, we'll use std::process::Command in blocking contexts for better control.
    let config = Config {
        no_setup_logger: true,
        no_reaper: true, // Keep disabled to avoid tokio/signal handler conflicts
        ..Default::default()
    };

    run::<ReaperShim>("io.containerd.reaper.v2", Some(config)).await;
    info!("containerd_shim::run() completed normally");
}
