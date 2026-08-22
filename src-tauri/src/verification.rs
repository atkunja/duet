use crate::{
    models::VerificationResult,
    process::{run_process, OutputCallback, ProcessRequest},
};
use anyhow::{anyhow, Result};
use std::{path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct VerificationItem {
    pub name: String,
    pub command: String,
    pub timeout: Duration,
    pub required: bool,
}

pub async fn execute(
    item: VerificationItem,
    cwd: &Path,
    cancel: CancellationToken,
    callback: OutputCallback,
) -> Result<VerificationResult> {
    let words = shell_words::split(&item.command)
        .map_err(|e| anyhow!("invalid verification command: {e}"))?;
    if words.is_empty() {
        return Err(anyhow!("verification command is empty"));
    }
    // Verification commands are explicit project configuration. They execute only in the Duet worktree.
    let output = run_process(
        ProcessRequest {
            program: "sh".into(),
            args: vec!["-lc".into(), item.command.clone()],
            cwd: cwd.into(),
            timeout: item.timeout,
            env: vec![("CI".into(), "1".into()), ("NO_COLOR".into(), "1".into())],
            stdin: None,
        },
        cancel,
        callback,
    )
    .await?;
    Ok(VerificationResult {
        name: item.name,
        command: item.command,
        success: output.success,
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        duration_ms: output.duration_ms,
        required: item.required,
    })
}

pub fn summarize(results: &[VerificationResult]) -> String {
    results
        .iter()
        .map(|r| {
            format!(
                "{}: {} (exit {:?}, {}ms)\n{}{}",
                r.name,
                if r.success { "PASS" } else { "FAIL" },
                r.exit_code,
                r.duration_ms,
                truncate(&r.stdout, 20_000),
                truncate(&r.stderr, 20_000)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.into()
    } else {
        format!("{}\n…[truncated]", &value[..value.floor_char_boundary(max)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[tokio::test]
    async fn reports_failed_checks_objectively() {
        let result = execute(
            VerificationItem {
                name: "Tests".into(),
                command: "exit 7".into(),
                timeout: Duration::from_secs(2),
                required: true,
            },
            &std::env::temp_dir(),
            CancellationToken::new(),
            Arc::new(|_, _| {}),
        )
        .await
        .unwrap();
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(7));
    }
}
