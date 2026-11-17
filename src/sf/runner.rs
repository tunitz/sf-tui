//! Process execution with streamed output, cancellation and JSON capture.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::detect::SfInstallation;

/// Which stream produced a line of output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// One line streamed from a running SF command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLine {
    pub job_id: u64,
    pub stream: OutputStream,
    pub text: String,
}

/// Everything a background process reports to the application.
#[derive(Debug)]
pub enum SfEvent {
    /// A single streamed line (kept for callers that do not batch).
    Line(JobLine),
    /// Several streamed lines coalesced into one event to spare the event loop.
    Lines(Vec<JobLine>),
    Finished {
        job_id: u64,
        outcome: ProcessOutcome,
    },
}

/// Upper bound for retained stdout/stderr per process. The UI also caps streamed lines.
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STREAM_LINE_CHARS: usize = 4_000;
/// Flush a partially filled line batch when it reaches this size or this long has elapsed, so
/// many CLI lines become one event without making live output wait for the process to end.
const LINE_BATCH_SIZE: usize = 64;
const LINE_BATCH_INTERVAL: Duration = Duration::from_millis(40);

/// Captured result of a finished process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutcome {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ProcessOutcome {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Errors from starting or probing an SF process.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("could not start {command}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("command timed out after {seconds}s")]
    TimedOut { seconds: u64 },
    #[error("command failed: {0}")]
    Io(#[from] std::io::Error),
}

/// A tracked background job: dropping the cancel sender does not kill it, using it does.
pub struct RunningJob {
    pub id: u64,
    cancel: oneshot::Sender<()>,
    task: JoinHandle<ProcessOutcome>,
}

impl RunningJob {
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Request cooperative termination. Returns false if the job already finished.
    pub fn cancel(self) -> bool {
        self.cancel.send(()).is_ok()
    }

    /// Request termination and wait for the capture task to finish.
    pub async fn cancel_and_join(self) -> ProcessOutcome {
        let RunningJob { cancel, task, .. } = self;
        let _ = cancel.send(());
        task.await.unwrap_or(ProcessOutcome {
            exit_code: None,
            stdout: String::new(),
            stderr: "process task was cancelled".to_owned(),
        })
    }

    /// Await the underlying task, e.g. during application shutdown.
    pub async fn join(self) -> ProcessOutcome {
        // The task is owned and always sends a final outcome, so a cancelled join is not fatal.
        self.task.await.unwrap_or(ProcessOutcome {
            exit_code: None,
            stdout: String::new(),
            stderr: "process task was cancelled".to_owned(),
        })
    }
}

/// Environment settings that stop the Salesforce CLI doing network/update work on every call.
/// This matters on Windows where each Node-based `sf` process is expensive to start.
pub fn sf_env() -> &'static [(&'static str, &'static str)] {
    &[
        ("SF_SKIP_NEW_VERSION_CHECK", "true"),
        ("SF_DISABLE_TELEMETRY", "true"),
        ("SF_DISABLE_AUTOUPDATE", "true"),
    ]
}

/// Spawn an SF command, streaming every stdout/stderr line to `events`.
pub fn spawn_sf(
    installation: &SfInstallation,
    argv: &[String],
    job_id: u64,
    events: Sender<SfEvent>,
) -> Result<RunningJob, RunnerError> {
    spawn_sf_mapped(installation, argv, job_id, events, std::convert::identity)
}

/// Spawn an SF command and map its events into the caller's event type.
pub fn spawn_sf_mapped<E, F>(
    installation: &SfInstallation,
    argv: &[String],
    job_id: u64,
    events: Sender<E>,
    map: F,
) -> Result<RunningJob, RunnerError>
where
    E: Send + 'static,
    F: Fn(SfEvent) -> E + Send + Clone + 'static,
{
    let argv = installation.argv(argv);
    spawn_with_env(
        installation.executable.as_path(),
        &argv,
        job_id,
        events,
        map,
        sf_env(),
    )
}

/// Spawn an arbitrary executable, streaming every stdout/stderr line to `events`.
pub fn spawn<E, F>(
    executable: &Path,
    argv: &[String],
    job_id: u64,
    events: Sender<E>,
    map: F,
) -> Result<RunningJob, RunnerError>
where
    E: Send + 'static,
    F: Fn(SfEvent) -> E + Send + Clone + 'static,
{
    spawn_with_env(executable, argv, job_id, events, map, &[])
}

pub fn spawn_with_env<E, F>(
    executable: &Path,
    argv: &[String],
    job_id: u64,
    events: Sender<E>,
    map: F,
    env: &[(&str, &str)],
) -> Result<RunningJob, RunnerError>
where
    E: Send + 'static,
    F: Fn(SfEvent) -> E + Send + Clone + 'static,
{
    let mut command = Command::new(executable);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|source| RunnerError::Spawn {
        command: format!("{} {}", executable.display(), argv.join(" ")),
        source,
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = stdout.map(|pipe| {
        tokio::spawn(read_stream(
            pipe,
            job_id,
            OutputStream::Stdout,
            events.clone(),
            map.clone(),
        ))
    });
    let stderr_task = stderr.map(|pipe| {
        tokio::spawn(read_stream(
            pipe,
            job_id,
            OutputStream::Stderr,
            events.clone(),
            map.clone(),
        ))
    });
    let (cancel, cancelled) = oneshot::channel::<()>();
    let task: JoinHandle<ProcessOutcome> = tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status.ok(),
            _ = cancelled => {
                let _ = child.kill().await;
                child.wait().await.ok()
            }
        };
        let (out, err) = tokio::join!(join_optional(stdout_task), join_optional(stderr_task),);
        let outcome = ProcessOutcome {
            exit_code: status.and_then(|status| status.code()),
            stdout: out,
            stderr: err,
        };
        let _ = events
            .send(map(SfEvent::Finished {
                job_id,
                outcome: outcome.clone(),
            }))
            .await;
        outcome
    });
    Ok(RunningJob {
        id: job_id,
        cancel,
        task,
    })
}

async fn join_optional(task: Option<JoinHandle<String>>) -> String {
    match task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    }
}

async fn read_stream<R, E, F>(
    pipe: R,
    job_id: u64,
    stream: OutputStream,
    events: Sender<E>,
    map: F,
) -> String
where
    R: AsyncRead + Unpin + Send + 'static,
    E: Send + 'static,
    F: Fn(SfEvent) -> E,
{
    let mut captured = String::new();
    let mut lines = BufReader::new(pipe).lines();
    let mut batch: Vec<JobLine> = Vec::with_capacity(LINE_BATCH_SIZE);
    let mut flush = tokio::time::interval(LINE_BATCH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first interval tick completes immediately; consume it before entering the loop.
    flush.tick().await;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else {
                    break;
                };
                if captured.len() < MAX_CAPTURE_BYTES {
                    captured.push_str(&line);
                    captured.push('\n');
                }
                let text = if line.chars().count() > MAX_STREAM_LINE_CHARS {
                    let mut shortened: String = line.chars().take(MAX_STREAM_LINE_CHARS).collect();
                    shortened.push('…');
                    shortened
                } else {
                    line
                };
                batch.push(JobLine {
                    job_id,
                    stream,
                    text,
                });
                if batch.len() >= LINE_BATCH_SIZE {
                    let full = std::mem::replace(&mut batch, Vec::with_capacity(LINE_BATCH_SIZE));
                    if events.send(map(SfEvent::Lines(full))).await.is_err() {
                        break;
                    }
                }
            }
            _ = flush.tick() => {
                if !batch.is_empty() && events.send(map(SfEvent::Lines(std::mem::take(&mut batch)))).await.is_err() {
                    break;
                }
            }
        }
    }
    // Do not lose the trailing partial batch on a clean stream end.
    if !batch.is_empty() {
        let _ = events.send(map(SfEvent::Lines(batch))).await;
    }
    captured
}

/// Run a short probe command to completion with a hard timeout.
pub async fn capture(
    executable: &Path,
    argv: &[String],
    timeout: Duration,
) -> Result<ProcessOutcome, RunnerError> {
    capture_with_env(executable, argv, timeout, &[]).await
}

/// Run a short probe with explicit child-process environment settings and a hard timeout.
pub async fn capture_with_env(
    executable: &Path,
    argv: &[String],
    timeout: Duration,
    env: &[(&str, &str)],
) -> Result<ProcessOutcome, RunnerError> {
    let mut command = Command::new(executable);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|source| RunnerError::Spawn {
        command: format!("{} {}", executable.display(), argv.join(" ")),
        source,
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_task = stdout.map(|mut pipe| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            use tokio::io::AsyncReadExt;
            let _ = pipe.read_to_end(&mut bytes).await;
            String::from_utf8_lossy(&bytes).into_owned()
        })
    });
    let err_task = stderr.map(|mut pipe| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            use tokio::io::AsyncReadExt;
            let _ = pipe.read_to_end(&mut bytes).await;
            String::from_utf8_lossy(&bytes).into_owned()
        })
    });
    let status = tokio::time::timeout(timeout, child.wait()).await;
    let status = match status {
        Ok(result) => result.map_err(RunnerError::Io)?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(RunnerError::TimedOut {
                seconds: timeout.as_secs(),
            });
        }
    };
    let (out, err) = tokio::join!(join_optional(out_task), join_optional(err_task));
    Ok(ProcessOutcome {
        exit_code: status.code(),
        stdout: out,
        stderr: err,
    })
}

/// Parse a JSON document embedded in text that may contain other CLI chatter.
pub fn parse_json_document(text: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str(text.trim()) {
        return Some(value);
    }
    // Try the first few `{`/`[` positions. A bracketed warning before the document used to make
    // the old min-start heuristic fail even when a valid object followed.
    for (index, _) in text
        .char_indices()
        .filter(|(_, character)| *character == '{' || *character == '[')
        .take(256)
    {
        let mut deserializer = serde_json::Deserializer::from_str(&text[index..]);
        if let Ok(value) = serde::Deserialize::deserialize(&mut deserializer) {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_document_is_extracted_from_noisy_output() {
        let text = "warning: deprecated flag\n{\"status\":0,\"result\":{\"ok\":true}}\n";
        let value = parse_json_document(text).expect("parse");
        assert_eq!(value["result"]["ok"], true);
    }

    #[test]
    fn json_document_survives_a_bracketed_warning() {
        let text = "[WARNING] plugin notice\n{\"status\":0,\"result\":{\"ok\":true}}\n";
        let value = parse_json_document(text).expect("parse");
        assert_eq!(value["result"]["ok"], true);
    }

    #[test]
    fn json_document_ignores_text_without_json() {
        assert!(parse_json_document("no json here").is_none());
    }

    #[tokio::test]
    async fn stream_lines_are_batched_in_order_without_loss() {
        let input = b"alpha\nbeta\ngamma\n".to_vec();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<SfEvent>(8);
        let captured = read_stream(
            std::io::Cursor::new(input),
            7,
            OutputStream::Stdout,
            tx,
            std::convert::identity,
        )
        .await;
        assert_eq!(captured, "alpha\nbeta\ngamma\n");
        let mut texts = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                SfEvent::Lines(lines) => {
                    assert!(lines.iter().all(|line| line.job_id == 7));
                    texts.extend(lines.into_iter().map(|line| line.text));
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(texts, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn outcome_success_requires_zero_exit_code() {
        assert!(
            ProcessOutcome {
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new()
            }
            .succeeded()
        );
        assert!(
            !ProcessOutcome {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new()
            }
            .succeeded()
        );
    }
}
