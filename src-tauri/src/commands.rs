use crate::{
    git,
    models::{
        DoctorReport, Project, RepoInspection, RunDetail, RunEvent, RunSummary, StartRunRequest,
        ToolStatus,
    },
    tooling::resolve_binary,
    workflow::{self, WorkflowContext},
    AppState,
};
use chrono::Utc;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type CommandResult<T> = Result<T, String>;
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
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
        git::apply_worktree_changes(
            Path::new(&info.repo_path),
            Path::new(&worktree),
            &info.base_sha,
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
    let canonical = path.canonicalize().map_err(err)?;
    let expected = state
        .worktrees_root
        .join(run_id)
        .join("implementation")
        .canonicalize()
        .map_err(err)?;
    if canonical != expected {
        return Err(
            "refusing to remove a worktree that does not exactly match this Duet run".into(),
        );
    }
    let command = tokio::process::Command::new("git")
        .arg("-C")
        .arg(&info.repo_path)
        .args(["worktree", "remove", "--force", &worktree])
        .output();
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), command)
        .await
        .map_err(|_| "Git worktree removal timed out".to_string())?
        .map_err(err)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    state.db.mark_discarded(run_id).map_err(err)?;
    if let Some(branch) = info.branch.filter(|b| b.starts_with("duet/run-")) {
        let command = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&info.repo_path)
            .args(["branch", "-D", &branch])
            .output();
        let output = tokio::time::timeout(std::time::Duration::from_secs(20), command)
            .await
            .map_err(|_| {
                "worktree was removed, but temporary branch cleanup timed out".to_string()
            })?
            .map_err(err)?;
        if !output.status.success() {
            return Err(format!(
                "worktree was discarded, but temporary branch cleanup failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
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
