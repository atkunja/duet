use crate::{
    codex_app_server::{
        AppServerConfig, AppServerError, ClientInfo, ModelInfo, ModelListParams, ThreadStartParams,
        TurnStartParams,
    },
    codex_runtime::{
        CodexRuntime, CodexRuntimeConfig, CodexRuntimeError, CodexRuntimeEvent,
        SequencedRuntimeEvent,
    },
    git,
    models::{
        AppPreferences, DoctorReport, Project, RepoInspection, RunDetail, RunEvent, RunSummary,
        StartRunRequest, ToolStatus,
    },
    process::{run_process, OutputCallback, ProcessRequest},
    tooling::{path_for_program, resolve_binary},
    verification::{self, VerificationItem},
    workflow::{self, WorkflowContext},
    AppState, CodexThreadOwner, ManagedCodexRuntime,
};
use chrono::Utc;
use serde::Serialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type CommandResult<T> = Result<T, String>;
const CODEX_AUTH_OPERATION: &str = "auth:codex";

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn codex_runtime(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> CommandResult<ManagedCodexRuntime> {
    if state.active_runs.lock().contains_key(CODEX_AUTH_OPERATION) {
        return Err("Codex sign-in is in progress".into());
    }
    let mut server = state.codex_server.lock().await;
    if state.active_runs.lock().contains_key(CODEX_AUTH_OPERATION) {
        return Err("Codex sign-in is in progress".into());
    }
    retire_closed_codex_runtime(app, state, &mut server);
    if let Some(managed) = server.as_ref() {
        return Ok(managed.clone());
    }
    let binary = resolve_binary("codex").ok_or_else(|| "Codex CLI was not found".to_string())?;
    let runtime = CodexRuntime::spawn(CodexRuntimeConfig::new(AppServerConfig::new(
        binary,
        ClientInfo::duet(env!("CARGO_PKG_VERSION")),
    )))
    .await
    .map_err(err)?;
    let generation = {
        let mut current = state.codex_generation.lock();
        *current = current.saturating_add(1);
        *current
    };
    let managed = ManagedCodexRuntime {
        runtime: runtime.clone(),
        generation,
    };
    let event_app = app.clone();
    let event_runtime = runtime.clone();
    let thread_owners = state.codex_threads.clone();
    let active_generation = state.codex_generation.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_sequence = 0;
        loop {
            let mut events = event_runtime.subscribe_events().await;
            loop {
                match events.recv().await {
                    Ok(event) if event.sequence <= last_sequence => continue,
                    Ok(event) => {
                        if *active_generation.lock() != generation {
                            return;
                        }
                        if sequence_has_gap(last_sequence, event.sequence) {
                            clear_codex_thread_generation(&thread_owners, generation);
                            {
                                let current = active_generation.lock();
                                if *current != generation {
                                    return;
                                }
                                let _ = event_app.emit(
                                    "duet://codex-event",
                                    SequencedRuntimeEvent {
                                        sequence: event.sequence,
                                        event: CodexRuntimeEvent::FatalProtocolError {
                                            message: "Codex event history was incomplete; the session was reset to avoid showing stale state".into(),
                                        },
                                    },
                                );
                            }
                            last_sequence = event.sequence;
                            let _ = event_runtime.shutdown().await;
                            continue;
                        }
                        last_sequence = event.sequence;
                        match &event.event {
                            CodexRuntimeEvent::ServerRequest { token, .. } => {
                                match event_runtime
                                    .respond_error(
                                        token,
                                        -32_002,
                                        "Duet's read-only assistant declined this request",
                                        None,
                                    )
                                    .await
                                {
                                    Ok(()) | Err(CodexRuntimeError::UnknownRequestToken) => {}
                                    Err(error) => {
                                        clear_codex_thread_generation(&thread_owners, generation);
                                        {
                                            let current = active_generation.lock();
                                            if *current != generation {
                                                return;
                                            }
                                            let _ = event_app.emit(
                                                "duet://codex-event",
                                                SequencedRuntimeEvent {
                                                    sequence: event.sequence,
                                                    event: CodexRuntimeEvent::FatalProtocolError {
                                                        message: format!(
                                                            "Codex request rejection failed: {error}"
                                                        ),
                                                    },
                                                },
                                            );
                                        }
                                        let _ = event_runtime.shutdown().await;
                                    }
                                }
                            }
                            CodexRuntimeEvent::Closed => {
                                let current = active_generation.lock();
                                if *current != generation {
                                    return;
                                }
                                clear_codex_thread_generation(&thread_owners, generation);
                                let _ = event_app.emit("duet://codex-event", event);
                                return;
                            }
                            _ => {
                                let current = active_generation.lock();
                                if *current != generation {
                                    return;
                                }
                                let _ = event_app.emit("duet://codex-event", event);
                            }
                        }
                    }
                    Err(CodexRuntimeError::EventStreamLagged(_)) => break,
                    Err(CodexRuntimeError::EventStreamClosed) => return,
                    Err(_) => return,
                }
            }
        }
    });
    *server = Some(managed.clone());
    Ok(managed)
}

fn retire_closed_codex_runtime(
    app: &AppHandle,
    state: &State<'_, AppState>,
    server: &mut Option<ManagedCodexRuntime>,
) {
    if !server
        .as_ref()
        .is_some_and(|managed| managed.runtime.is_closed())
    {
        return;
    }
    *server = None;
    state.codex_threads.lock().clear();
    // Runtime forwarding is asynchronous. Emit the reset synchronously before
    // a replacement can be returned so a mounted panel cannot retain an old ID.
    let _ = app.emit(
        "duet://codex-event",
        SequencedRuntimeEvent {
            sequence: 0,
            event: CodexRuntimeEvent::Closed,
        },
    );
}

fn clear_codex_thread_generation(
    thread_owners: &parking_lot::Mutex<std::collections::HashMap<String, CodexThreadOwner>>,
    generation: u64,
) {
    thread_owners
        .lock()
        .retain(|_, owner| owner.generation != generation);
}

fn sequence_has_gap(previous: u64, next: u64) -> bool {
    previous != 0 && next > previous.saturating_add(1)
}

#[tauri::command]
pub async fn list_codex_models(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Vec<ModelInfo>> {
    let client = codex_runtime(&app, &state).await?;
    match collect_codex_models(&client.runtime).await {
        Ok(models) => Ok(models),
        Err(CodexRuntimeError::AppServer(AppServerError::ConnectionClosed { .. })) => {
            let mut server = state.codex_server.lock().await;
            retire_closed_codex_runtime(&app, &state, &mut server);
            drop(server);
            let retry = codex_runtime(&app, &state).await?;
            collect_codex_models(&retry.runtime).await.map_err(err)
        }
        Err(error) => Err(err(error)),
    }
}

async fn collect_codex_models(client: &CodexRuntime) -> Result<Vec<ModelInfo>, CodexRuntimeError> {
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_models = HashSet::new();
    let mut models = Vec::new();
    loop {
        let response = client
            .list_models(ModelListParams {
                cursor: cursor.clone(),
                ..ModelListParams::default()
            })
            .await?;
        models.extend(
            response
                .data
                .into_iter()
                .filter(|model| !model.hidden && seen_models.insert(model.id.clone())),
        );
        let Some(next_cursor) = response.next_cursor else {
            break;
        };
        if !seen_cursors.insert(next_cursor.clone()) || seen_cursors.len() > 64 {
            return Err(CodexRuntimeError::AppServer(AppServerError::Protocol(
                "model/list returned an invalid cursor sequence".into(),
            )));
        }
        cursor = Some(next_cursor);
    }
    Ok(models)
}

#[tauri::command]
pub async fn start_codex_thread(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    model: String,
) -> CommandResult<crate::codex_app_server::ThreadInfo> {
    let project = state.db.get_project(&project_id).map_err(err)?;
    let cwd = canonical_path(&project.path)?;
    let managed = codex_runtime(&app, &state).await?;
    let response = managed
        .runtime
        .start_thread(ThreadStartParams {
            model: Some(model),
            cwd: Some(cwd.clone()),
            approval_policy: Some("never".into()),
            sandbox: Some("read-only".into()),
            personality: Some("friendly".into()),
            service_name: Some("duet_desktop".into()),
            ephemeral: true,
            ..ThreadStartParams::default()
        })
        .await
        .map_err(err)?;
    if managed.generation != *state.codex_generation.lock() || managed.runtime.is_closed() {
        return Err("Codex restarted before the thread was ready; retry the message".into());
    }
    state.codex_threads.lock().insert(
        response.thread.id.clone(),
        CodexThreadOwner {
            project_id,
            cwd,
            generation: managed.generation,
        },
    );
    Ok(response.thread)
}

#[tauri::command]
pub async fn start_codex_turn(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    thread_id: String,
    prompt: String,
    model: String,
    effort: String,
) -> CommandResult<crate::codex_app_server::TurnInfo> {
    if prompt.trim().is_empty() {
        return Err("enter a message for Codex".into());
    }
    let project = state.db.get_project(&project_id).map_err(err)?;
    let project_cwd = canonical_path(&project.path)?;
    let owner = state
        .codex_threads
        .lock()
        .get(&thread_id)
        .cloned()
        .ok_or_else(|| "Codex thread is not owned by this Duet session".to_string())?;
    if owner.project_id != project_id
        || owner.cwd != project_cwd
        || owner.generation != *state.codex_generation.lock()
    {
        return Err("Codex thread does not belong to this project".into());
    }
    let mut params = TurnStartParams::text(thread_id, prompt.trim());
    params.cwd = Some(owner.cwd);
    params.approval_policy = Some("never".into());
    params.sandbox_policy = Some(serde_json::json!({
        "type": "readOnly",
        "networkAccess": false
    }));
    params.model = Some(model);
    params.effort = Some(effort);
    let managed = codex_runtime(&app, &state).await?;
    ensure_codex_generation(owner.generation, managed.generation)?;
    managed
        .runtime
        .start_turn(params)
        .await
        .map(|response| response.turn)
        .map_err(err)
}

#[tauri::command]
pub async fn interrupt_codex_turn(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    thread_id: String,
    turn_id: String,
) -> CommandResult<()> {
    let owner = state
        .codex_threads
        .lock()
        .get(&thread_id)
        .cloned()
        .ok_or_else(|| "Codex thread is not owned by this Duet session".to_string())?;
    if owner.project_id != project_id || owner.generation != *state.codex_generation.lock() {
        return Err("Codex thread does not belong to this project".into());
    }
    let managed = codex_runtime(&app, &state).await?;
    ensure_codex_generation(owner.generation, managed.generation)?;
    managed
        .runtime
        .interrupt_turn(&thread_id, &turn_id)
        .await
        .map_err(err)
}

fn ensure_codex_generation(owner_generation: u64, runtime_generation: u64) -> CommandResult<()> {
    if owner_generation == runtime_generation {
        Ok(())
    } else {
        Err("Codex restarted; start a new repository thread and retry".into())
    }
}

fn canonical_path(path: &str) -> CommandResult<String> {
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| format!("could not resolve project path: {error}"))
}

#[tauri::command]
pub async fn inspect_repository(path: String) -> CommandResult<RepoInspection> {
    git::inspect_repository(Path::new(&path)).await.map_err(err)
}

#[tauri::command]
pub async fn add_project(state: State<'_, AppState>, path: String) -> CommandResult<Project> {
    let inspection = git::inspect_repository(Path::new(&path))
        .await
        .map_err(err)?;
    let name = Path::new(&inspection.path)
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("Repository")
        .to_string();
    let project = Project {
        id: Uuid::new_v4().to_string(),
        name,
        path: inspection.path,
        language: inspection.language,
        build_system: inspection.build_system,
        test_command: inspection.suggested_test_command,
        benchmark_command: String::new(),
        last_used_at: Utc::now().to_rfc3339(),
    };
    state.db.upsert_project(&project).map_err(err)?;
    state
        .db
        .list_projects()
        .map_err(err)?
        .into_iter()
        .find(|p| p.path == project.path)
        .ok_or_else(|| "could not load saved project".into())
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> CommandResult<Vec<Project>> {
    state.db.list_projects().map_err(err)
}

#[tauri::command]
pub fn remove_project(state: State<'_, AppState>, project_id: String) -> CommandResult<()> {
    state.db.remove_project(&project_id).map_err(err)
}

#[tauri::command]
pub async fn start_run(
    app: AppHandle,
    state: State<'_, AppState>,
    request: StartRunRequest,
) -> CommandResult<String> {
    if request.task.trim().is_empty() {
        return Err("Describe what you want Duet to build".into());
    }
    if request.test_command.trim().is_empty() {
        return Err("Configure a required test or build command before starting Duet".into());
    }
    if !(1..=5).contains(&request.max_repairs) {
        return Err("Repair rounds must be between 1 and 5".into());
    }
    if !matches!(request.agent_mode.as_str(), "duet" | "codex" | "claude") {
        return Err("Choose Duet, Codex, or Claude as the agent mode".into());
    }
    if request.execution_location != "local" {
        return Err("Cloud execution is not available yet; choose Local".into());
    }
    let project = state.db.get_project(&request.project_id).map_err(err)?;
    let inspection = git::inspect_repository(Path::new(&project.path))
        .await
        .map_err(err)?;
    if inspection.dirty {
        return Err("The selected repository has uncommitted changes. Commit or stash them before starting Duet so the isolated base and later apply operation are unambiguous.".into());
    }
    let run_id = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    {
        let mut active = state.active_runs.lock();
        if active.contains_key(CODEX_AUTH_OPERATION) {
            return Err("Wait for Codex sign-in to finish or cancel it in Settings".into());
        }
        active.insert(run_id.clone(), cancel.clone());
    }
    if let Err(error) = state.db.create_run(
        &run_id,
        &request.project_id,
        request.task.trim(),
        &inspection.head_sha,
    ) {
        state.active_runs.lock().remove(&run_id);
        return Err(err(error));
    }
    let ctx = WorkflowContext {
        app: app.clone(),
        db: state.db.clone(),
        worktrees_root: state.worktrees_root.clone(),
        run_id: run_id.clone(),
        request,
        cancel: cancel.clone(),
    };
    let active = state.active_runs.clone();
    let db = state.db.clone();
    let task_id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = workflow::execute(ctx).await {
            if cancel.is_cancelled() {
                let _ = db.complete_run(&task_id, "cancelled", Some("cancelled by user"));
                let _ = app.emit(
                    "duet://run-event",
                    RunEvent::RunCancelled {
                        run_id: task_id.clone(),
                    },
                );
            } else {
                let reason = error.to_string();
                let _ = db.complete_run(&task_id, "failed", Some(&reason));
                let _ = app.emit(
                    "duet://run-event",
                    RunEvent::RunFailed {
                        run_id: task_id.clone(),
                        reason,
                    },
                );
            }
        }
        active.lock().remove(&task_id);
    });
    Ok(run_id)
}

#[tauri::command]
pub fn cancel_run(state: State<'_, AppState>, run_id: String) -> CommandResult<()> {
    let token = state
        .active_runs
        .lock()
        .get(&run_id)
        .cloned()
        .ok_or_else(|| "run is not active".to_string())?;
    token.cancel();
    Ok(())
}

#[tauri::command]
pub fn list_runs(state: State<'_, AppState>) -> CommandResult<Vec<RunSummary>> {
    state.db.list_runs().map_err(err)
}

#[tauri::command]
pub fn get_run(state: State<'_, AppState>, run_id: String) -> CommandResult<RunDetail> {
    state.db.get_run(&run_id).map_err(err)
}

#[tauri::command]
pub async fn run_project_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    command: String,
    operation_id: String,
) -> CommandResult<crate::models::VerificationResult> {
    let command = command.trim().to_string();
    if command.is_empty() {
        return Err("enter a command to run".into());
    }
    let project = state.db.get_project(&project_id).map_err(err)?;
    let operation_key = repository_operation_key(&project.path)?;
    begin_operation(&state, &operation_key)?;
    let token = CancellationToken::new();
    let event_operation_id = operation_id.clone();
    let operation_id = format!("console:{operation_id}");
    {
        let mut active = state.active_runs.lock();
        if active.contains_key(CODEX_AUTH_OPERATION) {
            state.run_operations.lock().remove(&operation_key);
            return Err("Wait for Codex sign-in to finish or cancel it in Settings".into());
        }
        active.insert(operation_id.clone(), token.clone());
    }
    let result = verification::execute(
        VerificationItem {
            name: "Command console".into(),
            command,
            timeout: Duration::from_secs(10 * 60),
            required: false,
        },
        Path::new(&project.path),
        token,
        Arc::new(move |stream: &str, chunk: &str| {
            let _ = app.emit(
                "duet://console-output",
                ConsoleOutputEvent {
                    operation_id: event_operation_id.clone(),
                    stream: stream.to_string(),
                    chunk: chunk.to_string(),
                },
            );
        }) as OutputCallback,
    )
    .await
    .map_err(err);
    state.active_runs.lock().remove(&operation_id);
    state.run_operations.lock().remove(&operation_key);
    result
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsoleOutputEvent {
    operation_id: String,
    stream: String,
    chunk: String,
}

#[tauri::command]
pub fn cancel_project_command(
    state: State<'_, AppState>,
    operation_id: String,
) -> CommandResult<()> {
    let key = format!("console:{operation_id}");
    let token = state
        .active_runs
        .lock()
        .get(&key)
        .cloned()
        .ok_or_else(|| "command is not running".to_string())?;
    token.cancel();
    Ok(())
}

#[tauri::command]
pub fn open_local_preview(app: AppHandle, url: String) -> CommandResult<()> {
    let parsed = tauri::Url::parse(&url).map_err(err)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
    {
        return Err("preview URLs must use localhost, 127.0.0.1, or ::1".into());
    }
    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(err)
}

#[tauri::command]
pub async fn get_diff(state: State<'_, AppState>, run_id: String) -> CommandResult<String> {
    let info = state.db.apply_info(&run_id).map_err(err)?;
    let path = info
        .worktree_path
        .ok_or_else(|| "run has no worktree".to_string())?;
    let diff = git::diff(Path::new(&path), &info.base_sha)
        .await
        .map_err(err)?;
    const LIMIT: usize = 5_000_000;
    if diff.len() > LIMIT {
        let end = diff.floor_char_boundary(LIMIT);
        Ok(format!(
            "{}\n[Duet diff preview truncated at 5 MB]\n",
            &diff[..end]
        ))
    } else {
        Ok(diff)
    }
}

#[tauri::command]
pub fn reveal_run(app: AppHandle, state: State<'_, AppState>, run_id: String) -> CommandResult<()> {
    let info = state.db.apply_info(&run_id).map_err(err)?;
    let stored = info
        .worktree_path
        .ok_or_else(|| "run has no worktree to reveal".to_string())?;
    let path = validated_reveal_path(&state.worktrees_root, &run_id, Path::new(&stored))?;
    app.opener().reveal_item_in_dir(path).map_err(err)
}

fn validated_reveal_path(root: &Path, run_id: &str, stored: &Path) -> CommandResult<PathBuf> {
    let expected = root.join(run_id).join("implementation");
    if stored != expected {
        return Err("refusing to reveal a path outside this run's managed worktree".into());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|_| "Duet's managed worktree directory is unavailable".to_string())?;
    let canonical_path = stored
        .canonicalize()
        .map_err(|_| "this run's worktree no longer exists".to_string())?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("refusing to reveal a path outside Duet's managed worktrees".into());
    }
    Ok(canonical_path)
}

#[tauri::command]
pub async fn apply_changes(state: State<'_, AppState>, run_id: String) -> CommandResult<()> {
    if state.active_runs.lock().contains_key(&run_id) {
        return Err("cannot apply changes while the run is active".into());
    }
    let info = state.db.apply_info(&run_id).map_err(err)?;
    let operation_key = repository_operation_key(&info.repo_path)?;
    begin_operation(&state, &operation_key)?;
    let result = async {
        if info.status != "completed" {
            return Err("only a completed, verified run can be applied".into());
        }
        if info.applied_at.is_some() {
            return Err("this run was already applied".into());
        }
        let worktree = info
            .worktree_path
            .ok_or_else(|| "run has no worktree".to_string())?;
        let verified_patch_sha256 = info.verified_patch_sha256.ok_or_else(|| {
            "this run has no recorded verified patch; apply is unavailable".to_string()
        })?;
        git::apply_worktree_changes(
            Path::new(&info.repo_path),
            Path::new(&worktree),
            &info.base_sha,
            &verified_patch_sha256,
        )
        .await
        .map_err(err)?;
        state.db.mark_applied(&run_id).map_err(|error| {
            format!("changes were applied, but Duet could not persist the applied state: {error}")
        })
    }
    .await;
    state.run_operations.lock().remove(&operation_key);
    result
}

#[tauri::command]
pub async fn discard_run(state: State<'_, AppState>, run_id: String) -> CommandResult<()> {
    if state.active_runs.lock().contains_key(&run_id) {
        return Err("stop the active run before discarding it".into());
    }
    let info = state.db.apply_info(&run_id).map_err(err)?;
    let operation_key = repository_operation_key(&info.repo_path)?;
    begin_operation(&state, &operation_key)?;
    let result = discard_inner(&state, &run_id, info).await;
    state.run_operations.lock().remove(&operation_key);
    result
}

fn begin_operation(state: &State<'_, AppState>, key: &str) -> CommandResult<()> {
    if !state.run_operations.lock().insert(key.into()) {
        return Err("another operation is already changing this repository".into());
    }
    Ok(())
}
fn repository_operation_key(repo: &str) -> CommandResult<String> {
    Ok(Path::new(repo)
        .canonicalize()
        .map_err(err)?
        .to_string_lossy()
        .into())
}
async fn discard_inner(
    state: &State<'_, AppState>,
    run_id: &str,
    info: crate::db::RunApplyInfo,
) -> CommandResult<()> {
    let worktree = info
        .worktree_path
        .ok_or_else(|| "run is already discarded or has no worktree".to_string())?;
    let path = PathBuf::from(&worktree);
    let expected_path = state.worktrees_root.join(run_id).join("implementation");
    let exact_match = if path.exists() {
        path.canonicalize().map_err(err)? == expected_path.canonicalize().map_err(err)?
    } else {
        path == expected_path
    };
    if !exact_match {
        return Err(
            "refusing to remove a worktree that does not exactly match this Duet run".into(),
        );
    }
    if path.exists() {
        let output = run_git_operation(
            &info.repo_path,
            vec![
                "worktree".into(),
                "remove".into(),
                "--force".into(),
                worktree.clone(),
            ],
            Duration::from_secs(30),
        )
        .await?;
        if !output.success {
            return Err(output.stderr);
        }
    }
    if let Some(branch) = info.branch.filter(|b| b.starts_with("duet/run-")) {
        let output = run_git_operation(
            &info.repo_path,
            vec!["branch".into(), "-D".into(), branch],
            Duration::from_secs(20),
        )
        .await?;
        if !output.success {
            return Err(format!(
                "worktree was discarded, but temporary branch cleanup failed: {}",
                output.stderr
            ));
        }
    }
    state.db.mark_discarded(run_id).map_err(err)?;
    Ok(())
}

async fn run_git_operation(
    repo: &str,
    mut args: Vec<String>,
    timeout: Duration,
) -> CommandResult<crate::process::ProcessOutput> {
    let binary = resolve_binary("git").ok_or_else(|| "Git executable not found".to_string())?;
    args.splice(0..0, ["-C".into(), repo.into()]);
    run_process(
        ProcessRequest {
            program: binary.to_string_lossy().into(),
            args,
            cwd: PathBuf::from(repo),
            timeout,
            env: vec![],
            stdin: None,
            capture_limit: 1_000_000,
            fail_on_output_limit: false,
        },
        CancellationToken::new(),
        Arc::new(|_: &str, _: &str| {}) as OutputCallback,
    )
    .await
    .map_err(err)
}

#[tauri::command]
pub fn get_preferences(state: State<'_, AppState>) -> CommandResult<AppPreferences> {
    state.db.get_preferences().map_err(err)
}

#[tauri::command]
pub fn save_preferences(
    state: State<'_, AppState>,
    preferences: AppPreferences,
) -> CommandResult<()> {
    validate_preferences(&preferences)?;
    state.db.save_preferences(&preferences).map_err(err)
}

#[tauri::command]
pub async fn open_project_in_editor(
    state: State<'_, AppState>,
    project_id: String,
) -> CommandResult<String> {
    let project = state.db.get_project(&project_id).map_err(err)?;
    let path = std::fs::canonicalize(&project.path).map_err(err)?;
    open_in_editor(&state, &path).await
}

#[tauri::command]
pub async fn open_run_in_editor(
    state: State<'_, AppState>,
    run_id: String,
) -> CommandResult<String> {
    let run = state.db.get_run(&run_id).map_err(err)?.run;
    let worktree = run
        .worktree_path
        .ok_or_else(|| "This run no longer has an isolated worktree".to_string())?;
    let path = std::fs::canonicalize(worktree).map_err(err)?;
    let root = std::fs::canonicalize(&state.worktrees_root).map_err(err)?;
    if !path.starts_with(&root) {
        return Err("Refusing to open a worktree outside Duet's managed directory".into());
    }
    open_in_editor(&state, &path).await
}

fn validate_preferences(preferences: &AppPreferences) -> CommandResult<()> {
    if !matches!(
        preferences.editor.as_str(),
        "auto" | "cursor" | "vscode" | "zed" | "terminal" | "finder"
    ) {
        return Err("Choose a supported editor".into());
    }
    if !(1..=5).contains(&preferences.max_repairs) {
        return Err("Default repair rounds must be between 1 and 5".into());
    }
    Ok(())
}

async fn open_in_editor(state: &State<'_, AppState>, path: &Path) -> CommandResult<String> {
    let preferences = state.db.get_preferences().map_err(err)?;
    validate_preferences(&preferences)?;
    let (program, args, label) = editor_command(&preferences.editor, path)?;
    let output = run_process(
        ProcessRequest {
            program,
            args,
            cwd: path.to_path_buf(),
            timeout: Duration::from_secs(10),
            env: vec![],
            stdin: None,
            capture_limit: 64_000,
            fail_on_output_limit: false,
        },
        CancellationToken::new(),
        Arc::new(|_: &str, _: &str| {}) as OutputCallback,
    )
    .await
    .map_err(err)?;
    if !output.success {
        return Err(format!("Could not open {label}: {}", output.stderr));
    }
    Ok(label)
}

#[cfg(target_os = "macos")]
fn editor_command(preference: &str, path: &Path) -> CommandResult<(String, Vec<String>, String)> {
    let selected = match preference {
        "auto" => ["Cursor", "Visual Studio Code", "Zed"]
            .into_iter()
            .find(|name| mac_app_exists(name)),
        "cursor" => Some("Cursor"),
        "vscode" => Some("Visual Studio Code"),
        "zed" => Some("Zed"),
        "terminal" => Some("Terminal"),
        "finder" => None,
        _ => return Err("Choose a supported editor".into()),
    };
    let path = path.to_string_lossy().into_owned();
    if let Some(app) = selected {
        if preference != "auto" && !mac_app_exists(app) && app != "Terminal" {
            return Err(format!(
                "{app} is not installed in /Applications or ~/Applications"
            ));
        }
        Ok((
            "/usr/bin/open".into(),
            vec!["-a".into(), app.into(), path],
            app.into(),
        ))
    } else {
        Ok(("/usr/bin/open".into(), vec![path], "Finder".into()))
    }
}

#[cfg(target_os = "macos")]
fn mac_app_exists(name: &str) -> bool {
    let bundle = format!("{name}.app");
    Path::new("/Applications").join(&bundle).exists()
        || dirs::home_dir().is_some_and(|home| home.join("Applications").join(bundle).exists())
}

#[cfg(not(target_os = "macos"))]
fn editor_command(preference: &str, path: &Path) -> CommandResult<(String, Vec<String>, String)> {
    let candidates: &[&str] = match preference {
        "auto" => &["cursor", "code", "zed", "xdg-open"],
        "cursor" => &["cursor"],
        "vscode" => &["code"],
        "zed" => &["zed"],
        "terminal" | "finder" => &["xdg-open"],
        _ => return Err("Choose a supported editor".into()),
    };
    let binary = candidates
        .iter()
        .find_map(|candidate| resolve_binary(candidate))
        .ok_or_else(|| "The selected editor was not found".to_string())?;
    let label = binary
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("editor")
        .to_string();
    Ok((
        binary.to_string_lossy().into_owned(),
        vec![path.to_string_lossy().into_owned()],
        label,
    ))
}

#[tauri::command]
pub async fn doctor(state: State<'_, AppState>) -> CommandResult<DoctorReport> {
    let root = state
        .worktrees_root
        .parent()
        .unwrap_or(&state.worktrees_root);
    let writable = std::fs::create_dir_all(root).is_ok();
    let (git, claude, codex) = tokio::join!(
        tool_status("git", &["--version"], None),
        tool_status("claude", &["--version"], Some(&["auth", "status"])),
        tool_status("codex", &["--version"], Some(&["login", "status"]))
    );
    Ok(DoctorReport {
        app_data_writable: writable,
        database_healthy: state.db.healthy(),
        git,
        claude,
        codex,
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    })
}

#[tauri::command]
pub async fn login_codex(state: State<'_, AppState>) -> CommandResult<ToolStatus> {
    let path = resolve_binary("codex")
        .ok_or_else(|| "Install the Codex CLI before signing in".to_string())?;
    let token = CancellationToken::new();
    {
        let mut active = state.active_runs.lock();
        if active.contains_key(CODEX_AUTH_OPERATION) {
            return Err("Codex sign-in is already in progress".into());
        }
        if !active.is_empty() {
            return Err("Stop active runs and console commands before signing in to Codex".into());
        }
        active.insert(CODEX_AUTH_OPERATION.into(), token.clone());
    }

    let result = async {
        if let Some(managed) = state.codex_server.lock().await.take() {
            state.codex_threads.lock().clear();
            managed.runtime.shutdown().await.map_err(err)?;
        }
        let output = run_process(
            ProcessRequest {
                program: path.to_string_lossy().into_owned(),
                args: vec!["login".into()],
                cwd: state
                    .worktrees_root
                    .parent()
                    .unwrap_or(&state.worktrees_root)
                    .to_path_buf(),
                timeout: Duration::from_secs(10 * 60),
                env: vec![],
                stdin: None,
                capture_limit: 64 * 1024,
                fail_on_output_limit: false,
            },
            token,
            Arc::new(|_, _| {}),
        )
        .await
        .map_err(err)?;
        if !output.success {
            return Err(if output.exit_code.is_none() {
                "Codex sign-in was cancelled or timed out".into()
            } else {
                "Codex sign-in did not complete".into()
            });
        }
        let status = tool_status("codex", &["--version"], Some(&["login", "status"])).await;
        if status.authenticated != Some(true) {
            return Err(
                "Codex finished the login flow, but authentication could not be confirmed".into(),
            );
        }
        Ok(status)
    }
    .await;
    state.active_runs.lock().remove(CODEX_AUTH_OPERATION);
    result
}

#[tauri::command]
pub fn codex_auth_in_progress(state: State<'_, AppState>) -> bool {
    state.active_runs.lock().contains_key(CODEX_AUTH_OPERATION)
}

#[tauri::command]
pub fn cancel_codex_login(state: State<'_, AppState>) -> CommandResult<()> {
    let token = state
        .active_runs
        .lock()
        .get(CODEX_AUTH_OPERATION)
        .cloned()
        .ok_or_else(|| "Codex sign-in is not running".to_string())?;
    token.cancel();
    Ok(())
}

async fn tool_status(name: &str, version_args: &[&str], auth_args: Option<&[&str]>) -> ToolStatus {
    let Some(path) = resolve_binary(name) else {
        return ToolStatus {
            installed: false,
            authenticated: None,
            path: None,
            version: None,
            detail: format!("{name} was not found in standard local install directories"),
        };
    };
    let version = bounded_tool_output(&path, version_args)
        .await
        .map(|(_, text)| text);
    let authenticated = match auth_args {
        Some(args) => bounded_tool_output(&path, args)
            .await
            .map(|(success, _)| success),
        None => None,
    };
    ToolStatus {
        installed: true,
        authenticated,
        path: Some(path.to_string_lossy().into()),
        version,
        detail: if auth_args.is_some() && authenticated.is_none() {
            "Detected locally; authentication status timed out or is not exposed by this CLI."
                .into()
        } else {
            "Detected locally; Duet never stores API credentials.".into()
        },
    }
}

async fn bounded_tool_output(path: &Path, args: &[&str]) -> Option<(bool, String)> {
    let mut command = tokio::process::Command::new(path);
    command
        .args(args)
        .env("PATH", path_for_program(path))
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(8), command.output())
        .await
        .ok()?
        .ok()?;
    Some((
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .trim()
        .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        clear_codex_thread_generation, ensure_codex_generation, sequence_has_gap,
        validate_preferences, validated_reveal_path, AppPreferences, CodexThreadOwner,
    };
    use parking_lot::Mutex;
    use std::collections::HashMap;

    #[test]
    fn detects_only_missing_runtime_events() {
        assert!(!sequence_has_gap(0, 42));
        assert!(!sequence_has_gap(41, 42));
        assert!(!sequence_has_gap(42, 42));
        assert!(sequence_has_gap(41, 43));
    }

    #[test]
    fn validates_editor_and_repair_preferences() {
        assert!(validate_preferences(&AppPreferences {
            editor: "cursor".into(),
            max_repairs: 3,
        })
        .is_ok());
        assert!(validate_preferences(&AppPreferences {
            editor: "custom-shell".into(),
            max_repairs: 3,
        })
        .is_err());
        assert!(validate_preferences(&AppPreferences {
            editor: "auto".into(),
            max_repairs: 0,
        })
        .is_err());
    }

    #[test]
    fn reveal_paths_must_be_the_exact_managed_worktree() {
        let root = tempfile::tempdir().unwrap();
        let expected = root.path().join("run-1/implementation");
        std::fs::create_dir_all(&expected).unwrap();
        assert_eq!(
            validated_reveal_path(root.path(), "run-1", &expected).unwrap(),
            expected.canonicalize().unwrap()
        );
        assert!(validated_reveal_path(root.path(), "run-1", root.path()).is_err());
    }

    #[test]
    fn stale_runtime_cleanup_preserves_replacement_thread_owners() {
        let owners = Mutex::new(HashMap::from([
            (
                "old".into(),
                CodexThreadOwner {
                    project_id: "project".into(),
                    cwd: "/old".into(),
                    generation: 4,
                },
            ),
            (
                "new".into(),
                CodexThreadOwner {
                    project_id: "project".into(),
                    cwd: "/new".into(),
                    generation: 5,
                },
            ),
        ]));

        clear_codex_thread_generation(&owners, 4);

        let owners = owners.lock();
        assert!(!owners.contains_key("old"));
        assert_eq!(owners.get("new").map(|owner| owner.generation), Some(5));
    }

    #[test]
    fn stale_thread_operations_cannot_cross_runtime_generations() {
        assert!(ensure_codex_generation(7, 7).is_ok());
        assert!(ensure_codex_generation(7, 8).is_err());
    }
}
