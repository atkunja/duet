use crate::models::{ChangedFile, Project, RunDetail, RunSummary, StageRecord, VerificationResult};
use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct Database {
    connection: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let connection = Connection::open(path).context("open Duet database")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { connection: Mutex::new(connection) };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.connection.lock().execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
                language TEXT NOT NULL, build_system TEXT NOT NULL,
                test_command TEXT NOT NULL DEFAULT '', benchmark_command TEXT NOT NULL DEFAULT '',
                last_used_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, task TEXT NOT NULL,
                status TEXT NOT NULL, current_stage TEXT NOT NULL DEFAULT 'queued',
                base_sha TEXT NOT NULL, branch TEXT, worktree_path TEXT,
                additions INTEGER NOT NULL DEFAULT 0, deletions INTEGER NOT NULL DEFAULT 0,
                architecture TEXT, review TEXT, error TEXT,
                created_at TEXT NOT NULL, completed_at TEXT,
                FOREIGN KEY(project_id) REFERENCES projects(id)
            );
            CREATE TABLE IF NOT EXISTS stages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL,
                kind TEXT NOT NULL, agent TEXT NOT NULL, status TEXT NOT NULL,
                summary TEXT NOT NULL DEFAULT '', raw_output TEXT NOT NULL DEFAULT '',
                started_at TEXT NOT NULL, completed_at TEXT, duration_ms INTEGER,
                FOREIGN KEY(run_id) REFERENCES runs(id)
            );
            CREATE TABLE IF NOT EXISTS verification_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL,
                name TEXT NOT NULL, command TEXT NOT NULL, success INTEGER NOT NULL,
                exit_code INTEGER, stdout TEXT NOT NULL, stderr TEXT NOT NULL,
                duration_ms INTEGER NOT NULL, required INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS changed_files (
                run_id TEXT NOT NULL, path TEXT NOT NULL, status TEXT NOT NULL,
                additions INTEGER NOT NULL DEFAULT 0, deletions INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(run_id, path)
            );
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL,
                kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY, value TEXT NOT NULL
            );"
        )?;
        Ok(())
    }

    pub fn healthy(&self) -> bool { self.connection.lock().query_row("SELECT 1", [], |_| Ok(())).is_ok() }

    pub fn interrupt_active_runs(&self) -> Result<()> {
        self.connection.lock().execute(
            "UPDATE runs SET status='interrupted', current_stage='interrupted', completed_at=?1 WHERE status IN ('queued','running')",
            [Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn upsert_project(&self, project: &Project) -> Result<()> {
        self.connection.lock().execute(
            "INSERT INTO projects(id,name,path,language,build_system,test_command,benchmark_command,last_used_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(path) DO UPDATE SET name=excluded.name, language=excluded.language,
             build_system=excluded.build_system, test_command=excluded.test_command,
             benchmark_command=excluded.benchmark_command, last_used_at=excluded.last_used_at",
            params![project.id, project.name, project.path, project.language, project.build_system,
                    project.test_command, project.benchmark_command, project.last_used_at],
        )?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.connection.lock();
        let mut stmt = conn.prepare("SELECT id,name,path,language,build_system,test_command,benchmark_command,last_used_at FROM projects ORDER BY last_used_at DESC")?;
        let rows = stmt.query_map([], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, path:r.get(2)?, language:r.get(3)?, build_system:r.get(4)?, test_command:r.get(5)?, benchmark_command:r.get(6)?, last_used_at:r.get(7)? }))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        self.connection.lock().query_row(
            "SELECT id,name,path,language,build_system,test_command,benchmark_command,last_used_at FROM projects WHERE id=?1",
            [id], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, path:r.get(2)?, language:r.get(3)?, build_system:r.get(4)?, test_command:r.get(5)?, benchmark_command:r.get(6)?, last_used_at:r.get(7)? })
        ).context("project not found")
    }

    pub fn remove_project(&self, id: &str) -> Result<()> {
        self.connection.lock().execute("DELETE FROM projects WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM runs WHERE project_id=?1)", [id])?;
        Ok(())
    }

    pub fn create_run(&self, id: &str, project_id: &str, task: &str, base_sha: &str) -> Result<()> {
        self.connection.lock().execute(
            "INSERT INTO runs(id,project_id,task,status,current_stage,base_sha,created_at) VALUES(?1,?2,?3,'queued','queued',?4,?5)",
            params![id, project_id, task, base_sha, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn set_run_worktree(&self, id: &str, branch: &str, path: &str) -> Result<()> {
        self.connection.lock().execute("UPDATE runs SET branch=?2,worktree_path=?3,status='running' WHERE id=?1", params![id, branch, path])?;
        Ok(())
    }

    pub fn set_run_stage(&self, id: &str, stage: &str) -> Result<()> {
        self.connection.lock().execute("UPDATE runs SET current_stage=?2,status='running' WHERE id=?1", params![id, stage])?;
        Ok(())
    }

    pub fn complete_run(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        self.connection.lock().execute(
            "UPDATE runs SET status=?2,current_stage=?2,error=?3,completed_at=?4 WHERE id=?1",
            params![id, status, error, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn set_architecture(&self, id: &str, output: &str) -> Result<()> {
        self.connection.lock().execute("UPDATE runs SET architecture=?2 WHERE id=?1", params![id, output])?; Ok(())
    }
    pub fn set_review(&self, id: &str, output: &str) -> Result<()> {
        self.connection.lock().execute("UPDATE runs SET review=?2 WHERE id=?1", params![id, output])?; Ok(())
    }

    pub fn start_stage(&self, run_id: &str, kind: &str, agent: &str) -> Result<i64> {
        let conn = self.connection.lock();
        conn.execute("INSERT INTO stages(run_id,kind,agent,status,started_at) VALUES(?1,?2,?3,'running',?4)", params![run_id, kind, agent, Utc::now().to_rfc3339()])?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_stage(&self, id: i64, success: bool, summary: &str, raw: &str, duration_ms: u64) -> Result<()> {
        self.connection.lock().execute(
            "UPDATE stages SET status=?2,summary=?3,raw_output=?4,completed_at=?5,duration_ms=?6 WHERE id=?1",
            params![id, if success {"completed"} else {"failed"}, summary, raw, Utc::now().to_rfc3339(), duration_ms as i64],
        )?; Ok(())
    }

    pub fn add_verification(&self, run_id: &str, v: &VerificationResult) -> Result<()> {
        self.connection.lock().execute(
            "INSERT INTO verification_results(run_id,name,command,success,exit_code,stdout,stderr,duration_ms,required,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![run_id,v.name,v.command,v.success as i32,v.exit_code,v.stdout,v.stderr,v.duration_ms as i64,v.required as i32,Utc::now().to_rfc3339()],
        )?; Ok(())
    }

    pub fn replace_changed_files(&self, run_id: &str, files: &[ChangedFile]) -> Result<()> {
        let mut conn = self.connection.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM changed_files WHERE run_id=?1", [run_id])?;
        let mut additions = 0i64; let mut deletions = 0i64;
        for file in files {
            additions += file.additions; deletions += file.deletions;
            tx.execute("INSERT INTO changed_files(run_id,path,status,additions,deletions) VALUES(?1,?2,?3,?4,?5)", params![run_id,file.path,file.status,file.additions,file.deletions])?;
        }
        tx.execute("UPDATE runs SET additions=?2,deletions=?3 WHERE id=?1", params![run_id,additions,deletions])?;
        tx.commit()?; Ok(())
    }

    pub fn add_event(&self, run_id: &str, kind: &str, payload: &str) -> Result<()> {
        self.connection.lock().execute("INSERT INTO events(run_id,kind,payload,created_at) VALUES(?1,?2,?3,?4)", params![run_id,kind,payload,Utc::now().to_rfc3339()])?; Ok(())
    }

    pub fn list_runs(&self) -> Result<Vec<RunSummary>> {
        let conn = self.connection.lock();
        let mut stmt = conn.prepare("SELECT r.id,r.project_id,p.name,r.task,r.status,r.current_stage,r.created_at,r.completed_at,r.worktree_path,r.additions,r.deletions FROM runs r JOIN projects p ON p.id=r.project_id ORDER BY r.created_at DESC")?;
        let rows = stmt.query_map([], map_run)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_run(&self, id: &str) -> Result<RunDetail> {
        let conn = self.connection.lock();
        let run = conn.query_row("SELECT r.id,r.project_id,p.name,r.task,r.status,r.current_stage,r.created_at,r.completed_at,r.worktree_path,r.additions,r.deletions FROM runs r JOIN projects p ON p.id=r.project_id WHERE r.id=?1", [id], map_run)?;
        let (architecture, review): (Option<String>, Option<String>) = conn.query_row("SELECT architecture,review FROM runs WHERE id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?)))?;
        let mut stage_stmt = conn.prepare("SELECT id,run_id,kind,agent,status,summary,raw_output,started_at,completed_at,duration_ms FROM stages WHERE run_id=?1 ORDER BY id")?;
        let stages = stage_stmt.query_map([id], |r| Ok(StageRecord{id:r.get(0)?,run_id:r.get(1)?,kind:r.get(2)?,agent:r.get(3)?,status:r.get(4)?,summary:r.get(5)?,raw_output:r.get(6)?,started_at:r.get(7)?,completed_at:r.get(8)?,duration_ms:r.get(9)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut verify_stmt = conn.prepare("SELECT name,command,success,exit_code,stdout,stderr,duration_ms,required FROM verification_results WHERE run_id=?1 ORDER BY id")?;
        let verification = verify_stmt.query_map([id], |r| Ok(VerificationResult{name:r.get(0)?,command:r.get(1)?,success:r.get::<_,i64>(2)?!=0,exit_code:r.get(3)?,stdout:r.get(4)?,stderr:r.get(5)?,duration_ms:r.get::<_,i64>(6)? as u64,required:r.get::<_,i64>(7)?!=0}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut file_stmt = conn.prepare("SELECT path,status,additions,deletions FROM changed_files WHERE run_id=?1 ORDER BY path")?;
        let changed_files = file_stmt.query_map([id], |r| Ok(ChangedFile{path:r.get(0)?,status:r.get(1)?,additions:r.get(2)?,deletions:r.get(3)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(RunDetail { run, stages, architecture, review, verification, changed_files })
    }

    pub fn worktree_for_run(&self, id: &str) -> Result<Option<String>> {
        Ok(self.connection.lock().query_row("SELECT worktree_path FROM runs WHERE id=?1", [id], |r| r.get(0)).optional()?.flatten())
    }

    pub fn apply_info(&self, id: &str) -> Result<(String, String, String, Option<String>)> {
        self.connection.lock().query_row(
            "SELECT p.path,r.base_sha,r.worktree_path,r.branch FROM runs r JOIN projects p ON p.id=r.project_id WHERE r.id=?1",
            [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).context("run not found")
    }
}

fn map_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunSummary> {
    Ok(RunSummary{id:r.get(0)?,project_id:r.get(1)?,project_name:r.get(2)?,task:r.get(3)?,status:r.get(4)?,current_stage:r.get(5)?,created_at:r.get(6)?,completed_at:r.get(7)?,worktree_path:r.get(8)?,additions:r.get(9)?,deletions:r.get(10)?})
}
