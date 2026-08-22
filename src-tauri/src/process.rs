use anyhow::{anyhow, Context, Result};
use std::{path::PathBuf, process::Stdio, sync::Arc, time::{Duration, Instant}};
use tokio::{io::{AsyncBufReadExt, BufReader}, process::Command, sync::mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ProcessRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

pub type OutputCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub async fn run_process(request: ProcessRequest, cancel: CancellationToken, callback: OutputCallback) -> Result<ProcessOutput> {
    let started = Instant::now();
    let mut command = Command::new(&request.program);
    command.args(&request.args).current_dir(&request.cwd).envs(request.env.iter().cloned())
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = command.spawn().with_context(|| format!("launch {}", request.program))?;
    let stdout = child.stdout.take().context("capture stdout")?;
    let stderr = child.stderr.take().context("capture stderr")?;
    let (tx, mut rx) = mpsc::unbounded_channel::<(&'static str, String)>();
    let out_tx = tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await { let _ = out_tx.send(("stdout", line)); }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await { let _ = tx.send(("stderr", line)); }
    });

    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    let deadline = tokio::time::sleep(request.timeout);
    tokio::pin!(deadline);
    let status = loop {
        tokio::select! {
            line = rx.recv() => if let Some((stream, line)) = line {
                callback(stream, &line);
                let target = if stream == "stdout" { &mut stdout_text } else { &mut stderr_text };
                if target.len() < 1_000_000 { target.push_str(&line); target.push('\n'); }
            },
            result = child.wait() => break result.context("wait for child")?,
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(anyhow!("process cancelled"));
            },
            _ = &mut deadline => {
                let _ = child.kill().await;
                return Err(anyhow!("process timed out after {} seconds", request.timeout.as_secs()));
            }
        }
    };
    while let Ok((stream, line)) = rx.try_recv() {
        callback(stream, &line);
        let target = if stream == "stdout" { &mut stdout_text } else { &mut stderr_text };
        if target.len() < 1_000_000 { target.push_str(&line); target.push('\n'); }
    }
    Ok(ProcessOutput { success: status.success(), exit_code: status.code(), stdout: stdout_text, stderr: stderr_text, duration_ms: started.elapsed().as_millis() as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_both_streams() {
        let out = run_process(ProcessRequest { program:"sh".into(),args:vec!["-c".into(),"echo yes; echo no >&2".into()],cwd:std::env::temp_dir(),timeout:Duration::from_secs(2),env:vec![] }, CancellationToken::new(), Arc::new(|_,_|{})).await.unwrap();
        assert!(out.success); assert!(out.stdout.contains("yes")); assert!(out.stderr.contains("no"));
    }

    #[tokio::test]
    async fn enforces_timeout() {
        let result = run_process(ProcessRequest { program:"sh".into(),args:vec!["-c".into(),"sleep 2".into()],cwd:std::env::temp_dir(),timeout:Duration::from_millis(20),env:vec![] }, CancellationToken::new(), Arc::new(|_,_|{})).await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }
}
