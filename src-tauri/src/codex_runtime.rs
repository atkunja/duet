//! Safe, long-lived runtime ownership for interactive Codex App Server sessions.
//!
//! The lower-level client intentionally exposes raw notification and server-request
//! broadcasts. This wrapper subscribes to both before it can be returned to a
//! caller, retains server requests until they are answered, and gives UI clients
//! opaque request tokens instead of protocol request IDs.

use crate::codex_app_server::{
    AppServerConfig, AppServerError, CodexAppServerClient, InitializeResponse, ModelListParams,
    ModelListResponse, ServerNotification, ServerRequest, ThreadStartParams, ThreadStartResponse,
    TurnStartParams, TurnStartResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::{broadcast, mpsc, Mutex, Notify},
    task::JoinHandle,
    time::{timeout, Instant},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEFAULT_SERVER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_EVENT_HISTORY_CAPACITY: usize = 2_048;
const DEFAULT_AUTOMATIC_RESPONSE_CAPACITY: usize = 256;
const METHOD_NOT_FOUND: i64 = -32_601;
const CLIENT_RESPONSE_TIMEOUT: i64 = -32_001;
const CLIENT_SHUTTING_DOWN: i64 = -32_000;

#[derive(Debug, Error)]
pub enum CodexRuntimeError {
    #[error(transparent)]
    AppServer(#[from] AppServerError),
    #[error("invalid Codex runtime configuration: {0}")]
    InvalidConfiguration(String),
    #[error("unknown or already-resolved Codex request token")]
    UnknownRequestToken,
    #[error("Codex runtime is shutting down")]
    ShuttingDown,
    #[error("Codex runtime event stream closed")]
    EventStreamClosed,
    #[error("Codex runtime event receiver lagged by {0} events")]
    EventStreamLagged(u64),
}

#[derive(Debug, Clone)]
pub struct CodexRuntimeConfig {
    pub app_server: AppServerConfig,
    pub server_request_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub event_history_capacity: usize,
    pub automatic_response_capacity: usize,
}

impl CodexRuntimeConfig {
    pub fn new(app_server: AppServerConfig) -> Self {
        Self {
            app_server,
            server_request_timeout: DEFAULT_SERVER_REQUEST_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            event_history_capacity: DEFAULT_EVENT_HISTORY_CAPACITY,
            automatic_response_capacity: DEFAULT_AUTOMATIC_RESPONSE_CAPACITY,
        }
    }

    fn validate(&self) -> Result<(), CodexRuntimeError> {
        if self.server_request_timeout.is_zero() {
            return Err(CodexRuntimeError::InvalidConfiguration(
                "server_request_timeout must be greater than zero".into(),
            ));
        }
        if self.shutdown_timeout.is_zero() {
            return Err(CodexRuntimeError::InvalidConfiguration(
                "shutdown_timeout must be greater than zero".into(),
            ));
        }
        if self.event_history_capacity == 0 {
            return Err(CodexRuntimeError::InvalidConfiguration(
                "event_history_capacity must be greater than zero".into(),
            ));
        }
        if self.automatic_response_capacity == 0 {
            return Err(CodexRuntimeError::InvalidConfiguration(
                "automatic_response_capacity must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// An opaque handle for a server-initiated JSON-RPC request.
///
/// The underlying App Server request ID never crosses this API boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeRequestToken(String);

impl RuntimeRequestToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeRequestResolution {
    RespondedSuccess,
    RespondedError,
    TimedOut,
    ShuttingDown,
    ClearedByServer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CodexRuntimeEvent {
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        token: RuntimeRequestToken,
        method: String,
        params: Value,
    },
    ServerRequestResolved {
        token: RuntimeRequestToken,
        resolution: RuntimeRequestResolution,
    },
    ServerRequestRejected {
        method: String,
        reason: String,
    },
    NotificationStreamLagged {
        skipped: u64,
    },
    FatalProtocolError {
        message: String,
    },
    ShuttingDown,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencedRuntimeEvent {
    pub sequence: u64,
    pub event: CodexRuntimeEvent,
}

struct PendingServerRequest {
    protocol_id: Value,
    method: String,
    deadline: Instant,
    event: SequencedRuntimeEvent,
}

struct RuntimeState {
    next_sequence: u64,
    history_capacity: usize,
    history: VecDeque<SequencedRuntimeEvent>,
    pending: HashMap<RuntimeRequestToken, PendingServerRequest>,
    terminal: bool,
}

impl RuntimeState {
    fn new(history_capacity: usize) -> Self {
        Self {
            next_sequence: 1,
            history_capacity,
            history: VecDeque::with_capacity(history_capacity),
            pending: HashMap::new(),
            terminal: false,
        }
    }

    fn record(&mut self, event: CodexRuntimeEvent) -> Option<SequencedRuntimeEvent> {
        if self.terminal {
            return None;
        }
        let envelope = SequencedRuntimeEvent {
            sequence: self.next_sequence,
            event,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        if self.history.len() == self.history_capacity {
            self.history.pop_front();
        }
        self.history.push_back(envelope.clone());
        Some(envelope)
    }

    fn close(&mut self) -> Option<SequencedRuntimeEvent> {
        let event = self.record(CodexRuntimeEvent::Closed)?;
        self.terminal = true;
        Some(event)
    }
}

struct AutomaticErrorResponse {
    protocol_id: Value,
    method: String,
    code: i64,
    message: String,
    token: Option<(RuntimeRequestToken, RuntimeRequestResolution)>,
}

struct RuntimeInner {
    client: CodexAppServerClient,
    state: Arc<Mutex<RuntimeState>>,
    events: broadcast::Sender<SequencedRuntimeEvent>,
    deadline_changed: Arc<Notify>,
    stop: CancellationToken,
    shutdown_complete: CancellationToken,
    closed_event_sent: Arc<AtomicBool>,
    shutdown_started: AtomicBool,
    shutdown_lock: Mutex<()>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    automatic_errors: mpsc::Sender<AutomaticErrorResponse>,
    shutdown_timeout: Duration,
    notification_drained: CancellationToken,
    request_drained: CancellationToken,
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.stop.cancel();
        for task in self.tasks.get_mut().iter() {
            task.abort();
        }
    }
}

#[derive(Clone)]
pub struct CodexRuntime {
    inner: Arc<RuntimeInner>,
}

impl CodexRuntime {
    pub async fn spawn(config: CodexRuntimeConfig) -> Result<Self, CodexRuntimeError> {
        config.validate()?;
        let server_request_timeout = config.server_request_timeout;
        let client = CodexAppServerClient::spawn(config.app_server).await?;

        // These receivers are created before the runtime is returned. App Server
        // events emitted by the first public thread/turn request therefore have a
        // live consumer even if the frontend subscribes a little later.
        let notifications = client.subscribe_notifications();
        let server_requests = client.subscribe_server_requests();
        let state = Arc::new(Mutex::new(RuntimeState::new(config.event_history_capacity)));
        let (events, _) = broadcast::channel(config.event_history_capacity);
        let (automatic_errors, automatic_error_rx) =
            mpsc::channel(config.automatic_response_capacity);
        let deadline_changed = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let notification_drained = CancellationToken::new();
        let request_drained = CancellationToken::new();
        let closed_event_sent = Arc::new(AtomicBool::new(false));

        let inner = Arc::new(RuntimeInner {
            client,
            state,
            events,
            deadline_changed,
            stop,
            shutdown_complete: CancellationToken::new(),
            closed_event_sent,
            shutdown_started: AtomicBool::new(false),
            shutdown_lock: Mutex::new(()),
            tasks: Mutex::new(Vec::new()),
            automatic_errors,
            shutdown_timeout: config.shutdown_timeout,
            notification_drained: notification_drained.clone(),
            request_drained: request_drained.clone(),
        });

        let tasks = vec![
            tokio::spawn(notification_pump(
                notifications,
                inner.state.clone(),
                inner.events.clone(),
                inner.deadline_changed.clone(),
                inner.stop.clone(),
                notification_drained,
            )),
            tokio::spawn(server_request_pump(
                server_requests,
                RequestPumpContext {
                    client: inner.client.clone(),
                    state: inner.state.clone(),
                    events: inner.events.clone(),
                    automatic_errors: inner.automatic_errors.clone(),
                    deadline_changed: inner.deadline_changed.clone(),
                    stop: inner.stop.clone(),
                    closed_event_sent: inner.closed_event_sent.clone(),
                    request_timeout: server_request_timeout,
                    drained: request_drained,
                },
            )),
            tokio::spawn(expiration_pump(ExpirationPumpContext {
                client: inner.client.clone(),
                state: inner.state.clone(),
                events: inner.events.clone(),
                automatic_errors: inner.automatic_errors.clone(),
                deadline_changed: inner.deadline_changed.clone(),
                stop: inner.stop.clone(),
                closed_event_sent: inner.closed_event_sent.clone(),
            })),
            tokio::spawn(automatic_response_pump(
                automatic_error_rx,
                AutomaticResponseContext {
                    client: inner.client.clone(),
                    state: inner.state.clone(),
                    events: inner.events.clone(),
                    stop: inner.stop.clone(),
                    closed_event_sent: inner.closed_event_sent.clone(),
                    response_timeout: config.shutdown_timeout,
                },
            )),
            tokio::spawn(connection_monitor(ConnectionMonitorContext {
                client: inner.client.clone(),
                state: inner.state.clone(),
                events: inner.events.clone(),
                closed_event_sent: inner.closed_event_sent.clone(),
                stop: inner.stop.clone(),
                notification_drained: inner.notification_drained.clone(),
                request_drained: inner.request_drained.clone(),
                shutdown_timeout: inner.shutdown_timeout,
            })),
        ];
        *inner.tasks.lock().await = tasks;
        Ok(Self { inner })
    }

    pub fn initialize_response(&self) -> &InitializeResponse {
        &self.inner.client.initialize_response
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed_event_sent.load(Ordering::Acquire)
    }

    /// Subscribe to runtime events with a bounded replay window.
    ///
    /// Pending server requests are always included, even if their original event
    /// has aged out of the ordinary notification history.
    pub async fn subscribe_events(&self) -> RuntimeEventReceiver {
        let state = self.inner.state.lock().await;
        let live = self.inner.events.subscribe();
        let high_water = state.next_sequence.saturating_sub(1);
        let mut backlog: Vec<_> = state.history.iter().cloned().collect();
        backlog.retain(|event| match &event.event {
            CodexRuntimeEvent::ServerRequest { token, .. } => state.pending.contains_key(token),
            _ => true,
        });
        let mut sequences: HashSet<_> = backlog.iter().map(|event| event.sequence).collect();
        for pending in state.pending.values() {
            if sequences.insert(pending.event.sequence) {
                backlog.push(pending.event.clone());
            }
        }
        backlog.sort_unstable_by_key(|event| event.sequence);
        RuntimeEventReceiver {
            backlog: backlog.into(),
            live,
            high_water,
        }
    }

    pub async fn list_models(
        &self,
        params: ModelListParams,
    ) -> Result<ModelListResponse, CodexRuntimeError> {
        self.ensure_running()?;
        Ok(self.inner.client.list_models(params).await?)
    }

    pub async fn start_thread(
        &self,
        params: ThreadStartParams,
    ) -> Result<ThreadStartResponse, CodexRuntimeError> {
        self.ensure_running()?;
        Ok(self.inner.client.start_thread(params).await?)
    }

    pub async fn start_turn(
        &self,
        params: TurnStartParams,
    ) -> Result<TurnStartResponse, CodexRuntimeError> {
        self.ensure_running()?;
        Ok(self.inner.client.start_turn(params).await?)
    }

    pub async fn interrupt_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), CodexRuntimeError> {
        self.ensure_running()?;
        self.inner.client.interrupt_turn(thread_id, turn_id).await?;
        Ok(())
    }

    pub async fn respond_success(
        &self,
        token: &RuntimeRequestToken,
        result: Value,
    ) -> Result<(), CodexRuntimeError> {
        self.ensure_running()?;
        let pending = self.take_pending(token).await?;
        self.inner
            .client
            .respond_to_server_request(pending.protocol_id, result)
            .await?;
        publish_event(
            &self.inner.state,
            &self.inner.events,
            CodexRuntimeEvent::ServerRequestResolved {
                token: token.clone(),
                resolution: RuntimeRequestResolution::RespondedSuccess,
            },
        )
        .await;
        Ok(())
    }

    pub async fn respond_error(
        &self,
        token: &RuntimeRequestToken,
        code: i64,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Result<(), CodexRuntimeError> {
        self.ensure_running()?;
        let pending = self.take_pending(token).await?;
        self.inner
            .client
            .respond_to_server_error(pending.protocol_id, code, message, data)
            .await?;
        publish_event(
            &self.inner.state,
            &self.inner.events,
            CodexRuntimeEvent::ServerRequestResolved {
                token: token.clone(),
                resolution: RuntimeRequestResolution::RespondedError,
            },
        )
        .await;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), CodexRuntimeError> {
        let _shutdown_guard = self.inner.shutdown_lock.lock().await;
        if self.inner.shutdown_complete.is_cancelled() {
            return Ok(());
        }

        self.inner.shutdown_started.store(true, Ordering::Release);
        publish_event(
            &self.inner.state,
            &self.inner.events,
            CodexRuntimeEvent::ShuttingDown,
        )
        .await;
        self.inner.stop.cancel();
        self.inner.deadline_changed.notify_waiters();

        let pending = {
            let mut state = self.inner.state.lock().await;
            std::mem::take(&mut state.pending)
        };
        let fail_pending = async {
            for (token, pending) in pending {
                let _ = self
                    .inner
                    .client
                    .respond_to_server_error(
                        pending.protocol_id,
                        CLIENT_SHUTTING_DOWN,
                        "Codex client is shutting down",
                        None,
                    )
                    .await;
                publish_event(
                    &self.inner.state,
                    &self.inner.events,
                    CodexRuntimeEvent::ServerRequestResolved {
                        token,
                        resolution: RuntimeRequestResolution::ShuttingDown,
                    },
                )
                .await;
            }
        };
        let _ = timeout(self.inner.shutdown_timeout, fail_pending).await;
        let shutdown_result = self.inner.client.shutdown().await;
        self.stop_pumps().await;
        publish_closed_once(
            &self.inner.state,
            &self.inner.events,
            &self.inner.closed_event_sent,
        )
        .await;
        self.inner.shutdown_complete.cancel();
        shutdown_result.map_err(Into::into)
    }

    fn ensure_running(&self) -> Result<(), CodexRuntimeError> {
        if self.inner.stop.is_cancelled() || self.inner.shutdown_started.load(Ordering::Acquire) {
            Err(CodexRuntimeError::ShuttingDown)
        } else if self.inner.client.is_closed() {
            Err(CodexRuntimeError::AppServer(
                AppServerError::ConnectionClosed {
                    detail: String::new(),
                },
            ))
        } else {
            Ok(())
        }
    }

    async fn take_pending(
        &self,
        token: &RuntimeRequestToken,
    ) -> Result<PendingServerRequest, CodexRuntimeError> {
        let pending = self
            .inner
            .state
            .lock()
            .await
            .pending
            .remove(token)
            .ok_or(CodexRuntimeError::UnknownRequestToken)?;
        self.inner.deadline_changed.notify_one();
        Ok(pending)
    }

    async fn stop_pumps(&self) {
        let mut tasks = self.inner.tasks.lock().await;
        let joined = timeout(self.inner.shutdown_timeout, async {
            for task in tasks.iter_mut() {
                let _ = task.await;
            }
        })
        .await;
        if joined.is_err() {
            for task in tasks.iter() {
                task.abort();
            }
            for task in tasks.iter_mut() {
                let _ = task.await;
            }
        }
        tasks.clear();
    }
}

pub struct RuntimeEventReceiver {
    backlog: VecDeque<SequencedRuntimeEvent>,
    live: broadcast::Receiver<SequencedRuntimeEvent>,
    high_water: u64,
}

impl RuntimeEventReceiver {
    pub async fn recv(&mut self) -> Result<SequencedRuntimeEvent, CodexRuntimeError> {
        if let Some(event) = self.backlog.pop_front() {
            return Ok(event);
        }
        loop {
            match self.live.recv().await {
                Ok(event) if event.sequence <= self.high_water => continue,
                Ok(event) => return Ok(event),
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(CodexRuntimeError::EventStreamClosed);
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    return Err(CodexRuntimeError::EventStreamLagged(skipped));
                }
            }
        }
    }
}

struct RequestPumpContext {
    client: CodexAppServerClient,
    state: Arc<Mutex<RuntimeState>>,
    events: broadcast::Sender<SequencedRuntimeEvent>,
    automatic_errors: mpsc::Sender<AutomaticErrorResponse>,
    deadline_changed: Arc<Notify>,
    stop: CancellationToken,
    closed_event_sent: Arc<AtomicBool>,
    request_timeout: Duration,
    drained: CancellationToken,
}

struct ExpirationPumpContext {
    client: CodexAppServerClient,
    state: Arc<Mutex<RuntimeState>>,
    events: broadcast::Sender<SequencedRuntimeEvent>,
    automatic_errors: mpsc::Sender<AutomaticErrorResponse>,
    deadline_changed: Arc<Notify>,
    stop: CancellationToken,
    closed_event_sent: Arc<AtomicBool>,
}

struct AutomaticResponseContext {
    client: CodexAppServerClient,
    state: Arc<Mutex<RuntimeState>>,
    events: broadcast::Sender<SequencedRuntimeEvent>,
    stop: CancellationToken,
    closed_event_sent: Arc<AtomicBool>,
    response_timeout: Duration,
}

struct ConnectionMonitorContext {
    client: CodexAppServerClient,
    state: Arc<Mutex<RuntimeState>>,
    events: broadcast::Sender<SequencedRuntimeEvent>,
    closed_event_sent: Arc<AtomicBool>,
    stop: CancellationToken,
    notification_drained: CancellationToken,
    request_drained: CancellationToken,
    shutdown_timeout: Duration,
}

async fn notification_pump(
    mut receiver: broadcast::Receiver<ServerNotification>,
    state: Arc<Mutex<RuntimeState>>,
    events: broadcast::Sender<SequencedRuntimeEvent>,
    deadline_changed: Arc<Notify>,
    stop: CancellationToken,
    drained: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            message = receiver.recv() => match message {
                Ok(notification) => {
                    if notification.method == "serverRequest/resolved" {
                        clear_server_resolved_request(
                            &state,
                            &events,
                            &deadline_changed,
                            &notification.params,
                        ).await;
                    }
                    publish_event(
                        &state,
                        &events,
                        CodexRuntimeEvent::Notification {
                            method: notification.method,
                            params: notification.params,
                        },
                    ).await;
                },
                Err(broadcast::error::RecvError::Lagged(skipped)) => publish_event(
                    &state,
                    &events,
                    CodexRuntimeEvent::NotificationStreamLagged { skipped },
                ).await,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    drained.cancel();
}

async fn clear_server_resolved_request(
    state: &Mutex<RuntimeState>,
    events: &broadcast::Sender<SequencedRuntimeEvent>,
    deadline_changed: &Notify,
    params: &Value,
) {
    let Some(request_id) = params.get("requestId") else {
        return;
    };
    let mut state = state.lock().await;
    let token = state
        .pending
        .iter()
        .find_map(|(token, pending)| (pending.protocol_id == *request_id).then(|| token.clone()));
    let Some(token) = token else {
        return;
    };
    state.pending.remove(&token);
    let Some(event) = state.record(CodexRuntimeEvent::ServerRequestResolved {
        token,
        resolution: RuntimeRequestResolution::ClearedByServer,
    }) else {
        return;
    };
    let _ = events.send(event);
    drop(state);
    deadline_changed.notify_one();
}

async fn server_request_pump(
    mut receiver: broadcast::Receiver<ServerRequest>,
    context: RequestPumpContext,
) {
    loop {
        tokio::select! {
            biased;
            _ = context.stop.cancelled() => break,
            message = receiver.recv() => match message {
                Ok(request) => handle_server_request(request, &context).await,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    publish_event(
                        &context.state,
                        &context.events,
                        CodexRuntimeEvent::FatalProtocolError {
                            message: format!("server request stream lagged by {skipped}; closing fail-closed"),
                        },
                    ).await;
                    terminate_after_fatal(
                        &context.client,
                        &context.state,
                        &context.events,
                        &context.stop,
                        &context.closed_event_sent,
                    ).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    context.drained.cancel();
}

async fn handle_server_request(request: ServerRequest, context: &RequestPumpContext) {
    if !is_known_server_request(&request.method) {
        let method = request.method;
        let response = AutomaticErrorResponse {
            protocol_id: request.id,
            method: method.clone(),
            code: METHOD_NOT_FOUND,
            message: format!("Duet does not support server request method `{method}`"),
            token: None,
        };
        if context.automatic_errors.try_send(response).is_err() {
            publish_event(
                &context.state,
                &context.events,
                CodexRuntimeEvent::FatalProtocolError {
                    message: "automatic response queue was full; closing fail-closed".into(),
                },
            )
            .await;
            terminate_after_fatal(
                &context.client,
                &context.state,
                &context.events,
                &context.stop,
                &context.closed_event_sent,
            )
            .await;
        }
        return;
    }

    let deadline = Instant::now() + context.request_timeout;
    let mut state = context.state.lock().await;
    let token = loop {
        let candidate = RuntimeRequestToken(Uuid::new_v4().to_string());
        if !state.pending.contains_key(&candidate) {
            break candidate;
        }
    };
    let Some(event) = state.record(CodexRuntimeEvent::ServerRequest {
        token: token.clone(),
        method: request.method.clone(),
        params: request.params,
    }) else {
        return;
    };
    state.pending.insert(
        token,
        PendingServerRequest {
            protocol_id: request.id,
            method: request.method,
            deadline,
            event: event.clone(),
        },
    );
    let _ = context.events.send(event);
    drop(state);
    context.deadline_changed.notify_one();
}

async fn expiration_pump(context: ExpirationPumpContext) {
    loop {
        let next_deadline = {
            context
                .state
                .lock()
                .await
                .pending
                .values()
                .map(|pending| pending.deadline)
                .min()
        };

        match next_deadline {
            Some(deadline) => {
                tokio::select! {
                    biased;
                    _ = context.stop.cancelled() => break,
                    _ = context.deadline_changed.notified() => continue,
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            }
            None => {
                tokio::select! {
                    biased;
                    _ = context.stop.cancelled() => break,
                    _ = context.deadline_changed.notified() => continue,
                }
            }
        }

        let expired = {
            let now = Instant::now();
            let mut state = context.state.lock().await;
            let tokens: Vec<_> = state
                .pending
                .iter()
                .filter(|(_, pending)| pending.deadline <= now)
                .map(|(token, _)| token.clone())
                .collect();
            tokens
                .into_iter()
                .filter_map(|token| state.pending.remove(&token).map(|pending| (token, pending)))
                .collect::<Vec<_>>()
        };

        for (token, pending) in expired {
            let response = AutomaticErrorResponse {
                protocol_id: pending.protocol_id,
                method: pending.method,
                code: CLIENT_RESPONSE_TIMEOUT,
                message: "Duet did not receive a response before the request deadline".into(),
                token: Some((token, RuntimeRequestResolution::TimedOut)),
            };
            if context.automatic_errors.try_send(response).is_err() {
                publish_event(
                    &context.state,
                    &context.events,
                    CodexRuntimeEvent::FatalProtocolError {
                        message: "automatic response queue was full; closing fail-closed".into(),
                    },
                )
                .await;
                terminate_after_fatal(
                    &context.client,
                    &context.state,
                    &context.events,
                    &context.stop,
                    &context.closed_event_sent,
                )
                .await;
                break;
            }
        }
    }
}

async fn automatic_response_pump(
    mut responses: mpsc::Receiver<AutomaticErrorResponse>,
    context: AutomaticResponseContext,
) {
    loop {
        tokio::select! {
            biased;
            _ = context.stop.cancelled() => break,
            response = responses.recv() => {
                let Some(response) = response else { break; };
                let delivered = timeout(
                    context.response_timeout,
                    context.client.respond_to_server_error(
                        response.protocol_id,
                        response.code,
                        response.message.clone(),
                        None,
                    ),
                ).await.is_ok_and(|result| result.is_ok());
                if let Some((token, resolution)) = response.token {
                    publish_event(
                        &context.state,
                        &context.events,
                        CodexRuntimeEvent::ServerRequestResolved { token, resolution },
                    ).await;
                } else {
                    publish_event(
                        &context.state,
                        &context.events,
                        CodexRuntimeEvent::ServerRequestRejected {
                            method: response.method,
                            reason: response.message,
                        },
                    ).await;
                }
                if !delivered {
                    publish_event(
                        &context.state,
                        &context.events,
                        CodexRuntimeEvent::FatalProtocolError {
                            message: "automatic fail-closed response could not be delivered".into(),
                        },
                    ).await;
                    terminate_after_fatal(
                        &context.client,
                        &context.state,
                        &context.events,
                        &context.stop,
                        &context.closed_event_sent,
                    ).await;
                    break;
                }
            }
        }
    }
}

async fn connection_monitor(context: ConnectionMonitorContext) {
    loop {
        tokio::select! {
            biased;
            _ = context.stop.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if context.client.is_closed() {
                    let _ = timeout(context.shutdown_timeout, async {
                        tokio::join!(
                            context.notification_drained.cancelled(),
                            context.request_drained.cancelled()
                        );
                    }).await;
                    context.stop.cancel();
                    let _ = context.client.shutdown().await;
                    resolve_pending_for_shutdown(&context.state, &context.events).await;
                    publish_closed_once(
                        &context.state,
                        &context.events,
                        &context.closed_event_sent,
                    ).await;
                    break;
                }
            }
        }
    }
}

async fn terminate_after_fatal(
    client: &CodexAppServerClient,
    state: &Mutex<RuntimeState>,
    events: &broadcast::Sender<SequencedRuntimeEvent>,
    stop: &CancellationToken,
    closed_event_sent: &AtomicBool,
) {
    stop.cancel();
    let _ = client.shutdown().await;
    resolve_pending_for_shutdown(state, events).await;
    publish_closed_once(state, events, closed_event_sent).await;
}

async fn resolve_pending_for_shutdown(
    state: &Mutex<RuntimeState>,
    events: &broadcast::Sender<SequencedRuntimeEvent>,
) {
    let pending = {
        let mut state = state.lock().await;
        std::mem::take(&mut state.pending)
    };
    for (token, _) in pending {
        publish_event(
            state,
            events,
            CodexRuntimeEvent::ServerRequestResolved {
                token,
                resolution: RuntimeRequestResolution::ShuttingDown,
            },
        )
        .await;
    }
}

async fn publish_event(
    state: &Mutex<RuntimeState>,
    events: &broadcast::Sender<SequencedRuntimeEvent>,
    event: CodexRuntimeEvent,
) {
    let mut state = state.lock().await;
    let Some(event) = state.record(event) else {
        return;
    };
    // Sending while holding the state lock makes subscribe_events atomic with
    // respect to its replay snapshot and live subscription.
    let _ = events.send(event);
}

async fn publish_closed_once(
    state: &Mutex<RuntimeState>,
    events: &broadcast::Sender<SequencedRuntimeEvent>,
    closed_event_sent: &AtomicBool,
) {
    let mut state = state.lock().await;
    if let Some(event) = state.close() {
        let _ = events.send(event);
        // Replacement is allowed only after subscribers can observe the
        // terminal envelope. The state lock serializes concurrent finalizers.
        closed_event_sent.store(true, Ordering::Release);
    }
}

fn is_known_server_request(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/tool/requestUserInput"
            | "mcpServer/elicitation/request"
            | "item/permissions/requestApproval"
            | "item/tool/call"
            | "account/chatgptAuthTokens/refresh"
            | "attestation/generate"
            | "applyPatchApproval"
            | "execCommandApproval"
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::codex_app_server::ClientInfo;
    use std::{fs, os::unix::fs::PermissionsExt, process::Stdio};
    use tempfile::TempDir;

    struct FakeRuntime {
        runtime: CodexRuntime,
        _directory: TempDir,
    }

    async fn fake_runtime(script_body: &str, request_timeout: Duration) -> FakeRuntime {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("fake-codex");
        fs::write(&executable, format!("#!/bin/sh\n{script_body}")).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();

        let mut app_server = AppServerConfig::new(executable, ClientInfo::duet("test"));
        app_server.request_timeout = Duration::from_secs(2);
        app_server.shutdown_grace = Duration::from_millis(20);
        let mut config = CodexRuntimeConfig::new(app_server);
        config.server_request_timeout = request_timeout;
        config.shutdown_timeout = Duration::from_secs(1);
        config.event_history_capacity = 64;
        config.automatic_response_capacity = 16;
        let runtime = CodexRuntime::spawn(config).await.unwrap();
        FakeRuntime {
            runtime,
            _directory: directory,
        }
    }

    async fn next_matching(
        receiver: &mut RuntimeEventReceiver,
        predicate: impl Fn(&CodexRuntimeEvent) -> bool,
    ) -> SequencedRuntimeEvent {
        timeout(Duration::from_secs(2), async {
            loop {
                let event = receiver.recv().await.unwrap();
                if predicate(&event.event) {
                    return event;
                }
            }
        })
        .await
        .unwrap()
    }

    const HANDSHAKE: &str = r#"
read_line() { IFS= read -r line || exit 90; }
read_line
case "$line" in *'"method":"initialize"'*) ;; *) exit 91;; esac
printf '%s\n' '{"id":1,"result":{"userAgent":"fake/1","platformFamily":"unix","platformOs":"test"}}'
read_line
case "$line" in *'"method":"initialized"'*) ;; *) exit 92;; esac
"#;

    #[tokio::test]
    async fn replays_an_early_notification_to_a_late_subscriber() {
        let script = format!(
            "{HANDSHAKE}{}",
            r#"
read_line
printf '%s\n' '{"method":"item/agentMessage/delta","params":{"delta":"early"}}'
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr_early","ephemeral":false}}}'
while read_line; do :; done
"#
        );
        let fake = fake_runtime(&script, Duration::from_secs(1)).await;
        let thread = fake
            .runtime
            .start_thread(ThreadStartParams::default())
            .await
            .unwrap();
        assert_eq!(thread.thread.id, "thr_early");

        let mut events = fake.runtime.subscribe_events().await;
        let event = next_matching(&mut events, |event| {
            matches!(event, CodexRuntimeEvent::Notification { method, .. } if method == "item/agentMessage/delta")
        })
        .await;
        match event.event {
            CodexRuntimeEvent::Notification { params, .. } => {
                assert_eq!(params["delta"], "early");
            }
            _ => unreachable!(),
        }
        fake.runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retains_an_arbitrary_request_id_behind_an_opaque_token() {
        let script = format!(
            "{HANDSHAKE}{}",
            r#"
read_line
printf '%s\n' '{"id":"approval/string/77","method":"item/commandExecution/requestApproval","params":{"command":"cargo test"}}'
read_line
case "$line" in *'"id":"approval/string/77"'*'"decision":"decline"'*) ;; *) exit 93;; esac
printf '%s\n' '{"id":78,"method":"item/tool/requestUserInput","params":{"questions":[]}}'
read_line
case "$line" in *'"code":-42'*'"reason":"declined"'*'"id":78'*) ;; *) exit 94;; esac
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr_known","ephemeral":false}}}'
while read_line; do :; done
"#
        );
        let fake = fake_runtime(&script, Duration::from_secs(1)).await;
        let mut events = fake.runtime.subscribe_events().await;
        let runtime = fake.runtime.clone();
        let start =
            tokio::spawn(async move { runtime.start_thread(ThreadStartParams::default()).await });
        let request = next_matching(&mut events, |event| {
            matches!(event, CodexRuntimeEvent::ServerRequest { .. })
        })
        .await;
        let token = match request.event {
            CodexRuntimeEvent::ServerRequest { token, method, .. } => {
                assert_eq!(method, "item/commandExecution/requestApproval");
                assert!(!token.as_str().contains("approval/string/77"));
                token
            }
            _ => unreachable!(),
        };
        fake.runtime
            .respond_success(&token, serde_json::json!({"decision":"decline"}))
            .await
            .unwrap();
        let request = next_matching(&mut events, |event| {
            matches!(event, CodexRuntimeEvent::ServerRequest { method, .. } if method == "item/tool/requestUserInput")
        })
        .await;
        let token = match request.event {
            CodexRuntimeEvent::ServerRequest { token, .. } => token,
            _ => unreachable!(),
        };
        fake.runtime
            .respond_error(
                &token,
                -42,
                "user declined",
                Some(serde_json::json!({"reason":"declined"})),
            )
            .await
            .unwrap();
        assert_eq!(start.await.unwrap().unwrap().thread.id, "thr_known");
        fake.runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn no_subscriber_causes_a_bounded_fail_closed_response() {
        let script = format!(
            "{HANDSHAKE}{}",
            r#"
read_line
printf '%s\n' '{"id":77,"method":"item/fileChange/requestApproval","params":{"reason":"test"}}'
read_line
case "$line" in *'"code":-32001'*'"id":77'*) ;; *) exit 94;; esac
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr_timeout","ephemeral":false}}}'
while read_line; do :; done
"#
        );
        let fake = fake_runtime(&script, Duration::from_millis(40)).await;
        let thread = timeout(
            Duration::from_secs(1),
            fake.runtime.start_thread(ThreadStartParams::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(thread.thread.id, "thr_timeout");

        let mut events = fake.runtime.subscribe_events().await;
        next_matching(&mut events, |event| {
            matches!(
                event,
                CodexRuntimeEvent::ServerRequestResolved {
                    resolution: RuntimeRequestResolution::TimedOut,
                    ..
                }
            )
        })
        .await;
        fake.runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn server_resolution_clears_a_pending_request_without_a_late_response() {
        let script = format!(
            "{HANDSHAKE}{}",
            r#"
read_line
printf '%s\n' '{"id":"approval-1","method":"item/fileChange/requestApproval","params":{"threadId":"thr_resolved"}}'
printf '%s\n' '{"method":"serverRequest/resolved","params":{"threadId":"thr_resolved","requestId":"approval-1"}}'
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr_resolved","ephemeral":false}}}'
while read_line; do :; done
"#
        );
        let fake = fake_runtime(&script, Duration::from_millis(80)).await;
        let thread = fake
            .runtime
            .start_thread(ThreadStartParams::default())
            .await
            .unwrap();
        assert_eq!(thread.thread.id, "thr_resolved");
        let mut events = fake.runtime.subscribe_events().await;
        let resolved = events.recv().await.unwrap();
        assert!(matches!(
            &resolved.event,
            CodexRuntimeEvent::ServerRequestResolved {
                resolution: RuntimeRequestResolution::ClearedByServer,
                ..
            }
        ));
        let token = match resolved.event {
            CodexRuntimeEvent::ServerRequestResolved { token, .. } => token,
            _ => unreachable!(),
        };
        assert!(matches!(
            fake.runtime.respond_success(&token, Value::Null).await,
            Err(CodexRuntimeError::UnknownRequestToken)
        ));
        tokio::time::sleep(Duration::from_millis(120)).await;
        fake.runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_request_methods_fail_closed_immediately() {
        let script = format!(
            "{HANDSHAKE}{}",
            r#"
read_line
printf '%s\n' '{"id":{"future":1},"method":"future/unsafeRequest","params":{}}'
read_line
case "$line" in *'"code":-32601'*'"future":1'*) ;; *) exit 95;; esac
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr_unknown","ephemeral":false}}}'
while read_line; do :; done
"#
        );
        let fake = fake_runtime(&script, Duration::from_secs(1)).await;
        let thread = timeout(
            Duration::from_secs(1),
            fake.runtime.start_thread(ThreadStartParams::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(thread.thread.id, "thr_unknown");
        fake.runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_stops_pumps_and_terminates_the_child() {
        let script = format!(
            "{}{}",
            r#"
read_line() { IFS= read -r line || exit 90; }
read_line
printf '{"id":1,"result":{"userAgent":"fake/1","platformOs":"%s"}}\n' "$$"
read_line
printf '%s\n' '{"id":"pending-close","method":"item/fileChange/requestApproval","params":{}}'
"#,
            "while :; do sleep 30; done\n"
        );
        let fake = fake_runtime(&script, Duration::from_secs(1)).await;
        let pid = fake
            .runtime
            .initialize_response()
            .platform_os
            .as_deref()
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let mut events = fake.runtime.subscribe_events().await;
        next_matching(&mut events, |event| {
            matches!(event, CodexRuntimeEvent::ServerRequest { .. })
        })
        .await;
        fake.runtime.shutdown().await.unwrap();
        assert!(fake.runtime.is_closed());
        let mut resolved_before_close = false;
        loop {
            let event = events.recv().await.unwrap();
            match event.event {
                CodexRuntimeEvent::ServerRequestResolved {
                    resolution: RuntimeRequestResolution::ShuttingDown,
                    ..
                } => resolved_before_close = true,
                CodexRuntimeEvent::Closed => break,
                _ => {}
            }
        }
        assert!(resolved_before_close);
        assert!(timeout(Duration::from_millis(20), events.recv())
            .await
            .is_err());

        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!alive, "fake app-server process {pid} survived shutdown");
    }

    #[tokio::test]
    async fn natural_exit_drains_terminal_notifications_before_closed() {
        let script = format!(
            "{HANDSHAKE}{}",
            r#"
read_line
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thr_exit","ephemeral":false}}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thr_exit","turn":{"id":"turn_exit","status":"completed"}}}'
exit 0
"#
        );
        let fake = fake_runtime(&script, Duration::from_secs(1)).await;
        let mut events = fake.runtime.subscribe_events().await;
        let thread = fake
            .runtime
            .start_thread(ThreadStartParams::default())
            .await
            .unwrap();
        assert_eq!(thread.thread.id, "thr_exit");

        let mut saw_completion = false;
        loop {
            let event = timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            match event.event {
                CodexRuntimeEvent::Notification { method, .. } if method == "turn/completed" => {
                    saw_completion = true;
                }
                CodexRuntimeEvent::Closed => break,
                _ => {}
            }
        }
        assert!(saw_completion, "turn completion must precede Closed");
        assert!(fake.runtime.is_closed());
        assert!(timeout(Duration::from_millis(20), events.recv())
            .await
            .is_err());
    }
}
