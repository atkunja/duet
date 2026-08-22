use crate::{
    agents::{parse_architecture, parse_review, Agent, AgentRequest, AgentRole, ClaudeAgent, CodexAgent, MockAgent},
    db::Database, git, graph::{self, TaskStatus}, models::{RunEvent, StartRunRequest, VerificationResult}, prompts,
    process::OutputCallback, tooling::resolve_binary, verification::{self, VerificationItem},
};
use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use std::{path::{Path, PathBuf}, sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

pub struct WorkflowContext {
    pub app: AppHandle,
    pub db: Arc<Database>,
    pub worktrees_root: PathBuf,
    pub run_id: String,
    pub request: StartRunRequest,
    pub cancel: CancellationToken,
}

pub async fn execute(ctx: WorkflowContext) -> Result<()> {
    let mut task_graph = graph::default_workflow();
    task_graph.validate()?;
    let project = ctx.db.get_project(&ctx.request.project_id)?;
    let repo = PathBuf::from(&project.path);
    let inspection = git::inspect_repository(&repo).await?;
    task_graph.set_status("inspect", TaskStatus::Completed)?;
    let _ready_nodes = task_graph.ready();
    emit(&ctx, RunEvent::RunStarted { run_id:ctx.run_id.clone(), task:ctx.request.task.clone() });

    ctx.db.set_run_stage(&ctx.run_id, "preparing")?;
    let (worktree, branch) = git::create_worktree(&repo, &ctx.worktrees_root, &ctx.run_id, &inspection.head_sha).await.context("create isolated Git worktree")?;
    ctx.db.set_run_worktree(&ctx.run_id, &branch, &worktree.to_string_lossy())?;

    let codex_path = resolve_binary("codex").context("Codex CLI was not found in PATH or standard local install directories")?;
    let claude_path = resolve_binary("claude").context("Claude Code was not found in PATH or standard local install directories")?;
    let codex: Box<dyn Agent> = if ctx.request.mock_agents { Box::new(MockAgent{agent_name:"Codex"}) } else { Box::new(CodexAgent{binary:codex_path}) };
    let claude: Box<dyn Agent> = if ctx.request.mock_agents { Box::new(MockAgent{agent_name:"Claude"}) } else { Box::new(ClaudeAgent{binary:claude_path}) };

    let overview = format!("Path: {}\nBranch: {}\nBase SHA: {}\nLanguage: {}\nBuild system: {}\nDirty source tree: {}", worktree.display(), inspection.branch, inspection.head_sha, inspection.language, inspection.build_system, inspection.dirty);
    let architecture_result = run_agent(&ctx, codex.as_ref(), AgentRole::Architect, "architect", &worktree, prompts::architect(&ctx.request.task, &overview)).await?;
    if !architecture_result.success { return Err(anyhow!("Codex architecture failed: {}", architecture_result.stderr)); }
    let architecture = parse_architecture(&architecture_result.normalized_output)?;
    let architecture_json = serde_json::to_string_pretty(&architecture)?;
    ctx.db.set_architecture(&ctx.run_id, &architecture_json)?;

    let implementation = run_agent(&ctx, claude.as_ref(), AgentRole::Implementer, "build", &worktree, prompts::implementer(&ctx.request.task, &architecture_json, &ctx.request.test_command)).await?;
    if !implementation.success { return Err(anyhow!("Claude implementation failed: {}", implementation.stderr)); }
    refresh_changes(&ctx, &worktree).await?;

    let mut verification_results = run_verification(&ctx, &worktree).await?;
    let mut review = perform_review(&ctx, codex.as_ref(), &worktree, &architecture_json, &verification_results).await?;
    let mut last_diff = git::diff(&worktree).await.unwrap_or_default();
    let mut unchanged_repairs = 0u8;
    let mut round = 0u8;

    while (!required_checks_pass(&verification_results) || review.verdict != "pass") && round < ctx.request.max_repairs {
        if ctx.cancel.is_cancelled() { return Err(anyhow!("run cancelled")); }
        round += 1;
        let verification_text = verification::summarize(&verification_results);
        let review_json = serde_json::to_string_pretty(&review)?;
        let repair = run_agent(&ctx, claude.as_ref(), AgentRole::Repair, &format!("repair-{round}"), &worktree, prompts::repair(&ctx.request.task, &architecture_json, &verification_text, &review_json, round)).await?;
        if !repair.success { return Err(anyhow!("Claude repair round {round} failed: {}", repair.stderr)); }
        let new_diff = git::diff(&worktree).await.unwrap_or_default();
        if new_diff == last_diff { unchanged_repairs += 1; } else { unchanged_repairs = 0; }
        last_diff = new_diff;
        refresh_changes(&ctx, &worktree).await?;
        verification_results = run_verification(&ctx, &worktree).await?;
        review = perform_review(&ctx, codex.as_ref(), &worktree, &architecture_json, &verification_results).await?;
        if unchanged_repairs >= 2 { break; }
    }

    let verified = required_checks_pass(&verification_results) && review.verdict == "pass";
    ctx.db.complete_run(&ctx.run_id, if verified{"completed"}else{"failed"}, if verified{None}else{Some("verification or review did not pass within the repair limit")})?;
    if verified { emit(&ctx, RunEvent::RunCompleted{run_id:ctx.run_id.clone(),verified:true}); Ok(()) }
    else { emit(&ctx, RunEvent::RunFailed{run_id:ctx.run_id.clone(),reason:"Repair limit reached with unresolved failures".into()}); Err(anyhow!("repair limit reached with unresolved failures")) }
}

async fn run_agent(ctx:&WorkflowContext, agent:&dyn Agent, role:AgentRole, stage:&str, worktree:&Path, prompt:String) -> Result<crate::agents::AgentResult> {
    ctx.db.set_run_stage(&ctx.run_id, stage)?;
    let stage_id = ctx.db.start_stage(&ctx.run_id, stage, agent.name())?;
    emit(ctx, RunEvent::StageStarted{run_id:ctx.run_id.clone(),stage:stage.into(),agent:agent.name().into()});
    let callback = output_callback(ctx, stage);
    let result = agent.execute(AgentRequest{role,prompt,worktree:worktree.into(),timeout:Duration::from_secs(60*45),callback},ctx.cancel.clone()).await;
    match result {
        Ok(result) => {
            let summary=if result.success{"Completed".to_string()}else{format!("Agent exited with status {:?}",result.exit_code)};
            ctx.db.finish_stage(stage_id,result.success,&summary,&format!("{}\n{}",result.raw_output,result.stderr),result.duration_ms)?;
            emit(ctx,RunEvent::StageCompleted{run_id:ctx.run_id.clone(),stage:stage.into(),success:result.success,summary:if result.success{"Completed".into()}else{"Agent failed".into()}});
            Ok(result)
        },
        Err(error) => { ctx.db.finish_stage(stage_id,false,&error.to_string(),"",0)?; Err(error) }
    }
}

async fn run_verification(ctx:&WorkflowContext, worktree:&Path) -> Result<Vec<VerificationResult>> {
    ctx.db.set_run_stage(&ctx.run_id,"verify")?;
    let stage_id=ctx.db.start_stage(&ctx.run_id,"verify","Duet")?;
    emit(ctx,RunEvent::StageStarted{run_id:ctx.run_id.clone(),stage:"verify".into(),agent:"Duet".into()});
    let mut items=Vec::new();
    if !ctx.request.test_command.trim().is_empty() { items.push(VerificationItem{name:"Tests".into(),command:ctx.request.test_command.clone(),timeout:Duration::from_secs(20*60),required:true}); }
    if let Some(command)=ctx.request.benchmark_command.as_ref().filter(|s|!s.trim().is_empty()) { items.push(VerificationItem{name:"Benchmark".into(),command:command.clone(),timeout:Duration::from_secs(30*60),required:false}); }
    if items.is_empty() {
        let result=VerificationResult{name:"Tests".into(),command:"Not configured".into(),success:true,exit_code:None,stdout:"No test command configured; review remains required.".into(),stderr:String::new(),duration_ms:0,required:false};
        ctx.db.add_verification(&ctx.run_id,&result)?; ctx.db.finish_stage(stage_id,true,"No required command configured",&result.stdout,0)?; emit(ctx,RunEvent::VerificationCompleted{run_id:ctx.run_id.clone(),result:result.clone()}); return Ok(vec![result]);
    }
    let futures=items.into_iter().map(|item| verification::execute(item,worktree,ctx.cancel.clone(),output_callback(ctx,"verify")));
    let mut results=Vec::new();
    for result in join_all(futures).await { let result=result?; ctx.db.add_verification(&ctx.run_id,&result)?; emit(ctx,RunEvent::VerificationCompleted{run_id:ctx.run_id.clone(),result:result.clone()}); results.push(result); }
    let summary=verification::summarize(&results);ctx.db.finish_stage(stage_id,required_checks_pass(&results),&summary,&summary,results.iter().map(|r|r.duration_ms).max().unwrap_or(0))?;
    emit(ctx,RunEvent::StageCompleted{run_id:ctx.run_id.clone(),stage:"verify".into(),success:required_checks_pass(&results),summary});
    Ok(results)
}

async fn perform_review(ctx:&WorkflowContext, codex:&dyn Agent, worktree:&Path, architecture:&str, verification_results:&[VerificationResult]) -> Result<crate::models::ReviewResult> {
    let full_diff=git::diff(worktree).await?;
    let diff=if full_diff.len()>300_000 { format!("{}\n…[diff truncated]",&full_diff[..full_diff.floor_char_boundary(300_000)]) } else { full_diff };
    let result=run_agent(ctx,codex,AgentRole::Reviewer,"review",worktree,prompts::reviewer(&ctx.request.task,architecture,&diff,&verification::summarize(verification_results))).await?;
    if !result.success{return Err(anyhow!("Codex review failed: {}",result.stderr))}
    let review=parse_review(&result.normalized_output)?; let review_json=serde_json::to_string_pretty(&review)?; ctx.db.set_review(&ctx.run_id,&review_json)?;
    emit(ctx,RunEvent::ReviewCompleted{run_id:ctx.run_id.clone(),verdict:review.verdict.clone(),issues:review.issues.len()}); Ok(review)
}

async fn refresh_changes(ctx:&WorkflowContext,worktree:&Path)->Result<()> { let files=git::changed_files(worktree).await?; for file in &files{emit(ctx,RunEvent::FileChanged{run_id:ctx.run_id.clone(),path:file.path.clone()});} ctx.db.replace_changed_files(&ctx.run_id,&files)?; Ok(()) }
fn required_checks_pass(results:&[VerificationResult])->bool { results.iter().all(|r|!r.required||r.success) }
fn output_callback(ctx:&WorkflowContext,stage:&str)->OutputCallback { let app=ctx.app.clone();let db=ctx.db.clone();let run_id=ctx.run_id.clone();let stage=stage.to_string();Arc::new(move|stream,line|{let event=RunEvent::AgentOutput{run_id:run_id.clone(),stage:stage.clone(),stream:stream.into(),line:line.into()};let _=db.add_event(&run_id,"agentOutput",&serde_json::to_string(&event).unwrap_or_default());let _=app.emit("duet://run-event",event);}) }
fn emit(ctx:&WorkflowContext,event:RunEvent){let kind=match &event{RunEvent::RunStarted{..}=>"runStarted",RunEvent::StageStarted{..}=>"stageStarted",RunEvent::AgentOutput{..}=>"agentOutput",RunEvent::StageCompleted{..}=>"stageCompleted",RunEvent::FileChanged{..}=>"fileChanged",RunEvent::VerificationCompleted{..}=>"verificationCompleted",RunEvent::ReviewCompleted{..}=>"reviewCompleted",RunEvent::RunCompleted{..}=>"runCompleted",RunEvent::RunFailed{..}=>"runFailed",RunEvent::RunCancelled{..}=>"runCancelled"};let payload=serde_json::to_string(&event).unwrap_or_default();let _=ctx.db.add_event(&ctx.run_id,kind,&payload);let _=ctx.app.emit("duet://run-event",event);}
