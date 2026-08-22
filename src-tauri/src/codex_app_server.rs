//! Async stdio client for the Codex App Server protocol.
//!
//! App Server speaks newline-delimited JSON-RPC 2.0 with the `jsonrpc` member
//! omitted. The stable handshake is an `initialize` request followed by an
//! `initialized` notification. Protocol reference:
//! <https://developers.openai.com/codex/app-server/>
//!
//! This module deliberately keeps notification payloads and unknown response
//! fields as `serde_json::Value`: the installed Codex CLI owns the exact schema,
//! and newer versions may add fields without requiring a Duet release.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter,
    },
    process::{Child, Command},
    sync::{broadcast, mpsc, oneshot, Mutex},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_millis(400);
const DEFAULT_MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_NOTIFICATION_CAPACITY: usize = 2_048;
const STDERR_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("Codex App Server I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Codex App Server returned invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex App Server RPC {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("Codex App Server request `{method}` timed out")]
    RequestTimeout { method: String },
    #[error("Codex App Server connection closed{detail}")]
    ConnectionClosed { detail: String },
    #[error("Codex App Server request was cancelled")]
    Cancelled,
    #[error("Codex App Server notification receiver lagged by {0} messages")]
    NotificationLagged(u64),
    #[error("Codex App Server protocol error: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone)]
struct RpcFailure {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl From<RpcFailure> for AppServerError {
    fn from(value: RpcFailure) -> Self {
        Self::Rpc {
            code: value.code,
            message: value.message,
            data: value.data,
        }
    }
}

type PendingResponse = oneshot::Sender<Result<Value, RpcFailure>>;
type PendingRequests = Arc<Mutex<HashMap<u64, PendingResponse>>>;

#[derive(Debug, Clone)]
pub struct AppServerConfig {
    pub binary: PathBuf,
    pub extra_args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub client_info: ClientInfo,
    pub capabilities: InitializeCapabilities,
    pub request_timeout: Duration,
    pub shutdown_grace: Duration,
    pub max_message_bytes: usize,
    pub notification_capacity: usize,
}

impl AppServerConfig {
    pub fn new(binary: impl Into<PathBuf>, client_info: ClientInfo) -> Self {
        Self {
            binary: binary.into(),
            extra_args: Vec::new(),
            cwd: None,
            client_info,
            capabilities: InitializeCapabilities::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            notification_capacity: DEFAULT_NOTIFICATION_CAPACITY,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub title: String,
    pub version: String,
}

impl ClientInfo {
    pub fn duet(version: impl Into<String>) -> Self {
        Self {
            name: "duet_desktop".into(),
            title: "Duet".into(),
            version: version.into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeCapabilities {
    #[serde(default, skip_serializing_if = "is_false")]
    pub experimental_api: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opt_out_notification_methods: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_attestation: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mcp_server_openai_form_elicitation: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    client_info: ClientInfo,
    #[serde(skip_serializing_if = "capabilities_are_empty")]
    capabilities: InitializeCapabilities,
}

fn capabilities_are_empty(value: &InitializeCapabilities) -> bool {
    !value.experimental_api
        && value.opt_out_notification_methods.is_empty()
        && !value.request_attestation
        && !value.mcp_server_openai_form_elicitation
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub platform_family: Option<String>,
    #[serde(default)]
    pub platform_os: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub model: String,
    pub display_name: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub supports_personality: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub upgrade: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_input_modalities() -> Vec<String> {
    vec!["text".into(), "image".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    #[serde(default)]
    pub data: Vec<ModelInfo>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ephemeral: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInfo {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResponse {
    pub thread: ThreadInfo,
    #[serde(default)]
    pub instruction_sources: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TurnInput {
    Text { text: String },
    Image { url: String },
    LocalImage { path: String },
    Skill { name: String, path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<TurnInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl TurnStartParams {
    pub fn text(thread_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            input: vec![TurnInput::Text { text: text.into() }],
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            model: None,
            effort: None,
            summary: None,
            personality: None,
            output_schema: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInfo {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: TurnInfo,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ServerNotification {
    pub method: String,
    pub params: Value,
}

impl ServerNotification {
    pub fn turn_completed(&self) -> Result<Option<TurnInfo>, AppServerError> {
        if self.method != "turn/completed" {
            return Ok(None);
        }
        let turn = self
            .params
            .get("turn")
            .cloned()
            .ok_or_else(|| AppServerError::Protocol("turn/completed omitted `turn`".into()))?;
        Ok(Some(serde_json::from_value(turn)?))
    }
}

#[derive(Debug, Clone)]
pub struct ServerRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

enum SupervisorCommand {
    Shutdown {
        grace: Duration,
        completed: Option<oneshot::Sender<()>>,
    },
}

struct Inner {
    writer: Mutex<BufWriter<tokio::process::ChildStdin>>,
    pending: PendingRequests,
    next_id: AtomicU64,
    notifications: broadcast::Sender<ServerNotification>,
    server_requests: broadcast::Sender<ServerRequest>,
    supervisor: mpsc::Sender<SupervisorCommand>,
    closed: CancellationToken,
    stderr_tail: Arc<StdMutex<String>>,
    request_timeout: Duration,
    shutdown_grace: Duration,
    process_group: Arc<AtomicU64>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.supervisor.try_send(SupervisorCommand::Shutdown {
            grace: Duration::ZERO,
            completed: None,
        });
        kill_process_group_now(owned_process_group(&self.process_group));
    }
}

#[derive(Clone)]
pub struct CodexAppServerClient {
    inner: Arc<Inner>,
    pub initialize_response: InitializeResponse,
}

impl CodexAppServerClient {
    pub async fn spawn(config: AppServerConfig) -> Result<Self, AppServerError> {
        let mut args = vec!["app-server".to_string()];
        args.extend(config.extra_args.clone());
        Self::spawn_command(config, args).await
    }

    async fn spawn_command(
        config: AppServerConfig,
        args: Vec<String>,
    ) -> Result<Self, AppServerError> {
        if config.max_message_bytes == 0 {
            return Err(AppServerError::Protocol(
                "max_message_bytes must be greater than zero".into(),
            ));
        }
        if config.notification_capacity == 0 {
            return Err(AppServerError::Protocol(
                "notification_capacity must be greater than zero".into(),
            ));
        }

        let mut command = Command::new(&config.binary);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        let process_group = child.id();
        let owned_process_group = Arc::new(AtomicU64::new(process_group.unwrap_or(0) as u64));
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppServerError::Protocol("failed to capture app-server stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AppServerError::Protocol("failed to capture app-server stdout".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppServerError::Protocol("failed to capture app-server stderr".into())
        })?;

        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (notifications, _) = broadcast::channel(config.notification_capacity);
        let (server_requests, _) = broadcast::channel(config.notification_capacity.min(256));
        let closed = CancellationToken::new();
        let stderr_tail = Arc::new(StdMutex::new(String::new()));
        let (supervisor, supervisor_rx) = mpsc::channel(2);

        tokio::spawn(supervise_child(
            child,
            process_group,
            owned_process_group.clone(),
            supervisor_rx,
            closed.clone(),
        ));
        tokio::spawn(read_stderr(stderr, stderr_tail.clone()));
        tokio::spawn(read_messages(
            stdout,
            config.max_message_bytes,
            pending.clone(),
            notifications.clone(),
            server_requests.clone(),
            supervisor.clone(),
            closed.clone(),
        ));

        let inner = Arc::new(Inner {
            writer: Mutex::new(BufWriter::new(stdin)),
            pending,
            next_id: AtomicU64::new(1),
            notifications,
            server_requests,
            supervisor,
            closed,
            stderr_tail,
            request_timeout: config.request_timeout,
            shutdown_grace: config.shutdown_grace,
            process_group: owned_process_group,
        });
        let provisional = Self {
            inner,
            initialize_response: InitializeResponse::default(),
        };
        let initialize_response = provisional
            .request(
                "initialize",
                InitializeParams {
                    client_info: config.client_info,
                    capabilities: config.capabilities,
                },
            )
            .await?;
        provisional
            .notify("initialized", serde_json::json!({}))
            .await?;
        Ok(Self {
            initialize_response,
            ..provisional
        })
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<ServerNotification> {
        self.inner.notifications.subscribe()
    }

    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<ServerRequest> {
        self.inner.server_requests.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.is_cancelled()
    }

    pub fn stderr_tail(&self) -> String {
        self.inner
            .stderr_tail
            .lock()
            .map(|tail| tail.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub async fn list_models(
        &self,
        params: ModelListParams,
    ) -> Result<ModelListResponse, AppServerError> {
        self.request("model/list", params).await
    }

    pub async fn start_thread(
        &self,
        params: ThreadStartParams,
    ) -> Result<ThreadStartResponse, AppServerError> {
        self.request("thread/start", params).await
    }

    pub async fn start_turn(
        &self,
        params: TurnStartParams,
    ) -> Result<TurnStartResponse, AppServerError> {
        self.request("turn/start", params).await
    }

    pub async fn start_turn_stream(
        &self,
        params: TurnStartParams,
        cancel: CancellationToken,
    ) -> Result<ActiveTurn, AppServerError> {
        if cancel.is_cancelled() {
            return Err(AppServerError::Cancelled);
        }
        let thread_id = params.thread_id.clone();
        let notifications = self.subscribe_notifications();
        let response = self.start_turn(params).await?;
        Ok(ActiveTurn {
            client: self.clone(),
            thread_id,
            turn_id: response.turn.id,
            notifications,
            cancel,
        })
    }

    pub async fn interrupt_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), AppServerError> {
        let _: Value = self
            .request(
                "turn/interrupt",
                serde_json::json!({"threadId": thread_id, "turnId": turn_id}),
            )
            .await?;
        Ok(())
    }

    pub async fn respond_to_server_request(
        &self,
        id: Value,
        result: Value,
    ) -> Result<(), AppServerError> {
        self.write_message(&serde_json::json!({"id": id, "result": result}))
            .await
    }

    pub async fn respond_to_server_error(
        &self,
        id: Value,
        code: i64,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Result<(), AppServerError> {
        self.write_message(&serde_json::json!({
            "id": id,
            "error": {"code": code, "message": message.into(), "data": data}
        }))
        .await
    }

    pub async fn request<T, P>(&self, method: &str, params: P) -> Result<T, AppServerError>
    where
        T: DeserializeOwned,
        P: Serialize,
    {
        let value = serde_json::to_value(params)?;
        let response = self.request_value(method, value, None).await?;
        Ok(serde_json::from_value(response)?)
    }

    pub async fn request_with_cancel<T, P>(
        &self,
        method: &str,
        params: P,
        cancel: CancellationToken,
    ) -> Result<T, AppServerError>
    where
        T: DeserializeOwned,
        P: Serialize,
    {
        let value = serde_json::to_value(params)?;
        let response = self.request_value(method, value, Some(cancel)).await?;
        Ok(serde_json::from_value(response)?)
    }

    async fn request_value(
        &self,
        method: &str,
        params: Value,
        cancel: Option<CancellationToken>,
    ) -> Result<Value, AppServerError> {
        if self.is_closed() {
            return Err(self.closed_error());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, sender);
        if let Err(error) = self
            .write_message(&serde_json::json!({"method": method, "id": id, "params": params}))
            .await
        {
            self.inner.pending.lock().await.remove(&id);
            return Err(error);
        }

        let deadline = tokio::time::sleep(self.inner.request_timeout);
        tokio::pin!(deadline);
        let cancelled = cancel.unwrap_or_default();
        let result = tokio::select! {
            biased;
            response = receiver => match response {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(error.into()),
                Err(_) => Err(self.closed_error()),
            },
            _ = self.inner.closed.cancelled() => Err(self.closed_error()),
            _ = cancelled.cancelled() => Err(AppServerError::Cancelled),
            _ = &mut deadline => Err(AppServerError::RequestTimeout { method: method.into() }),
        };
        if result.is_err() {
            self.inner.pending.lock().await.remove(&id);
        }
        result
    }

    async fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<(), AppServerError> {
        self.write_message(&serde_json::json!({"method": method, "params": params}))
            .await
    }

    async fn write_message(&self, message: &Value) -> Result<(), AppServerError> {
        if self.is_closed() {
            return Err(self.closed_error());
        }
        let mut bytes = serde_json::to_vec(message)?;
        bytes.push(b'\n');
        let mut writer = self.inner.writer.lock().await;
        writer.write_all(&bytes).await?;
        writer.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), AppServerError> {
        if self.is_closed() {
            return Ok(());
        }
        let (completed, wait) = oneshot::channel();
        self.inner
            .supervisor
            .send(SupervisorCommand::Shutdown {
                grace: self.inner.shutdown_grace,
                completed: Some(completed),
            })
            .await
            .map_err(|_| self.closed_error())?;
        timeout(self.inner.shutdown_grace + Duration::from_secs(3), wait)
            .await
            .map_err(|_| AppServerError::RequestTimeout {
                method: "shutdown".into(),
            })?
            .map_err(|_| self.closed_error())?;
        Ok(())
    }

    fn closed_error(&self) -> AppServerError {
        let stderr = self.stderr_tail();
        let detail = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        };
        AppServerError::ConnectionClosed { detail }
    }
}

pub struct ActiveTurn {
    client: CodexAppServerClient,
    pub thread_id: String,
    pub turn_id: String,
    notifications: broadcast::Receiver<ServerNotification>,
    cancel: CancellationToken,
}

impl ActiveTurn {
    pub async fn next_notification(&mut self) -> Result<ServerNotification, AppServerError> {
        tokio::select! {
            notification = self.notifications.recv() => map_notification(notification),
            _ = self.client.inner.closed.cancelled() => Err(self.client.closed_error()),
        }
    }

    pub async fn interrupt(&self) -> Result<(), AppServerError> {
        self.client
            .interrupt_turn(&self.thread_id, &self.turn_id)
            .await
    }

    pub async fn wait_for_completion(&mut self) -> Result<TurnInfo, AppServerError> {
        let mut interrupt_sent = false;
        loop {
            tokio::select! {
                biased;
                notification = self.notifications.recv() => {
                    let notification = map_notification(notification)?;
                    if let Some(turn) = notification.turn_completed()? {
                        if turn.id == self.turn_id {
                            return Ok(turn);
                        }
                    }
                },
                _ = self.cancel.cancelled(), if !interrupt_sent => {
                    self.interrupt().await?;
                    interrupt_sent = true;
                },
                _ = self.client.inner.closed.cancelled() => return Err(self.client.closed_error()),
            }
        }
    }
}

fn map_notification(
    value: Result<ServerNotification, broadcast::error::RecvError>,
) -> Result<ServerNotification, AppServerError> {
    match value {
        Ok(notification) => Ok(notification),
        Err(broadcast::error::RecvError::Closed) => Err(AppServerError::ConnectionClosed {
            detail: String::new(),
        }),
        Err(broadcast::error::RecvError::Lagged(count)) => {
            Err(AppServerError::NotificationLagged(count))
        }
    }
}

async fn read_messages(
    stdout: tokio::process::ChildStdout,
    max_message_bytes: usize,
    pending: PendingRequests,
    notifications: broadcast::Sender<ServerNotification>,
    server_requests: broadcast::Sender<ServerRequest>,
    supervisor: mpsc::Sender<SupervisorCommand>,
    closed: CancellationToken,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let line = match read_bounded_line(&mut reader, max_message_bytes).await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(_) => break,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let message: Value = match serde_json::from_slice(&line) {
            Ok(message) => message,
            Err(_) => break,
        };
        let Some(object) = message.as_object() else {
            break;
        };

        if let Some(id) = object.get("id").and_then(Value::as_u64) {
            if object.contains_key("result") || object.contains_key("error") {
                if let Some(sender) = pending.lock().await.remove(&id) {
                    let response = if let Some(error) = object.get("error") {
                        Err(parse_rpc_failure(error))
                    } else {
                        Ok(object.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(response);
                }
                continue;
            }
        }

        let Some(method) = object.get("method").and_then(Value::as_str) else {
            break;
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        if let Some(id) = object.get("id") {
            let _ = server_requests.send(ServerRequest {
                id: id.clone(),
                method: method.into(),
                params,
            });
        } else {
            let _ = notifications.send(ServerNotification {
                method: method.into(),
                params,
            });
        }
    }

    closed.cancel();
    fail_pending(&pending, "transport reader stopped").await;
    let _ = supervisor
        .send(SupervisorCommand::Shutdown {
            grace: DEFAULT_SHUTDOWN_GRACE,
            completed: None,
        })
        .await;
}

fn parse_rpc_failure(error: &Value) -> RpcFailure {
    RpcFailure {
        code: error.get("code").and_then(Value::as_i64).unwrap_or(-32_000),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown app-server error")
            .into(),
        data: error.get("data").cloned(),
    }
}

async fn fail_pending(pending: &PendingRequests, message: &str) {
    let requests = {
        let mut pending = pending.lock().await;
        std::mem::take(&mut *pending)
    };
    for (_, sender) in requests {
        let _ = sender.send(Err(RpcFailure {
            code: -32_000,
            message: message.into(),
            data: None,
        }));
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |position| position);
        if line.len().saturating_add(count) > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("app-server message exceeded {max_bytes} bytes"),
            ));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

async fn read_stderr<R: AsyncRead + Unpin>(mut stderr: R, tail: Arc<StdMutex<String>>) {
    let mut buffer = [0u8; 4_096];
    while let Ok(count) = stderr.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        let chunk = String::from_utf8_lossy(&buffer[..count]);
        let mut value = match tail.lock() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        };
        value.push_str(&chunk);
        if value.len() > STDERR_TAIL_BYTES {
            let start = value.ceil_char_boundary(value.len() - STDERR_TAIL_BYTES);
            *value = value[start..].to_string();
        }
    }
}

async fn supervise_child(
    mut child: Child,
    process_group: Option<u32>,
    owned_process_group: Arc<AtomicU64>,
    mut commands: mpsc::Receiver<SupervisorCommand>,
    closed: CancellationToken,
) {
    let (shutdown_requested, completed) = tokio::select! {
        _ = child.wait() => {
            terminate_remaining_group(process_group, DEFAULT_SHUTDOWN_GRACE).await;
            (false, None)
        },
        command = commands.recv() => {
            let grace = command.as_ref().map(|command| match command {
                SupervisorCommand::Shutdown { grace, .. } => *grace,
            }).unwrap_or(Duration::ZERO);
            terminate_child(&mut child, process_group, grace).await;
            let completed = command.and_then(|command| match command {
                SupervisorCommand::Shutdown { completed, .. } => completed,
            });
            (true, completed)
        },
    };
    // Prevent a late `Drop` from signaling a recycled process-group id after
    // the owned child and all of its descendants have been reaped.
    owned_process_group.store(0, Ordering::Release);
    if let Some(completed) = completed {
        let _ = completed.send(());
    }
    // On natural exit, let the stdout reader dispatch every buffered response
    // and notification before it marks the connection closed. On explicit
    // shutdown there may be no EOF consumer left, so close immediately.
    if shutdown_requested {
        closed.cancel();
    }
}

async fn terminate_child(child: &mut Child, process_group: Option<u32>, grace: Duration) {
    #[cfg(unix)]
    if let Some(pid) = process_group {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if !grace.is_zero() {
            tokio::time::sleep(grace).await;
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = timeout(Duration::from_secs(2), child.wait()).await;
        return;
    }
    let _ = child.kill().await;
    let _ = timeout(Duration::from_secs(2), child.wait()).await;
}

async fn terminate_remaining_group(process_group: Option<u32>, grace: Duration) {
    #[cfg(unix)]
    if let Some(pid) = process_group {
        unsafe {
            if libc::kill(-(pid as i32), 0) != 0 {
                return;
            }
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if !grace.is_zero() {
            tokio::time::sleep(grace).await;
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

fn kill_process_group_now(process_group: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = process_group {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

fn owned_process_group(process_group: &AtomicU64) -> Option<u32> {
    match process_group.load(Ordering::Acquire) {
        0 => None,
        pid => u32::try_from(pid).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_config(script: &str) -> (AppServerConfig, Vec<String>) {
        let mut config = AppServerConfig::new("/bin/sh", ClientInfo::duet("test"));
        config.request_timeout = Duration::from_secs(2);
        config.shutdown_grace = Duration::from_millis(20);
        (config, vec!["-c".into(), script.into()])
    }

    #[cfg(unix)]
    async fn fake_client(script: &str) -> CodexAppServerClient {
        let (config, args) = fake_config(script);
        CodexAppServerClient::spawn_command(config, args)
            .await
            .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn initializes_lists_models_and_streams_a_turn() {
        let script = r#"
read_line() { IFS= read -r line || exit 90; }
read_line
case "$line" in *'"method":"initialize"'*) ;; *) exit 91;; esac
printf '%s\n' '{"id":1,"result":{"userAgent":"fake/1","platformFamily":"unix","platformOs":"test"}}'
read_line
case "$line" in *'"method":"initialized"'*) ;; *) exit 92;; esac
read_line
case "$line" in *'"method":"model/list"'*) ;; *) exit 93;; esac
printf '%s\n' '{"id":2,"result":{"data":[{"id":"fake-model","model":"fake-model","displayName":"Fake","isDefault":true}],"nextCursor":null}}'
read_line
case "$line" in *'"method":"thread/start"'*) ;; *) exit 94;; esac
printf '%s\n' '{"id":3,"result":{"thread":{"id":"thr_fake","sessionId":"thr_fake","ephemeral":false}}}'
printf '%s\n' '{"method":"thread/started","params":{"thread":{"id":"thr_fake"}}}'
read_line
case "$line" in *'"method":"turn/start"'*) ;; *) exit 95;; esac
printf '%s\n' '{"id":4,"result":{"turn":{"id":"turn_fake","status":"inProgress","items":[],"error":null}}}'
printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thr_fake","turnId":"turn_fake","itemId":"item_1","delta":"hello"}}'
printf '%s\n' '{"method":"turn/completed","params":{"turn":{"id":"turn_fake","status":"completed","items":[],"error":null}}}'
"#;
        let client = fake_client(script).await;
        assert_eq!(
            client.initialize_response.user_agent.as_deref(),
            Some("fake/1")
        );

        let models = client
            .list_models(ModelListParams::default())
            .await
            .unwrap();
        assert_eq!(models.data[0].id, "fake-model");
        assert_eq!(models.data[0].input_modalities, vec!["text", "image"]);

        let thread = client
            .start_thread(ThreadStartParams::default())
            .await
            .unwrap();
        assert_eq!(thread.thread.id, "thr_fake");
        let mut turn = client
            .start_turn_stream(
                TurnStartParams::text("thr_fake", "hello"),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut saw_delta = false;
        loop {
            let notification = turn.next_notification().await.unwrap();
            if notification.method == "item/agentMessage/delta" {
                saw_delta = true;
            }
            if let Some(completed) = notification.turn_completed().unwrap() {
                assert_eq!(completed.status, "completed");
                break;
            }
        }
        assert!(saw_delta);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_interrupts_the_active_turn_and_waits_for_completion() {
        let script = r#"
read_line() { IFS= read -r line || exit 90; }
read_line
printf '%s\n' '{"id":1,"result":{"userAgent":"fake/1"}}'
read_line
read_line
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr_cancel","ephemeral":false}}}'
read_line
printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn_cancel","status":"inProgress","items":[],"error":null}}}'
read_line
case "$line" in *'"method":"turn/interrupt"'*) ;; *) exit 96;; esac
printf '%s\n' '{"id":4,"result":{}}'
printf '%s\n' '{"method":"turn/completed","params":{"turn":{"id":"turn_cancel","status":"interrupted","items":[],"error":null}}}'
"#;
        let client = fake_client(script).await;
        let thread = client
            .start_thread(ThreadStartParams::default())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let mut turn = client
            .start_turn_stream(
                TurnStartParams::text(thread.thread.id, "keep working"),
                cancel.clone(),
            )
            .await
            .unwrap();
        cancel.cancel();
        let completed = turn.wait_for_completion().await.unwrap();
        assert_eq!(completed.status, "interrupted");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn surfaces_and_answers_server_initiated_requests() {
        let script = r#"
read_line() { IFS= read -r line || exit 90; }
read_line
printf '%s\n' '{"id":1,"result":{"userAgent":"fake/1"}}'
read_line
read_line
printf '%s\n' '{"id":77,"method":"item/commandExecution/requestApproval","params":{"command":"cargo test"}}'
printf '%s\n' '{"id":2,"result":{"data":[],"nextCursor":null}}'
read_line
case "$line" in *'"id":77'*'"decision":"decline"'*) ;; *) exit 97;; esac
"#;
        let client = fake_client(script).await;
        let mut requests = client.subscribe_server_requests();
        let models = client
            .list_models(ModelListParams::default())
            .await
            .unwrap();
        assert!(models.data.is_empty());
        let request = requests.recv().await.unwrap();
        assert_eq!(request.id, serde_json::json!(77));
        assert_eq!(request.method, "item/commandExecution/requestApproval");
        client
            .respond_to_server_request(request.id, serde_json::json!({"decision":"decline"}))
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_terminates_the_owned_child() {
        let script = r#"
read_line() { IFS= read -r line || exit 90; }
read_line
printf '{"id":1,"result":{"userAgent":"fake/1","platformOs":"%s"}}\n' "$$"
read_line
while :; do sleep 30; done
"#;
        let client = fake_client(script).await;
        let pid = client
            .initialize_response
            .platform_os
            .as_deref()
            .unwrap()
            .parse::<u32>()
            .unwrap();
        client.shutdown().await.unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!alive, "fake app-server process {pid} survived shutdown");
    }

    #[tokio::test]
    async fn bounded_reader_rejects_oversized_messages() {
        let bytes = b"123456789\n";
        let mut reader = BufReader::new(&bytes[..]);
        let error = read_bounded_line(&mut reader, 4).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
