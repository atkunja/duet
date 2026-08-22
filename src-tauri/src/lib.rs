mod agents;
mod commands;
mod db;
mod git;
mod graph;
mod models;
mod process;
mod prompts;
mod tooling;
mod verification;
mod workflow;

use db::Database;
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};
use tauri::Manager;
use tokio_util::sync::CancellationToken;

pub struct AppState {
    db: Arc<Database>,
    worktrees_root: std::path::PathBuf,
    active_runs: Arc<Mutex<HashMap<String,CancellationToken>>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let data=app.path().app_data_dir()?; std::fs::create_dir_all(&data)?;
            let db=Arc::new(Database::open(&data.join("duet.sqlite3"))?); db.interrupt_active_runs()?;
            app.manage(AppState{db,worktrees_root:data.join("worktrees"),active_runs:Arc::new(Mutex::new(HashMap::new()))});
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::inspect_repository,commands::add_project,commands::list_projects,commands::remove_project,
            commands::start_run,commands::cancel_run,commands::list_runs,commands::get_run,commands::get_diff,
            commands::apply_changes,commands::discard_run,commands::doctor
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Duet");
}
