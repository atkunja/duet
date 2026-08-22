use crate::{
    agents::{
        parse_architecture, parse_review, Agent, AgentRequest, AgentRole, ClaudeAgent, CodexAgent,
        MockAgent,
    },
    db::Database,
    git,
    graph::{self, TaskStatus},
    models::{RunEvent, StartRunRequest, VerificationResult},
    process::OutputCallback,
    prompts,
    tooling::resolve_binary,
    verification::{self, VerificationItem},
};
use anyhow::{anyhow, Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Runtime};
use tokio_util::sync::CancellationToken;

pub struct WorkflowContext<R: Runtime = tauri::Wry> {
    pub app: AppHandle<R>,
    pub db: Arc<Database>,
    pub worktrees_root: PathBuf,
    pub run_id: String,
    pub request: StartRunRequest,
    pub cancel: CancellationToken,
}

type WorkflowAgents = (
    Box<dyn Agent>,
    Box<dyn Agent>,
    Box<dyn Agent>,
    Box<dyn Agent>,
);

pub async fn execute<R: Runtime>(ctx: WorkflowContext<R>) -> Result<()> {
    let mut task_graph = graph::default_workflow();
    task_graph.validate()?;
    let project = ctx.db.get_project(&ctx.request.project_id)?;
    let repo = PathBuf::from(&project.path);
    let base_sha = ctx.db.base_sha_for_run(&ctx.run_id)?;
    let inspection = git::inspect_repository(&repo).await?;
    task_graph.set_status("inspect", TaskStatus::Completed)?;
    let _ready_nodes = task_graph.ready();
    emit(
        &ctx,
        RunEvent::RunStarted {
            run_id: ctx.run_id.clone(),
            task: ctx.request.task.clone(),
        },
    );

    ctx.db.set_run_stage(&ctx.run_id, "preparing")?;
    let (worktree, branch) =
        git::create_worktree(&repo, &ctx.worktrees_root, &ctx.run_id, &base_sha)
            .await
            .context("create isolated Git worktree")?;
    ctx.db
        .set_run_worktree(&ctx.run_id, &branch, &worktree.to_string_lossy())?;

    let (architect_agent, builder_agent, reviewer_agent, repair_agent): WorkflowAgents =
        if ctx.request.mock_agents {
            let (planning_name, building_name) = match ctx.request.agent_mode.as_str() {
                "codex" => ("Codex", "Codex"),
                "claude" => ("Claude", "Claude"),
                _ => ("Codex", "Claude"),
            };
            (
                Box::new(MockAgent {
                    agent_name: planning_name,
                }),
                Box::new(MockAgent {
                    agent_name: building_name,
                }),
                Box::new(MockAgent {
                    agent_name: planning_name,
                }),
                Box::new(MockAgent {
                    agent_name: building_name,
                }),
            )
        } else {
            let codex = if ctx.request.agent_mode != "claude" {
                Some(CodexAgent {
                    binary: resolve_binary("codex").context(
                        "Codex CLI was not found in PATH or standard local install directories",
                    )?,
                    model: ctx.request.codex_model.clone(),
                    reasoning: ctx.request.codex_reasoning.clone(),
                })
            } else {
                None
            };
            let claude = if ctx.request.agent_mode != "codex" {
                Some(ClaudeAgent {
                    binary: resolve_binary("claude").context(
                        "Claude Code was not found in PATH or standard local install directories",
                    )?,
                    model: ctx.request.claude_model.clone(),
                    reasoning: ctx.request.claude_reasoning.clone(),
                })
            } else {
                None
            };
            match ctx.request.agent_mode.as_str() {
                "codex" => {
                    let agent = codex.expect("validated Codex mode must initialize Codex");
                    (
                        Box::new(agent.clone()),
                        Box::new(agent.clone()),
                        Box::new(agent.clone()),
                        Box::new(agent),
                    )
                }
                "claude" => {
                    let agent = claude.expect("validated Claude mode must initialize Claude");
                    (
                        Box::new(agent.clone()),
                        Box::new(agent.clone()),
                        Box::new(agent.clone()),
                        Box::new(agent),
                    )
                }
                _ => {
                    let codex = codex.expect("Duet mode must initialize Codex");
                    let claude = claude.expect("Duet mode must initialize Claude");
                    (
                        Box::new(codex.clone()),
                        Box::new(claude.clone()),
                        Box::new(codex),
                        Box::new(claude),
                    )
                }
            }
        };

    let overview = format!(
        "Path: {}\nBranch: {}\nBase SHA: {}\nLanguage: {}\nBuild system: {}\nDirty source tree: {}",
        worktree.display(),
        inspection.branch,
        base_sha,
        inspection.language,
        inspection.build_system,
        inspection.dirty
    );
    let architecture_result = run_agent(
        &ctx,
        architect_agent.as_ref(),
        AgentRole::Architect,
        "architect",
        &worktree,
        prompts::architect(&ctx.request.task, &overview),
    )
    .await?;
    if !architecture_result.success {
        return Err(anyhow!(
            "{} architecture failed: {}",
            architect_agent.name(),
            architecture_result.stderr
        ));
    }
    let architecture = parse_architecture(&architecture_result.normalized_output)?;
    let architecture_json = serde_json::to_string_pretty(&architecture)?;
    ctx.db.set_architecture(&ctx.run_id, &architecture_json)?;

    let implementation = run_agent(
        &ctx,
        builder_agent.as_ref(),
        AgentRole::Implementer,
        "build",
        &worktree,
        prompts::implementer(
            &ctx.request.task,
            &architecture_json,
            &ctx.request.test_command,
        ),
    )
    .await?;
    if !implementation.success {
        return Err(anyhow!(
            "{} implementation failed: {}",
            builder_agent.name(),
            implementation.stderr
        ));
    }
    refresh_changes(&ctx, &worktree, &base_sha).await?;

    let mut verification_results = run_verification(&ctx, &worktree).await?;
    let (mut review, mut reviewed_patch_sha256) = perform_review_snapshot(
        &ctx,
        reviewer_agent.as_ref(),
        &worktree,
        &base_sha,
        &architecture_json,
        &verification_results,
    )
    .await?;
    let mut last_diff = git::diff(&worktree, &base_sha).await.unwrap_or_default();
    let mut unchanged_repairs = 0u8;
    let mut round = 0u8;

    while (!required_checks_pass(&verification_results) || review.verdict != "pass")
        && round < ctx.request.max_repairs
    {
        if ctx.cancel.is_cancelled() {
            return Err(anyhow!("run cancelled"));
        }
        round += 1;
        let verification_text = verification::summarize(&verification_results);
        let review_json = serde_json::to_string_pretty(&review)?;
        let repair = run_agent(
            &ctx,
            repair_agent.as_ref(),
            AgentRole::Repair,
            &format!("repair-{round}"),
            &worktree,
            prompts::repair(
                &ctx.request.task,
                &architecture_json,
                &verification_text,
                &review_json,
                round,
            ),
        )
        .await?;
        if !repair.success {
            return Err(anyhow!(
                "{} repair round {round} failed: {}",
                repair_agent.name(),
                repair.stderr
            ));
        }
        let new_diff = git::diff(&worktree, &base_sha).await.unwrap_or_default();
        if new_diff == last_diff {
            unchanged_repairs += 1;
        } else {
            unchanged_repairs = 0;
        }
        last_diff = new_diff;
        refresh_changes(&ctx, &worktree, &base_sha).await?;
        verification_results = run_verification(&ctx, &worktree).await?;
        (review, reviewed_patch_sha256) = perform_review_snapshot(
            &ctx,
            reviewer_agent.as_ref(),
            &worktree,
            &base_sha,
            &architecture_json,
            &verification_results,
        )
        .await?;
        if unchanged_repairs >= 2 {
            break;
        }
    }

    let verified = required_checks_pass(&verification_results) && review.verdict == "pass";
    if verified {
        ctx.db
            .set_verified_patch_sha256(&ctx.run_id, &reviewed_patch_sha256)?;
    }
    ctx.db.complete_run(
        &ctx.run_id,
        if verified { "completed" } else { "failed" },
        if verified {
            None
        } else {
            Some("verification or review did not pass within the repair limit")
        },
    )?;
    if verified {
        emit(
            &ctx,
            RunEvent::RunCompleted {
                run_id: ctx.run_id.clone(),
                verified: true,
            },
        );
        Ok(())
    } else {
        emit(
            &ctx,
            RunEvent::RunFailed {
                run_id: ctx.run_id.clone(),
                reason: "Repair limit reached with unresolved failures".into(),
            },
        );
        Ok(())
    }
}

async fn run_agent<R: Runtime>(
    ctx: &WorkflowContext<R>,
    agent: &dyn Agent,
    role: AgentRole,
    stage: &str,
    worktree: &Path,
    prompt: String,
) -> Result<crate::agents::AgentResult> {
    ctx.db.set_run_stage(&ctx.run_id, stage)?;
    let stage_id = ctx.db.start_stage(&ctx.run_id, stage, agent.name())?;
    emit(
        ctx,
        RunEvent::StageStarted {
            run_id: ctx.run_id.clone(),
            stage: stage.into(),
            agent: agent.name().into(),
        },
    );
    let callback = output_callback(ctx, stage);
    let result = agent
        .execute(
            AgentRequest {
                role,
                prompt,
                worktree: worktree.into(),
                timeout: Duration::from_secs(60 * 45),
                callback,
            },
            ctx.cancel.clone(),
        )
        .await;
    match result {
        Ok(result) => {
            let summary = if result.success {
                "Completed".to_string()
            } else {
                format!("Agent exited with status {:?}", result.exit_code)
            };
            ctx.db.finish_stage(
                stage_id,
                result.success,
                &summary,
                &format!("{}\n{}", result.raw_output, result.stderr),
                &result.normalized_output,
                result.duration_ms,
            )?;
            emit(
                ctx,
                RunEvent::StageCompleted {
                    run_id: ctx.run_id.clone(),
                    stage: stage.into(),
                    success: result.success,
                    summary: if result.success {
                        "Completed".into()
                    } else {
                        "Agent failed".into()
                    },
                },
            );
            Ok(result)
        }
        Err(error) => {
            ctx.db
                .finish_stage(stage_id, false, &error.to_string(), "", "", 0)?;
            Err(error)
        }
    }
}

async fn run_verification<R: Runtime>(
    ctx: &WorkflowContext<R>,
    worktree: &Path,
) -> Result<Vec<VerificationResult>> {
    ctx.db.set_run_stage(&ctx.run_id, "verify")?;
    let stage_id = ctx.db.start_stage(&ctx.run_id, "verify", "Duet")?;
    emit(
        ctx,
        RunEvent::StageStarted {
            run_id: ctx.run_id.clone(),
            stage: "verify".into(),
            agent: "Duet".into(),
        },
    );
    let mut items = Vec::new();
    if !ctx.request.test_command.trim().is_empty() {
        items.push(VerificationItem {
            name: "Tests".into(),
            command: ctx.request.test_command.clone(),
            timeout: Duration::from_secs(20 * 60),
            required: true,
        });
    }
    if let Some(command) = ctx
        .request
        .benchmark_command
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        items.push(VerificationItem {
            name: "Benchmark".into(),
            command: command.clone(),
            timeout: Duration::from_secs(30 * 60),
            required: true,
        });
    }
    if items.is_empty() {
        let result = VerificationResult {
            name: "Tests".into(),
            command: "Not configured".into(),
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: "A required test or build command was not configured.".into(),
            duration_ms: 0,
            required: true,
        };
        ctx.db.add_verification(&ctx.run_id, &result)?;
        ctx.db.finish_stage(
            stage_id,
            false,
            "Required verification is not configured",
            &result.stderr,
            &result.stderr,
            0,
        )?;
        emit(
            ctx,
            RunEvent::VerificationCompleted {
                run_id: ctx.run_id.clone(),
                result: result.clone(),
            },
        );
        return Ok(vec![result]);
    }
    let mut results = Vec::new();
    for item in items {
        let result = match verification::execute(
            item.clone(),
            worktree,
            ctx.cancel.clone(),
            output_callback(ctx, "verify"),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => VerificationResult {
                name: item.name,
                command: item.command,
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: error.to_string(),
                duration_ms: 0,
                required: item.required,
            },
        };
        ctx.db.add_verification(&ctx.run_id, &result)?;
        emit(
            ctx,
            RunEvent::VerificationCompleted {
                run_id: ctx.run_id.clone(),
                result: result.clone(),
            },
        );
        results.push(result);
        if ctx.cancel.is_cancelled() {
            let summary = verification::summarize(&results);
            ctx.db.finish_stage(
                stage_id,
                false,
                "Verification cancelled",
                &summary,
                &summary,
                0,
            )?;
            return Err(anyhow!("run cancelled"));
        }
    }
    let summary = verification::summarize(&results);
    ctx.db.finish_stage(
        stage_id,
        required_checks_pass(&results),
        &summary,
        &summary,
        &summary,
        results.iter().map(|r| r.duration_ms).max().unwrap_or(0),
    )?;
    emit(
        ctx,
        RunEvent::StageCompleted {
            run_id: ctx.run_id.clone(),
            stage: "verify".into(),
            success: required_checks_pass(&results),
            summary,
        },
    );
    Ok(results)
}

async fn perform_review_snapshot<R: Runtime>(
    ctx: &WorkflowContext<R>,
    reviewer_agent: &dyn Agent,
    worktree: &Path,
    base_sha: &str,
    architecture: &str,
    verification_results: &[VerificationResult],
) -> Result<(crate::models::ReviewResult, String)> {
    let full_diff = git::diff(worktree, base_sha).await?;
    let reviewed_patch_sha256 = git::patch_content_sha256(&full_diff);
    let review = perform_review(
        ctx,
        reviewer_agent,
        worktree,
        architecture,
        verification_results,
        &full_diff,
    )
    .await?;
    let current_patch_sha256 = git::patch_sha256(worktree, base_sha).await?;
    anyhow::ensure!(
        current_patch_sha256 == reviewed_patch_sha256,
        "the isolated worktree changed during review; verification result was not accepted"
    );
    Ok((review, reviewed_patch_sha256))
}

async fn perform_review<R: Runtime>(
    ctx: &WorkflowContext<R>,
    reviewer_agent: &dyn Agent,
    worktree: &Path,
    architecture: &str,
    verification_results: &[VerificationResult],
    full_diff: &str,
) -> Result<crate::models::ReviewResult> {
    let diff = if full_diff.len() > 300_000 {
        format!(
            "{}\n…[diff truncated]",
            &full_diff[..full_diff.floor_char_boundary(300_000)]
        )
    } else {
        full_diff.to_string()
    };
    let result = run_agent(
        ctx,
        reviewer_agent,
        AgentRole::Reviewer,
        "review",
        worktree,
        prompts::reviewer(
            &ctx.request.task,
            architecture,
            &diff,
            &verification::summarize(verification_results),
        ),
    )
    .await?;
    if !result.success {
        return Err(anyhow!(
            "{} review failed: {}",
            reviewer_agent.name(),
            result.stderr
        ));
    }
    let review = parse_review(&result.normalized_output)?;
    let review_json = serde_json::to_string_pretty(&review)?;
    ctx.db.set_review(&ctx.run_id, &review_json)?;
    emit(
        ctx,
        RunEvent::ReviewCompleted {
            run_id: ctx.run_id.clone(),
            verdict: review.verdict.clone(),
            issues: review.issues.len(),
        },
    );
    Ok(review)
}

async fn refresh_changes<R: Runtime>(
    ctx: &WorkflowContext<R>,
    worktree: &Path,
    base_sha: &str,
) -> Result<()> {
    let files = git::changed_files(worktree, base_sha).await?;
    for file in &files {
        emit(
            ctx,
            RunEvent::FileChanged {
                run_id: ctx.run_id.clone(),
                path: file.path.clone(),
            },
        );
    }
    ctx.db.replace_changed_files(&ctx.run_id, &files)?;
    Ok(())
}
fn required_checks_pass(results: &[VerificationResult]) -> bool {
    results.iter().all(|r| !r.required || r.success)
}
fn output_callback<R: Runtime>(ctx: &WorkflowContext<R>, stage: &str) -> OutputCallback {
    let app = ctx.app.clone();
    let run_id = ctx.run_id.clone();
    let stage = stage.to_string();
    Arc::new(move |stream, line| {
        let event = RunEvent::AgentOutput {
            run_id: run_id.clone(),
            stage: stage.clone(),
            stream: stream.into(),
            line: line.into(),
        };
        let _ = app.emit("duet://run-event", event);
    })
}
fn emit<R: Runtime>(ctx: &WorkflowContext<R>, event: RunEvent) {
    let kind = match &event {
        RunEvent::RunStarted { .. } => "runStarted",
        RunEvent::StageStarted { .. } => "stageStarted",
        RunEvent::AgentOutput { .. } => "agentOutput",
        RunEvent::StageCompleted { .. } => "stageCompleted",
        RunEvent::FileChanged { .. } => "fileChanged",
        RunEvent::VerificationCompleted { .. } => "verificationCompleted",
        RunEvent::ReviewCompleted { .. } => "reviewCompleted",
        RunEvent::RunCompleted { .. } => "runCompleted",
        RunEvent::RunFailed { .. } => "runFailed",
        RunEvent::RunCancelled { .. } => "runCancelled",
    };
    let payload = serde_json::to_string(&event).unwrap_or_default();
    let _ = ctx.db.add_event(&ctx.run_id, kind, &payload);
    let _ = ctx.app.emit("duet://run-event", event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentResult;
    use crate::models::Project;
    use async_trait::async_trait;
    use chrono::Utc;
    #[tokio::test]
    async fn completes_the_full_mock_workflow_in_an_isolated_worktree() {
        let source = tempfile::tempdir().unwrap();
        crate::git::tests_support::init_repo(source.path()).await;
        let inspection = git::inspect_repository(source.path()).await.unwrap();
        let data = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&data.path().join("test.sqlite3")).unwrap());
        let project = Project {
            id: "project".into(),
            name: "fixture".into(),
            path: inspection.path.clone(),
            language: inspection.language,
            build_system: inspection.build_system,
            test_command: "test -f DUET_MOCK_RESULT.md".into(),
            benchmark_command: String::new(),
            last_used_at: Utc::now().to_rfc3339(),
        };
        db.upsert_project(&project).unwrap();
        let run_id = "mock-workflow-run".to_string();
        db.create_run(
            &run_id,
            &project.id,
            "Add a documented mock result",
            &inspection.head_sha,
        )
        .unwrap();
        let app = tauri::test::mock_app();
        execute(WorkflowContext {
            app: app.handle().clone(),
            db: db.clone(),
            worktrees_root: data.path().join("worktrees"),
            run_id: run_id.clone(),
            request: StartRunRequest {
                project_id: project.id,
                task: "Add a documented mock result".into(),
                test_command: "test -f DUET_MOCK_RESULT.md".into(),
                benchmark_command: None,
                max_repairs: 3,
                mock_agents: true,
                agent_mode: "duet".into(),
                execution_location: "local".into(),
                codex_model: "gpt-5.6-sol".into(),
                claude_model: "sonnet".into(),
                codex_reasoning: "high".into(),
                claude_reasoning: "high".into(),
            },
            cancel: CancellationToken::new(),
        })
        .await
        .unwrap();
        let result = db.get_run(&run_id).unwrap();
        assert_eq!(result.run.status, "completed");
        assert!(result.verification.iter().all(|v| v.success));
        assert!(result
            .changed_files
            .iter()
            .any(|f| f.path == "DUET_MOCK_RESULT.md"));
        assert!(!source.path().join("DUET_MOCK_RESULT.md").exists());
    }

    struct MutatingReviewer;

    #[async_trait]
    impl Agent for MutatingReviewer {
        fn name(&self) -> &'static str {
            "Mutating reviewer"
        }

        async fn execute(
            &self,
            request: AgentRequest,
            _cancel: CancellationToken,
        ) -> Result<AgentResult> {
            std::fs::write(request.worktree.join("late-mutation.txt"), "not verified\n")?;
            let output = r#"{"verdict":"pass","summary":"looks good","issues":[]}"#.to_string();
            Ok(AgentResult {
                success: true,
                normalized_output: output.clone(),
                raw_output: output,
                stderr: String::new(),
                exit_code: Some(0),
                duration_ms: 1,
            })
        }
    }

    #[tokio::test]
    async fn rejects_a_worktree_mutated_during_review() {
        let source = tempfile::tempdir().unwrap();
        crate::git::tests_support::init_repo(source.path()).await;
        let inspection = git::inspect_repository(source.path()).await.unwrap();
        let data = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&data.path().join("test.sqlite3")).unwrap());
        let project = Project {
            id: "project".into(),
            name: "fixture".into(),
            path: inspection.path.clone(),
            language: inspection.language,
            build_system: inspection.build_system,
            test_command: "true".into(),
            benchmark_command: String::new(),
            last_used_at: Utc::now().to_rfc3339(),
        };
        db.upsert_project(&project).unwrap();
        db.create_run("review-race", &project.id, "task", &inspection.head_sha)
            .unwrap();
        let (worktree, branch) = git::create_worktree(
            source.path(),
            &data.path().join("worktrees"),
            "review-race",
            &inspection.head_sha,
        )
        .await
        .unwrap();
        std::fs::write(worktree.join("reviewed.txt"), "verified\n").unwrap();
        db.set_run_worktree("review-race", &branch, &worktree.to_string_lossy())
            .unwrap();
        let app = tauri::test::mock_app();
        let context = WorkflowContext {
            app: app.handle().clone(),
            db,
            worktrees_root: data.path().join("worktrees"),
            run_id: "review-race".into(),
            request: StartRunRequest {
                project_id: project.id,
                task: "task".into(),
                test_command: "true".into(),
                benchmark_command: None,
                max_repairs: 1,
                mock_agents: true,
                agent_mode: "duet".into(),
                execution_location: "local".into(),
                codex_model: "gpt-5.6-sol".into(),
                claude_model: "sonnet".into(),
                codex_reasoning: "high".into(),
                claude_reasoning: "high".into(),
            },
            cancel: CancellationToken::new(),
        };

        let error = perform_review_snapshot(
            &context,
            &MutatingReviewer,
            &worktree,
            &inspection.head_sha,
            "{}",
            &[],
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("changed during review"));
    }
}
