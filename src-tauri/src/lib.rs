mod agents;
pub mod codex_app_server;
pub mod codex_runtime;
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
use fs2::FileExt;
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tauri::Manager;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

pub struct AppState {
    db: Arc<Database>,
    worktrees_root: std::path::PathBuf,
    active_runs: Arc<Mutex<HashMap<String, CancellationToken>>>,
    run_operations: Arc<Mutex<HashSet<String>>>,
    codex_server: Arc<AsyncMutex<Option<codex_runtime::CodexRuntime>>>,
    codex_threads: Arc<Mutex<HashMap<String, CodexThreadOwner>>>,
    _instance_lock: File,
}

#[derive(Clone)]
struct CodexThreadOwner {
    project_id: String,
    cwd: String,
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
            let instance_lock = acquire_instance_lock(&data)?;
            let db = Arc::new(Database::open(&data.join("duet.sqlite3"))?);
            db.interrupt_active_runs()?;
            app.manage(AppState {
                db,
                worktrees_root: data.join("worktrees"),
                active_runs: Arc::new(Mutex::new(HashMap::new())),
                run_operations: Arc::new(Mutex::new(HashSet::new())),
                codex_server: Arc::new(AsyncMutex::new(None)),
                codex_threads: Arc::new(Mutex::new(HashMap::new())),
                _instance_lock: instance_lock,
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                let codex_server_active = state.codex_server.try_lock().map_or(true, |server| {
                    server.as_ref().is_some_and(|client| !client.is_closed())
                });
                if !state.active_runs.lock().is_empty()
                    || !state.run_operations.lock().is_empty()
                    || codex_server_active
                {
                    api.prevent_close();
                    for token in state.active_runs.lock().values() {
                        token.cancel();
                    }
                    let window = window.clone();
                    let active = state.active_runs.clone();
                    let operations = state.run_operations.clone();
                    let codex_server = state.codex_server.clone();
                    tauri::async_runtime::spawn(async move {
                        let settle_operations = async {
                            while !active.lock().is_empty() || !operations.lock().is_empty() {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        };
                        let shutdown_codex = async {
                            if let Some(client) = codex_server.lock().await.take() {
                                let _ = client.shutdown().await;
                            }
                        };
                        let _ = tokio::time::timeout(Duration::from_secs(40), async {
                            tokio::join!(settle_operations, shutdown_codex);
                        })
                        .await;
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
            commands::run_project_command,
            commands::cancel_project_command,
            commands::open_local_preview,
            commands::open_project_in_editor,
            commands::open_run_in_editor,
            commands::apply_changes,
            commands::discard_run,
            commands::doctor,
            commands::get_preferences,
            commands::save_preferences,
            commands::list_codex_models,
            commands::start_codex_thread,
            commands::start_codex_turn,
            commands::interrupt_codex_turn
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Duet");
}

fn acquire_instance_lock(data: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join("duet.lock"))?;
    FileExt::try_lock_exclusive(&file).map_err(|_| {
        anyhow::anyhow!("Duet is already running. Use the existing window to manage active runs.")
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_instance_can_own_the_data_directory() {
        let directory = tempfile::tempdir().unwrap();
        let _owner = acquire_instance_lock(directory.path()).unwrap();
        assert!(acquire_instance_lock(directory.path()).is_err());
    }
}
