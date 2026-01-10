use containerd_shim::{
    asynchronous::{run, spawn, ExitSignal, Shim},
    publisher::RemotePublisher,
    Config, Error, Flags, StartOpts, TtrpcResult,
};
use containerd_shim_protos::{
    api, api::DeleteResponse, shim_async::Task, ttrpc::r#async::TtrpcContext,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct ReaperShim {
    exit: Arc<ExitSignal>,
    commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommandConfig {
    command: String,
    args: Vec<String>,
    env: Vec<String>,
    cwd: Option<String>,
    user: Option<UserConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserConfig {
    uid: u32,
    gid: u32,
}

#[derive(Debug)]
struct CommandInfo {
    id: String,
    bundle: String,
    config: CommandConfig,
    pid: Option<u32>,
    status: CommandStatus,
    child: Option<tokio::process::Child>,
}

#[derive(Debug, Clone, PartialEq)]
enum CommandStatus {
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
            commands: Arc::new(Mutex::new(HashMap::new())),
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
            commands: self.commands.clone(),
        }
    }
}

#[derive(Clone)]
struct ReaperTask {
    commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
}

#[async_trait::async_trait]
impl Task for ReaperTask {
    async fn create(
        &self,
        _ctx: &TtrpcContext,
        req: api::CreateTaskRequest,
    ) -> TtrpcResult<api::CreateTaskResponse> {
        info!("create called for command: {}", req.id);

        // Parse command config from bundle/config.json
        let config_path = std::path::Path::new(&req.bundle).join("config.json");
        let config_content = tokio::fs::read_to_string(&config_path).await.map_err(|e| {
            tracing::error!("Failed to read config.json: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to read config.json: {}", e),
            ))
        })?;

        let config: CommandConfig = serde_json::from_str(&config_content).map_err(|e| {
            tracing::error!("Failed to parse config.json: {}", e);
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INTERNAL,
                format!("Failed to parse config.json: {}", e),
            ))
        })?;

        // Store command info
        let command_info = CommandInfo {
            id: req.id.clone(),
            bundle: req.bundle.clone(),
            config,
            pid: None,
            status: CommandStatus::Created,
            child: None,
        };

        {
            let mut commands = self.commands.lock().unwrap();
            commands.insert(req.id.clone(), command_info);
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
        info!("start called for command: {}", req.id);

        // Get and update command info
        {
            let mut commands = self.commands.lock().unwrap();
            let command_info = commands.get_mut(&req.id).ok_or_else(|| {
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::NOT_FOUND,
                    format!("Command {} not found", req.id),
                ))
            })?;

            // Execute command directly
            let mut cmd = Command::new(&command_info.config.command);
            cmd.args(&command_info.config.args);

            // Set environment variables
            for env_var in &command_info.config.env {
                if let Some((key, value)) = env_var.split_once('=') {
                    cmd.env(key, value);
                }
            }

            // Set working directory
            if let Some(cwd) = &command_info.config.cwd {
                cmd.current_dir(cwd);
            }

            // Set up stdio
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            let child = cmd.spawn().map_err(|e| {
                tracing::error!("Failed to spawn command: {}", e);
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    format!("Failed to spawn command: {}", e),
                ))
            })?;

            let pid = child.id().ok_or_else(|| {
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INTERNAL,
                    "Failed to get child process ID".to_string(),
                ))
            })?;

            // Update command info
            command_info.pid = Some(pid);
            command_info.status = CommandStatus::Running;
            command_info.child = Some(child);

            let mut resp = api::StartResponse::new();
            resp.set_pid(pid);
            Ok(resp)
        }
    }

    async fn delete(
        &self,
        _ctx: &TtrpcContext,
        req: api::DeleteRequest,
    ) -> TtrpcResult<api::DeleteResponse> {
        info!("delete called for command: {}", req.id);

        // Get command info
        let child = {
            let mut commands = self.commands.lock().unwrap();
            if let Some(command_info) = commands.get_mut(&req.id) {
                command_info.child.take() // Take ownership of the child
            } else {
                return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::NOT_FOUND,
                    format!("Command {} not found", req.id),
                )));
            }
        };

        // Wait for child process if it exists
        let exit_status = if let Some(mut child) = child {
            match child.wait().await {
                Ok(status) => status.code().unwrap_or(0),
                Err(_) => 0,
            }
        } else {
            0
        };

        // Update status to Stopped and remove from tracking
        {
            let mut commands = self.commands.lock().unwrap();
            if let Some(command_info) = commands.get_mut(&req.id) {
                command_info.status = CommandStatus::Stopped;
            }
            commands.remove(&req.id);
        }

        let mut resp = api::DeleteResponse::new();
        resp.set_pid(0);
        resp.set_exit_status(exit_status as u32);
        Ok(resp)
    }

    async fn kill(&self, _ctx: &TtrpcContext, req: api::KillRequest) -> TtrpcResult<api::Empty> {
        info!("kill called for command: {} signal: {}", req.id, req.signal);

        // Send signal to child process
        {
            let commands = self.commands.lock().unwrap();
            if let Some(command_info) = commands.get(&req.id) {
                if let Some(pid) = command_info.pid {
                    // Send signal to process
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid;

                    let signal = match req.signal {
                        9 => Signal::SIGKILL,
                        15 => Signal::SIGTERM,
                        _ => Signal::SIGTERM, // Default to SIGTERM
                    };

                    if let Err(e) = kill(Pid::from_raw(pid as i32), signal) {
                        tracing::error!("Failed to send signal to process {}: {}", pid, e);
                        return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                            ttrpc::Code::INTERNAL,
                            format!("Failed to kill process: {}", e),
                        )));
                    }
                }
            } else {
                return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::NOT_FOUND,
                    format!("Command {} not found", req.id),
                )));
            }
        }

        Ok(api::Empty::new())
    }

    async fn wait(
        &self,
        _ctx: &TtrpcContext,
        req: api::WaitRequest,
    ) -> TtrpcResult<api::WaitResponse> {
        info!("wait called for command: {}", req.id);

        // Take the child process to wait on
        let child = {
            let mut commands = self.commands.lock().unwrap();
            if let Some(command_info) = commands.get_mut(&req.id) {
                command_info.child.take()
            } else {
                return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::NOT_FOUND,
                    format!("Command {} not found", req.id),
                )));
            }
        };

        // Wait for the child process to exit
        let exit_status = if let Some(mut child) = child {
            match child.wait().await {
                Ok(status) => status.code().unwrap_or(0),
                Err(e) => {
                    tracing::error!("Failed to wait for process: {}", e);
                    0
                }
            }
        } else {
            0
        };

        // Update command status to Stopped
        {
            let mut commands = self.commands.lock().unwrap();
            if let Some(command_info) = commands.get_mut(&req.id) {
                command_info.status = CommandStatus::Stopped;
            }
        }

        let mut resp = api::WaitResponse::new();
        resp.set_exit_status(exit_status as u32);
        Ok(resp)
    }

    async fn state(
        &self,
        _ctx: &TtrpcContext,
        req: api::StateRequest,
    ) -> TtrpcResult<api::StateResponse> {
        info!("state called for command: {}", req.id);

        let commands = self.commands.lock().unwrap();
        let command = commands.get(&req.id);

        let mut resp = api::StateResponse::new();
        if let Some(command) = command {
            resp.id = command.id.clone();
            resp.bundle = command.bundle.clone();
            resp.pid = command.pid.unwrap_or(0);
            resp.status = match command.status {
                CommandStatus::Created => ::protobuf::EnumOrUnknown::new(api::Status::CREATED),
                CommandStatus::Running => ::protobuf::EnumOrUnknown::new(api::Status::RUNNING),
                CommandStatus::Stopped => ::protobuf::EnumOrUnknown::new(api::Status::STOPPED),
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
        info!("pids called for command: {}", req.id);

        let commands = self.commands.lock().unwrap();
        let command = commands.get(&req.id);

        let mut resp = api::PidsResponse::new();
        if let Some(command) = command {
            if let Some(pid) = command.pid {
                let mut process = api::ProcessInfo::new();
                process.pid = pid;
                resp.processes.push(process);
            }
        }

        Ok(resp)
    }

    async fn exec(
        &self,
        _ctx: &TtrpcContext,
        req: api::ExecProcessRequest,
    ) -> TtrpcResult<api::Empty> {
        info!(
            "exec called for command: {} with spec: {:?}",
            req.id,
            req.spec.as_ref().map(|s| &s.type_url)
        );

        // Verify the parent command exists and is running
        {
            let commands = self.commands.lock().unwrap();
            let command_info = commands.get(&req.id).ok_or_else(|| {
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::NOT_FOUND,
                    format!("Parent command {} not found", req.id),
                ))
            })?;

            if command_info.status != CommandStatus::Running {
                return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::FAILED_PRECONDITION,
                    format!("Parent command {} is not running", req.id),
                )));
            }
        }

        // For Milestone 4: Implement exec functionality
        // This would allow running additional commands within the context of an already running process
        // For our use case, this could mean spawning related processes or sub-commands
        // For now, we don't support exec since we run independent commands
        Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
            ttrpc::Code::UNIMPLEMENTED,
            "Exec not supported - each command runs independently".to_string(),
        )))
    }

    async fn stats(
        &self,
        _ctx: &TtrpcContext,
        req: api::StatsRequest,
    ) -> TtrpcResult<api::StatsResponse> {
        info!("stats called for command: {}", req.id);

        // Verify command exists
        {
            let commands = self.commands.lock().unwrap();
            commands.get(&req.id).ok_or_else(|| {
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::NOT_FOUND,
                    format!("Command {} not found", req.id),
                ))
            })?;
        }

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
            "resize_pty called for command: {} to {}x{}",
            req.id, req.width, req.height
        );

        // Verify command exists and is running
        {
            let commands = self.commands.lock().unwrap();
            let command_info = commands.get(&req.id).ok_or_else(|| {
                ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::NOT_FOUND,
                    format!("Command {} not found", req.id),
                ))
            })?;

            if command_info.status != CommandStatus::Running {
                return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::FAILED_PRECONDITION,
                    format!("Command {} is not running", req.id),
                )));
            }
        }

        // For Milestone 4: Implement terminal resizing
        // This would send SIGWINCH to the process and update terminal size
        // For now, we don't support interactive resizing since we run non-interactive commands
        Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
            ttrpc::Code::UNIMPLEMENTED,
            "ResizePty not supported for non-interactive commands".to_string(),
        )))
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
