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
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tauri::Manager;
use tokio_util::sync::CancellationToken;

pub struct AppState {
    db: Arc<Database>,
    worktrees_root: std::path::PathBuf,
    active_runs: Arc<Mutex<HashMap<String, CancellationToken>>>,
    run_operations: Arc<Mutex<HashSet<String>>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let db = Arc::new(Database::open(&data.join("duet.sqlite3"))?);
            db.interrupt_active_runs()?;
            app.manage(AppState {
                db,
                worktrees_root: data.join("worktrees"),
                active_runs: Arc::new(Mutex::new(HashMap::new())),
                run_operations: Arc::new(Mutex::new(HashSet::new())),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if !state.active_runs.lock().is_empty() || !state.run_operations.lock().is_empty() {
                    api.prevent_close();
                    for token in state.active_runs.lock().values() {
                        token.cancel();
                    }
                    let window = window.clone();
                    let active = state.active_runs.clone();
                    let operations = state.run_operations.clone();
                    tauri::async_runtime::spawn(async move {
                        while !active.lock().is_empty() || !operations.lock().is_empty() {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        let _ = window.destroy();
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::inspect_repository,
            commands::add_project,
            commands::list_projects,
            commands::remove_project,
            commands::start_run,
            commands::cancel_run,
            commands::list_runs,
            commands::get_run,
            commands::get_diff,
            commands::apply_changes,
            commands::discard_run,
            commands::doctor
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Duet");
}
