use crate::{models::{ArchitecturePlan, ReviewResult}, process::{run_process, OutputCallback, ProcessRequest}};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy)]
pub enum AgentRole { Architect, Implementer, Reviewer, Repair }

pub struct AgentRequest {
    pub role: AgentRole,
    pub prompt: String,
    pub worktree: PathBuf,
    pub timeout: Duration,
    pub callback: OutputCallback,
}

pub struct AgentResult {
    pub success: bool,
    pub normalized_output: String,
    pub raw_output: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &'static str;
    async fn execute(&self, request: AgentRequest, cancel: CancellationToken) -> Result<AgentResult>;
}

pub struct CodexAgent { pub binary: PathBuf }
pub struct ClaudeAgent { pub binary: PathBuf }

#[async_trait]
impl Agent for CodexAgent {
    fn name(&self) -> &'static str { "Codex" }
    async fn execute(&self, request: AgentRequest, cancel: CancellationToken) -> Result<AgentResult> {
        let sandbox = if matches!(request.role, AgentRole::Architect | AgentRole::Reviewer) { "read-only" } else { "workspace-write" };
        let output = run_process(ProcessRequest {
            program: self.binary.to_string_lossy().into(),
            args: vec!["exec".into(), "--json".into(), "--skip-git-repo-check".into(), "--sandbox".into(), sandbox.into(), "-C".into(), request.worktree.to_string_lossy().into(), request.prompt],
            cwd: request.worktree, timeout: request.timeout, env: vec![],
        }, cancel, request.callback).await?;
        let normalized = normalize_jsonl(&output.stdout);
        Ok(AgentResult { success: output.success, normalized_output: normalized, raw_output: output.stdout, stderr: output.stderr, exit_code: output.exit_code, duration_ms: output.duration_ms })
    }
}

#[async_trait]
impl Agent for ClaudeAgent {
    fn name(&self) -> &'static str { "Claude" }
    async fn execute(&self, request: AgentRequest, cancel: CancellationToken) -> Result<AgentResult> {
        let mut args = vec!["-p".into(), request.prompt, "--output-format".into(), "stream-json".into(), "--verbose".into(), "--add-dir".into(), request.worktree.to_string_lossy().into()];
        if matches!(request.role, AgentRole::Implementer | AgentRole::Repair) { args.extend(["--permission-mode".into(), "acceptEdits".into()]); }
        let output = run_process(ProcessRequest { program:self.binary.to_string_lossy().into(), args, cwd:request.worktree, timeout:request.timeout, env:vec![] }, cancel, request.callback).await?;
        let normalized = normalize_jsonl(&output.stdout);
        Ok(AgentResult { success:output.success, normalized_output:normalized, raw_output:output.stdout, stderr:output.stderr, exit_code:output.exit_code, duration_ms:output.duration_ms })
    }
}

pub struct MockAgent { pub agent_name: &'static str }

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &'static str { self.agent_name }
    async fn execute(&self, request: AgentRequest, cancel: CancellationToken) -> Result<AgentResult> {
        if cancel.is_cancelled() { return Err(anyhow!("process cancelled")); }
        tokio::time::sleep(Duration::from_millis(350)).await;
        let output: String = match request.role {
            AgentRole::Architect => r#"{"goal":"Complete the requested change","summary":"Inspect existing conventions, implement the smallest compatible change, and cover it with tests.","files_to_modify":[],"files_to_add":[],"implementation_steps":["Inspect relevant modules","Implement behavior","Add tests","Run verification"],"risks":["Behavioral regression"],"tests_required":["Existing test suite","New regression test"]}"#.into(),
            AgentRole::Reviewer => r#"{"verdict":"pass","summary":"Objective checks pass and no material defects were found.","issues":[]}"#.into(),
            AgentRole::Implementer => {
                tokio::fs::write(request.worktree.join("DUET_MOCK_RESULT.md"), "# Duet mock implementation\n\nThis file proves isolated worktree editing without agent usage.\n").await?;
                "Mock implementation completed in the isolated worktree.".into()
            },
            AgentRole::Repair => "Mock repair completed.".into(),
        };
        (request.callback)("stdout", &output);
        Ok(AgentResult { success:true, normalized_output:output.clone(), raw_output:output, stderr:String::new(), exit_code:Some(0), duration_ms:350 })
    }
}

pub fn parse_architecture(text: &str) -> Result<ArchitecturePlan> { parse_json_object(text).ok_or_else(|| anyhow!("Codex did not return a valid architecture plan")) }
pub fn parse_review(text: &str) -> Result<ReviewResult> { parse_json_object(text).ok_or_else(|| anyhow!("Codex did not return a valid review")) }

fn parse_json_object<T: DeserializeOwned>(text: &str) -> Option<T> {
    if let Ok(value) = serde_json::from_str(text.trim()) { return Some(value); }
    for line in text.lines().rev() {
        if let Ok(value) = serde_json::from_str(line.trim()) { return Some(value); }
    }
    let bytes = text.as_bytes();
    for start in (0..bytes.len()).filter(|i| bytes[*i] == b'{') {
        let mut depth = 0i32; let mut quoted = false; let mut escaped = false;
        for end in start..bytes.len() {
            let b = bytes[end];
            if quoted { if escaped { escaped=false; } else if b == b'\\' { escaped=true; } else if b == b'"' { quoted=false; } continue; }
            if b == b'"' { quoted=true; } else if b == b'{' { depth+=1; } else if b == b'}' { depth-=1; if depth==0 { if let Ok(value)=serde_json::from_slice(&bytes[start..=end]) { return Some(value); } break; } }
        }
    }
    None
}

fn normalize_jsonl(raw: &str) -> String {
    let mut pieces = Vec::new();
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { if !line.trim().is_empty() { pieces.push(line.to_string()); } continue };
        collect_strings(&value, &mut pieces);
    }
    if pieces.is_empty() { raw.to_string() } else { pieces.join("\n") }
}

fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for key in ["result", "text", "output_text", "message"] {
                if let Some(serde_json::Value::String(s)) = map.get(key) { if !s.is_empty() && !out.contains(s) { out.push(s.clone()); } }
            }
            for value in map.values() { if value.is_array() || value.is_object() { collect_strings(value, out); } }
        },
        serde_json::Value::Array(items) => for item in items { collect_strings(item, out); },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_json_from_markdown_noise() {
        let result: ReviewResult = parse_json_object("text\n```json\n{\"verdict\":\"pass\",\"summary\":\"ok\",\"issues\":[]}\n```").unwrap();
        assert_eq!(result.verdict, "pass");
    }
    #[test]
    fn survives_malformed_jsonl() { assert!(normalize_jsonl("not json\n{\"result\":\"yes\"}").contains("yes")); }
}
