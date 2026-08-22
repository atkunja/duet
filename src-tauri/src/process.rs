use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ProcessRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
    pub capture_limit: usize,
    pub fail_on_output_limit: bool,
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

pub async fn run_process(
    request: ProcessRequest,
    cancel: CancellationToken,
    callback: OutputCallback,
) -> Result<ProcessOutput> {
    let started = Instant::now();
    let mut command = Command::new(&request.program);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .envs(request.env.iter().cloned())
        .stdin(if request.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("launch {}", request.program))?;
    let process_group = child.id();
    let stdin_task = request.stdin.map(|input| {
        let mut stdin = child.stdin.take().expect("piped stdin must be available");
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        })
    });
    let stdout = child.stdout.take().context("capture stdout")?;
    let stderr = child.stderr.take().context("capture stderr")?;
    let (tx, mut rx) = mpsc::channel::<(&'static str, String)>(512);
    let out_tx = tx.clone();
    let stdout_task = tokio::spawn(pump_stream(stdout, "stdout", out_tx));
    let stderr_task = tokio::spawn(pump_stream(stderr, "stderr", tx));

    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    let mut output_truncated = false;
    let deadline = tokio::time::sleep(request.timeout);
    tokio::pin!(deadline);
    enum Finish {
        Exited(std::process::ExitStatus),
        Cancelled,
        TimedOut,
    }
    let finish = loop {
        tokio::select! {
            Some((stream, line)) = rx.recv() => {
                output_truncated |= capture_chunk(stream,&line,&callback,&mut stdout_text,&mut stderr_text,request.capture_limit);
            },
            result = child.wait() => break Finish::Exited(result.context("wait for child")?),
            _ = cancel.cancelled() => break Finish::Cancelled,
            _ = &mut deadline => break Finish::TimedOut,
        }
    };
    if !matches!(finish, Finish::Exited(_)) {
        terminate_process_tree(&mut child, process_group).await;
        let _ = child.wait().await;
    }
    let mut readers = tokio::spawn(async move {
        if let Some(task) = stdin_task {
            let _ = task.await;
        }
        let _ = stdout_task.await;
        let _ = stderr_task.await;
    });
    let drain_deadline = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(drain_deadline);
    loop {
        tokio::select! {Some((stream,line))=rx.recv()=>output_truncated |= capture_chunk(stream,&line,&callback,&mut stdout_text,&mut stderr_text,request.capture_limit),_=&mut readers=>break,_=&mut drain_deadline=>{terminate_process_tree(&mut child,process_group).await;readers.abort();break}}
    }
    while let Ok((stream, line)) = rx.try_recv() {
        output_truncated |= capture_chunk(
            stream,
            &line,
            &callback,
            &mut stdout_text,
            &mut stderr_text,
            request.capture_limit,
        );
    }
    if matches!(finish, Finish::Exited(_)) {
        terminate_process_tree(&mut child, process_group).await;
    }
    if output_truncated && request.fail_on_output_limit {
        return Err(anyhow::anyhow!(
            "process output exceeded the {} byte capture limit",
            request.capture_limit
        ));
    }
    match finish {
        Finish::Exited(status) => Ok(ProcessOutput {
            success: status.success(),
            exit_code: status.code(),
            stdout: stdout_text,
            stderr: stderr_text,
            duration_ms: started.elapsed().as_millis() as u64,
        }),
        Finish::Cancelled => {
            stderr_text.push_str("\n[Duet] process cancelled\n");
            Ok(ProcessOutput {
                success: false,
                exit_code: None,
                stdout: stdout_text,
                stderr: stderr_text,
                duration_ms: started.elapsed().as_millis() as u64,
            })
        }
        Finish::TimedOut => {
            stderr_text.push_str(&format!(
                "\n[Duet] process timed out after {} seconds\n",
                request.timeout.as_secs()
            ));
            Ok(ProcessOutput {
                success: false,
                exit_code: None,
                stdout: stdout_text,
                stderr: stderr_text,
                duration_ms: started.elapsed().as_millis() as u64,
            })
        }
    }
}

async fn pump_stream<R: AsyncRead + Unpin>(
    mut reader: R,
    stream: &'static str,
    tx: mpsc::Sender<(&'static str, String)>,
) {
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let chunk = String::from_utf8_lossy(&buffer[..count]).into_owned();
                if tx.send((stream, chunk)).await.is_err() {
                    break;
                }
            }
        }
    }
}
fn capture_chunk(
    stream: &str,
    chunk: &str,
    callback: &OutputCallback,
    stdout: &mut String,
    stderr: &mut String,
    limit: usize,
) -> bool {
    callback(stream, chunk);
    bounded_append(
        if stream == "stdout" { stdout } else { stderr },
        chunk,
        limit,
    )
}
fn bounded_append(target: &mut String, chunk: &str, limit: usize) -> bool {
    const MARKER: &str = "[Duet truncated earlier output]\n";
    target.push_str(chunk);
    if target.len() > limit {
        let keep = limit.saturating_sub(MARKER.len());
        let start = target.ceil_char_boundary(target.len() - keep);
        let tail = target[start..].to_string();
        target.clear();
        target.push_str(MARKER);
        target.push_str(&tail);
        true
    } else {
        false
    }
}

async fn terminate_process_tree(child: &mut Child, process_group: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = process_group {
        unsafe {
            if libc::kill(-(pid as i32), 0) != 0 {
                return;
            }
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        return;
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_both_streams() {
        let out = run_process(
            ProcessRequest {
                program: "sh".into(),
                args: vec!["-c".into(), "echo yes; echo no >&2".into()],
                cwd: std::env::temp_dir(),
                timeout: Duration::from_secs(2),
                env: vec![],
                stdin: None,
                capture_limit: 1_000_000,
                fail_on_output_limit: false,
            },
            CancellationToken::new(),
            Arc::new(|_, _| {}),
        )
        .await
        .unwrap();
        assert!(out.success);
        assert!(out.stdout.contains("yes"));
        assert!(out.stderr.contains("no"));
    }

    #[tokio::test]
    async fn enforces_timeout() {
        let result = run_process(
            ProcessRequest {
                program: "sh".into(),
                args: vec!["-c".into(), "sleep 2".into()],
                cwd: std::env::temp_dir(),
                timeout: Duration::from_millis(20),
                env: vec![],
                stdin: None,
                capture_limit: 1_000_000,
                fail_on_output_limit: false,
            },
            CancellationToken::new(),
            Arc::new(|_, _| {}),
        )
        .await
        .unwrap();
        assert!(!result.success);
        assert!(result.stderr.contains("timed out"));
    }

    #[tokio::test]
    async fn writes_sensitive_input_through_stdin() {
        let out = run_process(
            ProcessRequest {
                program: "sh".into(),
                args: vec!["-c".into(), "cat".into()],
                cwd: std::env::temp_dir(),
                timeout: Duration::from_secs(2),
                env: vec![],
                stdin: Some("private prompt".into()),
                capture_limit: 1_000_000,
                fail_on_output_limit: false,
            },
            CancellationToken::new(),
            Arc::new(|_, _| {}),
        )
        .await
        .unwrap();
        assert_eq!(out.stdout.trim(), "private prompt");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_descendant_processes() {
        let descendant = Arc::new(parking_lot::Mutex::new(None));
        let observed = descendant.clone();
        let callback: OutputCallback = Arc::new(move |stream, line| {
            if stream == "stdout" {
                if let Ok(pid) = line.trim().parse::<u32>() {
                    *observed.lock() = Some(pid);
                }
            }
        });
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run_process(
                ProcessRequest {
                    program: "sh".into(),
                    args: vec!["-c".into(), "sleep 30 & echo $!; wait".into()],
                    cwd: std::env::temp_dir(),
                    timeout: Duration::from_secs(5),
                    env: vec![],
                    stdin: None,
                    capture_limit: 1_000_000,
                    fail_on_output_limit: false,
                },
                cancel_run,
                callback,
            )
            .await
        });
        for _ in 0..100 {
            if descendant.lock().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = descendant
            .lock()
            .expect("shell should report its child PID");
        cancel.cancel();
        let result = task.await.unwrap().unwrap();
        assert!(!result.success);
        assert!(result.stderr.contains("cancelled"));
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!alive, "descendant process {pid} survived cancellation");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_exit_cannot_leave_pipe_holding_descendants() {
        let started = Instant::now();
        let result = run_process(
            ProcessRequest {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    "sleep 30 >/dev/null 2>&1 & echo $!; exit 0".into(),
                ],
                cwd: std::env::temp_dir(),
                timeout: Duration::from_secs(10),
                env: vec![],
                stdin: None,
                capture_limit: 1_000_000,
                fail_on_output_limit: false,
            },
            CancellationToken::new(),
            Arc::new(|_, _| {}),
        )
        .await
        .unwrap();
        assert!(result.success);
        assert!(started.elapsed() < Duration::from_secs(4));
        let pid = result.stdout.trim();
        let alive = std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!alive, "detached descendant {pid} survived direct exit");
    }
    #[tokio::test]
    async fn bounded_capture_preserves_final_structured_output() {
        let mut captured = String::new();
        bounded_append(&mut captured, &"x".repeat(1_100_000), 1_000_000);
        bounded_append(&mut captured, "\n{\"verdict\":\"pass\"}\n", 1_000_000);
        assert!(captured.len() <= 1_000_000);
        assert!(captured.starts_with("[Duet truncated"));
        assert!(captured.ends_with("{\"verdict\":\"pass\"}\n"));
    }
}
