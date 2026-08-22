use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppPreferences {
    pub editor: String,
    pub max_repairs: u8,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            editor: "auto".into(),
            max_repairs: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub language: String,
    pub build_system: String,
    pub test_command: String,
    pub benchmark_command: String,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoInspection {
    pub path: String,
    pub branch: String,
    pub head_sha: String,
    pub dirty: bool,
    pub language: String,
    pub build_system: String,
    pub suggested_test_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRunRequest {
    pub project_id: String,
    pub task: String,
    pub test_command: String,
    pub benchmark_command: Option<String>,
    pub max_repairs: u8,
    #[serde(default)]
    pub parallel_verification: bool,
    #[serde(default)]
    pub mock_agents: bool,
    #[serde(default = "default_agent_mode")]
    pub agent_mode: String,
    #[serde(default = "default_execution_location")]
    pub execution_location: String,
    #[serde(default = "default_codex_model")]
    pub codex_model: String,
    #[serde(default = "default_claude_model")]
    pub claude_model: String,
    #[serde(default = "default_reasoning")]
    pub codex_reasoning: String,
    #[serde(default = "default_reasoning")]
    pub claude_reasoning: String,
}

fn default_agent_mode() -> String {
    "duet".into()
}
fn default_execution_location() -> String {
    "local".into()
}
fn default_codex_model() -> String {
    "gpt-5.6-sol".into()
}
fn default_claude_model() -> String {
    "sonnet".into()
}
fn default_reasoning() -> String {
    "high".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub task: String,
    pub status: String,
    pub current_stage: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub worktree_path: Option<String>,
    pub additions: i64,
    pub deletions: i64,
    pub applied_at: Option<String>,
    pub discarded_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageRecord {
    pub id: i64,
    pub run_id: String,
    pub kind: String,
    pub agent: String,
    pub status: String,
    pub summary: String,
    pub raw_output: String,
    pub normalized_output: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: RunSummary,
    pub stages: Vec<StageRecord>,
    pub architecture: Option<String>,
    pub review: Option<String>,
    pub verification: Vec<VerificationResult>,
    pub changed_files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: i64,
    pub deletions: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    pub name: String,
    pub command: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewIssue {
    pub severity: String,
    pub category: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub problem: String,
    pub reason: String,
    #[serde(alias = "suggested_fix")]
    pub suggested_fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResult {
    pub verdict: String,
    pub summary: String,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchitecturePlan {
    pub goal: String,
    #[serde(deserialize_with = "deserialize_plan_text")]
    pub summary: String,
    #[serde(
        default,
        alias = "files_to_modify",
        deserialize_with = "deserialize_plan_text_list"
    )]
    pub files_to_modify: Vec<String>,
    #[serde(
        default,
        alias = "files_to_add",
        deserialize_with = "deserialize_plan_text_list"
    )]
    pub files_to_add: Vec<String>,
    #[serde(
        default,
        alias = "implementation_steps",
        deserialize_with = "deserialize_plan_text_list"
    )]
    pub implementation_steps: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_plan_text_list")]
    pub risks: Vec<String>,
    #[serde(
        default,
        alias = "tests_required",
        deserialize_with = "deserialize_plan_text_list"
    )]
    pub tests_required: Vec<String>,
}

fn deserialize_plan_text<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(plan_value_text(&value))
}

fn deserialize_plan_text_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(plan_value_text)
            .filter(|item| !item.is_empty())
            .collect(),
        other => {
            let text = plan_value_text(&other);
            (!text.is_empty()).then_some(text).into_iter().collect()
        }
    })
}

fn plan_value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(plan_value_text)
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => {
            if let Some(path) = map.get("path").and_then(serde_json::Value::as_str) {
                let changes = map.get("changes").map(plan_value_text).unwrap_or_default();
                return if changes.is_empty() {
                    path.into()
                } else {
                    format!("{path}: {}", changes.replace('\n', "; "))
                };
            }
            if let Some(risk) = map.get("risk").and_then(serde_json::Value::as_str) {
                let mitigation = map
                    .get("mitigation")
                    .map(plan_value_text)
                    .unwrap_or_default();
                return if mitigation.is_empty() {
                    risk.into()
                } else {
                    format!("{risk} Mitigation: {mitigation}")
                };
            }
            serde_json::to_string(value).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub app_data_writable: bool,
    pub database_healthy: bool,
    pub git: ToolStatus,
    pub claude: ToolStatus,
    pub codex: ToolStatus,
    pub os: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub installed: bool,
    pub authenticated: Option<bool>,
    pub path: Option<String>,
    pub version: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunEvent {
    RunStarted {
        run_id: String,
        task: String,
    },
    StageStarted {
        run_id: String,
        stage: String,
        agent: String,
    },
    AgentOutput {
        run_id: String,
        stage: String,
        stream: String,
        line: String,
    },
    StageCompleted {
        run_id: String,
        stage: String,
        success: bool,
        summary: String,
    },
    FileChanged {
        run_id: String,
        path: String,
    },
    VerificationCompleted {
        run_id: String,
        result: VerificationResult,
    },
    ReviewCompleted {
        run_id: String,
        verdict: String,
        issues: usize,
    },
    RunCompleted {
        run_id: String,
        verified: bool,
    },
    RunFailed {
        run_id: String,
        reason: String,
    },
    RunCancelled {
        run_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::RunEvent;

    #[test]
    fn run_events_serialize_field_names_for_the_frontend() {
        let value = serde_json::to_value(RunEvent::RunStarted {
            run_id: "run-123".into(),
            task: "Fix the event contract".into(),
        })
        .unwrap();

        assert_eq!(value["type"], "runStarted");
        assert_eq!(value["runId"], "run-123");
        assert!(value.get("run_id").is_none());
    }
}
