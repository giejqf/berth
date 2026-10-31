//! Spawn and supervise one sidecar process and speak NDJSON with it (AGENTS.md §10).
//!
//! The caller builds the [`tokio::process::Command`]: `node <bundle>` for host
//! placement, `slot.sh exec -- node <bundle>` for inside placement. The client
//! only owns the pipes.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use berth_protocol::{AppMessage, AppToSidecar, SidecarMessage};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

/// Lines of stderr kept for diagnostics when the process exits.
const STDERR_TAIL_LINES: usize = 50;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("failed to spawn sidecar: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("sidecar stdio unavailable")]
    NoStdio,
    #[error("sidecar has exited")]
    Closed,
    #[error("timed out waiting for reply to {0}")]
    Timeout(String),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Everything the supervisor reports, in order.
#[derive(Debug, Clone)]
pub enum SidecarEvent {
    Message(SidecarMessage),
    /// A stdout line that is not a valid protocol message.
    ProtocolError {
        line: String,
        error: String,
    },
    /// The process is gone. Always the last event.
    Exited {
        code: Option<i32>,
        stderr_tail: String,
    },
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<SidecarMessage>>>>;

/// Handle to a running sidecar. Dropping it kills the process.
pub struct Sidecar {
    tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    child: Arc<tokio::sync::Mutex<Child>>,
    pid: Option<u32>,
}

impl Sidecar {
    /// Spawn `cmd` with piped stdio and start supervising it.
    pub fn spawn(
        mut cmd: Command,
    ) -> Result<(Self, mpsc::UnboundedReceiver<SidecarEvent>), ClientError> {
        // Own process group, so `kill` also reaches `slot.sh exec` grandchildren.
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = cmd.spawn().map_err(ClientError::Spawn)?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or(ClientError::NoStdio)?;
        let stdout = child.stdout.take().ok_or(ClientError::NoStdio)?;
        let stderr = child.stderr.take().ok_or(ClientError::NoStdio)?;

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::default();
        let child = Arc::new(tokio::sync::Mutex::new(child));

        tokio::spawn(write_loop(stdin, line_rx));
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_done = tokio::spawn(stderr_loop(stderr, tail.clone()));
        tokio::spawn(read_loop(
            stdout,
            events_tx,
            pending.clone(),
            child.clone(),
            stderr_done,
            tail,
        ));

        tracing::info!(pid, "sidecar spawned");
        Ok((
            Self {
                tx: line_tx,
                pending,
                child,
                pid,
            },
            events_rx,
        ))
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Send a message; returns its id.
    pub fn send(&self, body: AppToSidecar) -> Result<String, ClientError> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send_with_id(&id, body)?;
        Ok(id)
    }

    fn send_with_id(&self, id: &str, body: AppToSidecar) -> Result<(), ClientError> {
        let kind = body.type_name();
        let line = serde_json::to_string(&AppMessage {
            id: id.to_owned(),
            body,
        })?;
        tracing::debug!(%id, kind, "-> sidecar");
        self.tx.send(line).map_err(|_| ClientError::Closed)
    }

    /// Send and wait for the message whose `reply_to` is this request's id.
    /// The reply is also delivered on the event stream.
    pub async fn request(
        &self,
        body: AppToSidecar,
        timeout: Duration,
    ) -> Result<SidecarMessage, ClientError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id.clone(), tx);
        if let Err(e) = self.send_with_id(&id, body) {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(_)) => Err(ClientError::Closed),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(ClientError::Timeout(id))
            }
        }
    }

    /// Ask the sidecar to stop, then kill it if it has not exited after `grace`.
    pub async fn shutdown(&self, grace: Duration) {
        let _ = self.send(AppToSidecar::SessionStop);
        let child = self.child.clone();
        let wait = async move {
            loop {
                if let Ok(Some(_)) = child.lock().await.try_wait() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        if tokio::time::timeout(grace, wait).await.is_err() {
            self.kill().await;
        }
    }

    /// Kill the sidecar's whole process group.
    pub async fn kill(&self) {
        if let Some(pid) = self.pid {
            let _ = tokio::process::Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        if let Err(e) = self.child.lock().await.start_kill() {
            tracing::debug!(error = %e, "sidecar kill");
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn write_loop(
    mut stdin: tokio::process::ChildStdin,
    mut lines: mpsc::UnboundedReceiver<String>,
) {
    while let Some(mut line) = lines.recv().await {
        line.push('\n');
        if let Err(e) = stdin.write_all(line.as_bytes()).await {
            tracing::warn!(error = %e, "sidecar stdin closed");
            return;
        }
        if stdin.flush().await.is_err() {
            return;
        }
    }
    // All senders dropped: closing stdin tells the sidecar to stop.
    let _ = stdin.shutdown().await;
}

/// The next line of `r`, without its newline, invalid UTF-8 replaced: a
/// stray byte must not stop the reading, or the pipe fills and the sidecar
/// blocks writing to it. `None` at the end, or when reading fails.
async fn next_line<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Option<String> {
    buf.clear();
    match r.read_until(b'\n', buf).await {
        Ok(0) => None,
        Ok(_) => {
            if buf.last() == Some(&b'\n') {
                buf.pop();
            }
            Some(String::from_utf8_lossy(buf).into_owned())
        }
        Err(e) => {
            tracing::warn!(error = %e, "sidecar pipe read failed");
            None
        }
    }
}

async fn stderr_loop(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let (mut r, mut buf) = (BufReader::new(stderr), Vec::new());
    while let Some(line) = next_line(&mut r, &mut buf).await {
        tracing::debug!(target: "sidecar_stderr", "{line}");
        let mut t = lock(&tail);
        if t.len() == STDERR_TAIL_LINES {
            t.pop_front();
        }
        t.push_back(line);
    }
}

async fn read_loop(
    stdout: tokio::process::ChildStdout,
    events: mpsc::UnboundedSender<SidecarEvent>,
    pending: Pending,
    child: Arc<tokio::sync::Mutex<Child>>,
    stderr_done: tokio::task::JoinHandle<()>,
    tail: Arc<Mutex<VecDeque<String>>>,
) {
    let (mut r, mut buf) = (BufReader::new(stdout), Vec::new());
    loop {
        match next_line(&mut r, &mut buf).await {
            Some(line) if line.trim().is_empty() => {}
            Some(line) => match serde_json::from_str::<SidecarMessage>(&line) {
                Ok(msg) => {
                    if let Some(reply_to) = &msg.reply_to
                        && let Some(waiter) = lock(&pending).remove(reply_to)
                    {
                        let _ = waiter.send(msg.clone());
                    }
                    if events.send(SidecarEvent::Message(msg)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "bad sidecar line");
                    let _ = events.send(SidecarEvent::ProtocolError {
                        line,
                        error: e.to_string(),
                    });
                }
            },
            None => break,
        }
    }
    // Something the sidecar started may hold its stderr open after it is gone.
    let _ = tokio::time::timeout(Duration::from_secs(2), stderr_done).await;
    // Poll instead of holding the lock across `wait()`, so `kill()` stays usable.
    let code = loop {
        match child.lock().await.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(_) => break None,
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let stderr_tail = lock(&tail).iter().cloned().collect::<Vec<_>>().join("\n");
    tracing::info!(?code, "sidecar exited");
    lock(&pending).clear();
    let _ = events.send(SidecarEvent::Exited { code, stderr_tail });
}
