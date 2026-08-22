use crate::{git, models::{DoctorReport, Project, RepoInspection, RunDetail, RunEvent, RunSummary, StartRunRequest, ToolStatus}, workflow::{self, WorkflowContext}, AppState};
use chrono::Utc;
use std::{path::{Path, PathBuf}, process::Command};
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type CommandResult<T> = Result<T, String>;
fn err(error: impl std::fmt::Display) -> String { error.to_string() }

#[tauri::command]
pub async fn inspect_repository(path: String) -> CommandResult<RepoInspection> {
    git::inspect_repository(Path::new(&path)).await.map_err(err)
}

#[tauri::command]
pub async fn add_project(state: State<'_, AppState>, path: String) -> CommandResult<Project> {
    let inspection = git::inspect_repository(Path::new(&path)).await.map_err(err)?;
    let name = Path::new(&inspection.path).file_name().and_then(|v|v.to_str()).unwrap_or("Repository").to_string();
    let project = Project { id:Uuid::new_v4().to_string(),name,path:inspection.path,language:inspection.language,build_system:inspection.build_system,test_command:inspection.suggested_test_command,benchmark_command:String::new(),last_used_at:Utc::now().to_rfc3339() };
    state.db.upsert_project(&project).map_err(err)?;
    state.db.list_projects().map_err(err)?.into_iter().find(|p|p.path==project.path).ok_or_else(||"could not load saved project".into())
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> CommandResult<Vec<Project>> { state.db.list_projects().map_err(err) }

#[tauri::command]
pub fn remove_project(state: State<'_, AppState>, project_id: String) -> CommandResult<()> { state.db.remove_project(&project_id).map_err(err) }

#[tauri::command]
pub async fn start_run(app:AppHandle,state:State<'_,AppState>,request:StartRunRequest)->CommandResult<String>{
    if request.task.trim().is_empty(){return Err("Describe what you want Duet to build".into())}
    let project=state.db.get_project(&request.project_id).map_err(err)?;
    let inspection=git::inspect_repository(Path::new(&project.path)).await.map_err(err)?;
    let run_id=Uuid::new_v4().to_string(); state.db.create_run(&run_id,&request.project_id,request.task.trim(),&inspection.head_sha).map_err(err)?;
    let cancel=CancellationToken::new(); state.active_runs.lock().insert(run_id.clone(),cancel.clone());
    let ctx=WorkflowContext{app:app.clone(),db:state.db.clone(),worktrees_root:state.worktrees_root.clone(),run_id:run_id.clone(),request,cancel:cancel.clone()};
    let active=state.active_runs.clone(); let db=state.db.clone(); let task_id=run_id.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error)=workflow::execute(ctx).await {
            if cancel.is_cancelled(){let _=db.complete_run(&task_id,"cancelled",Some("cancelled by user"));let _=app.emit("duet://run-event",RunEvent::RunCancelled{run_id:task_id.clone()});}
            else {let reason=error.to_string();let _=db.complete_run(&task_id,"failed",Some(&reason));let _=app.emit("duet://run-event",RunEvent::RunFailed{run_id:task_id.clone(),reason});}
        }
        active.lock().remove(&task_id);
    });
    Ok(run_id)
}

#[tauri::command]
pub fn cancel_run(state:State<'_,AppState>,run_id:String)->CommandResult<()> {
    let token=state.active_runs.lock().get(&run_id).cloned().ok_or_else(||"run is not active".to_string())?; token.cancel(); Ok(())
}

#[tauri::command]
pub fn list_runs(state:State<'_,AppState>)->CommandResult<Vec<RunSummary>>{state.db.list_runs().map_err(err)}

#[tauri::command]
pub fn get_run(state:State<'_,AppState>,run_id:String)->CommandResult<RunDetail>{state.db.get_run(&run_id).map_err(err)}

#[tauri::command]
pub async fn get_diff(state:State<'_,AppState>,run_id:String)->CommandResult<String>{
    let path=state.db.worktree_for_run(&run_id).map_err(err)?.ok_or_else(||"run has no worktree".to_string())?; git::diff(Path::new(&path)).await.map_err(err)
}

#[tauri::command]
pub async fn apply_changes(state:State<'_,AppState>,run_id:String)->CommandResult<()> {
    let (repo,base,worktree,_)=state.db.apply_info(&run_id).map_err(err)?; git::apply_worktree_changes(Path::new(&repo),Path::new(&worktree),&base).await.map_err(err)
}

#[tauri::command]
pub async fn discard_run(state:State<'_,AppState>,run_id:String)->CommandResult<()> {
    if state.active_runs.lock().contains_key(&run_id){return Err("stop the active run before discarding it".into())}
    let (repo,_,worktree,branch)=state.db.apply_info(&run_id).map_err(err)?;
    let path=PathBuf::from(&worktree); if !path.starts_with(&state.worktrees_root){return Err("refusing to remove a worktree outside Duet's data directory".into())}
    let output=tokio::process::Command::new("git").arg("-C").arg(&repo).args(["worktree","remove","--force",&worktree]).output().await.map_err(err)?;
    if !output.status.success(){return Err(String::from_utf8_lossy(&output.stderr).into())}
    if let Some(branch)=branch.filter(|b|b.starts_with("duet/run-")){let _=tokio::process::Command::new("git").arg("-C").arg(&repo).args(["branch","-D",&branch]).output().await;}
    Ok(())
}

#[tauri::command]
pub async fn doctor(state:State<'_,AppState>)->CommandResult<DoctorReport>{
    let root=state.worktrees_root.parent().unwrap_or(&state.worktrees_root); let writable=std::fs::create_dir_all(root).is_ok();
    let git=tool_status("git",&["--version"],None); let claude=tool_status("claude",&["--version"],Some(&["auth","status"])); let codex=tool_status("codex",&["--version"],Some(&["login","status"]));
    Ok(DoctorReport{app_data_writable:writable,database_healthy:state.db.healthy(),git,claude,codex,os:format!("{} {}",std::env::consts::OS,std::env::consts::ARCH)})
}

fn tool_status(name:&str,version_args:&[&str],auth_args:Option<&[&str]>)->ToolStatus{
    let Ok(path)=which::which(name) else{return ToolStatus{installed:false,authenticated:None,path:None,version:None,detail:format!("{name} was not found on PATH")}};
    let version=Command::new(&path).args(version_args).output().ok().map(|o|format!("{}{}",String::from_utf8_lossy(&o.stdout),String::from_utf8_lossy(&o.stderr)).trim().to_string());
    let authenticated=auth_args.and_then(|args|Command::new(&path).args(args).output().ok()).map(|o|o.status.success());
    ToolStatus{installed:true,authenticated,path:Some(path.to_string_lossy().into()),version,detail:"Detected locally; Duet never stores API credentials.".into()}
}
