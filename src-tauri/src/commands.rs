use crate::{
    codex_app_server::{
        AppServerConfig, ClientInfo, CodexAppServerClient, ModelInfo, ModelListParams,
    },
    git,
    models::{
        DoctorReport, Project, RepoInspection, RunDetail, RunEvent, RunSummary, StartRunRequest,
        ToolStatus,
    },
    process::{run_process, OutputCallback, ProcessRequest},
    tooling::resolve_binary,
    verification::{self, VerificationItem},
    workflow::{self, WorkflowContext},
    AppState,
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
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn codex_app_server(state: &State<'_, AppState>) -> CommandResult<CodexAppServerClient> {
    let mut server = state.codex_server.lock().await;
    if server.as_ref().is_some_and(CodexAppServerClient::is_closed) {
        *server = None;
    }
    if let Some(client) = server.as_ref() {
        return Ok(client.clone());
    }
    let binary = resolve_binary("codex").ok_or_else(|| "Codex CLI was not found".to_string())?;
    let client = CodexAppServerClient::spawn(AppServerConfig::new(
        binary,
        ClientInfo::duet(env!("CARGO_PKG_VERSION")),
    ))
    .await
    .map_err(err)?;
    *server = Some(client.clone());
    Ok(client)
}

#[tauri::command]
pub async fn list_codex_models(state: State<'_, AppState>) -> CommandResult<Vec<ModelInfo>> {
    let client = codex_app_server(&state).await?;
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut models = Vec::new();
    loop {
        let response = client
            .list_models(ModelListParams {
                cursor: cursor.clone(),
                ..ModelListParams::default()
            })
            .await
            .map_err(err)?;
        models.extend(response.data.into_iter().filter(|model| !model.hidden));
        let Some(next_cursor) = response.next_cursor else {
            break;
        };
        if !seen_cursors.insert(next_cursor.clone()) || seen_cursors.len() > 64 {
            return Err("Codex App Server returned an invalid model cursor sequence".into());
        }
        cursor = Some(next_cursor);
    }
    Ok(models)
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
    state
        .db
        .create_run(
            &run_id,
            &request.project_id,
            request.task.trim(),
            &inspection.head_sha,
        )
        .map_err(err)?;
    let cancel = CancellationToken::new();
    state
        .active_runs
        .lock()
        .insert(run_id.clone(), cancel.clone());
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
    state
        .active_runs
        .lock()
        .insert(operation_id.clone(), token.clone());
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
    command.args(args).kill_on_drop(true);
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
