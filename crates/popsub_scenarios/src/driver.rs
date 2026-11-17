//! A running driver process: sends it commands, matches their responses,
//! and collects its events (see `clients/DRIVER.md`).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

/// How long a command may take before the scenario fails.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// Responses waiting to be matched, by command id.
type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// One driver process.
pub struct Driver {
    /// A name for messages, such as `alice`.
    name: String,
    /// The process; killed when the driver is dropped.
    _child: Child,
    /// Where commands are written.
    stdin: Mutex<ChildStdin>,
    /// The next command id.
    next_id: AtomicU64,
    /// Commands waiting for their responses.
    waiting: Waiting,
    /// Events not yet looked at, in arrival order.
    events: Mutex<mpsc::UnboundedReceiver<Value>>,
}

impl Driver {
    /// Starts a driver process with `sh -c`.
    ///
    /// Args:
    ///     command: The shell command that runs the driver.
    ///     name: A name for messages.
    ///
    /// Returns:
    ///     The running driver.
    ///
    /// Raises:
    ///     String: When the process can't be started.
    pub fn spawn(command: &str, name: &str) -> Result<Self, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("can't start the driver `{command}`: {e}"))?;
        let stdin = child.stdin.take().ok_or("the driver has no stdin")?;
        let stdout = child.stdout.take().ok_or("the driver has no stdout")?;
        let waiting: Waiting = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let reader_waiting = waiting.clone();
        let reader_name = name.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    eprintln!("[{reader_name}] not JSON: {line}");
                    continue;
                };
                if value.get("event").is_some() {
                    let _ = events_tx.send(value);
                } else if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if let Some(reply) = reader_waiting.lock().await.remove(&id) {
                        let _ = reply.send(value);
                    }
                } else {
                    eprintln!("[{reader_name}] neither a response nor an event: {line}");
                }
            }
        });
        Ok(Self {
            name: name.to_string(),
            _child: child,
            stdin: Mutex::new(stdin),
            next_id: AtomicU64::new(1),
            waiting,
            events: Mutex::new(events_rx),
        })
    }

    /// Sends a command and waits for its response.
    ///
    /// Args:
    ///     op: The command's name.
    ///     args: Its arguments, an object.
    ///
    /// Returns:
    ///     The whole response, `ok` true or false.
    ///
    /// Raises:
    ///     String: When it can't be sent, or no response arrives in time.
    pub async fn send(&self, op: &str, args: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut command = json!({"id": id, "op": op});
        if let (Some(command), Some(args)) = (command.as_object_mut(), args.as_object()) {
            command.extend(args.clone());
        }
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(id, tx);
        let mut line = command.to_string();
        line.push('\n');
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("[{}] writing {op} failed: {e}", self.name))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("[{}] writing {op} failed: {e}", self.name))?;
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(format!("[{}] exited before answering {op}", self.name)),
            Err(_) => Err(format!(
                "[{}] didn't answer {op} within {COMMAND_TIMEOUT:?}",
                self.name
            )),
        }
    }

    /// Sends a command that must succeed.
    ///
    /// Args:
    ///     op: The command's name.
    ///     args: Its arguments.
    ///
    /// Returns:
    ///     The response.
    ///
    /// Raises:
    ///     String: When it fails or doesn't answer.
    pub async fn ok(&self, op: &str, args: Value) -> Result<Value, String> {
        let response = self.send(op, args).await?;
        if response["ok"] == true {
            Ok(response)
        } else {
            Err(format!("[{}] {op} failed: {response}", self.name))
        }
    }

    /// Sends a command that must fail.
    ///
    /// Args:
    ///     op: The command's name.
    ///     args: Its arguments.
    ///
    /// Returns:
    ///     The response's `error` object.
    ///
    /// Raises:
    ///     String: When it succeeds or doesn't answer.
    pub async fn fails(&self, op: &str, args: Value) -> Result<Value, String> {
        let response = self.send(op, args).await?;
        if response["ok"] == false {
            Ok(response["error"].clone())
        } else {
            Err(format!(
                "[{}] {op} should have failed: {response}",
                self.name
            ))
        }
    }

    /// Waits for the next event matching a condition, discarding events
    /// before it that don't match.
    ///
    /// Args:
    ///     within: How long to wait.
    ///     what: A description for the failure message.
    ///     matches: The condition.
    ///
    /// Returns:
    ///     The matching event.
    ///
    /// Raises:
    ///     String: When none arrives in time.
    pub async fn event(
        &self,
        within: Duration,
        what: &str,
        matches: impl Fn(&Value) -> bool,
    ) -> Result<Value, String> {
        let mut events = self.events.lock().await;
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(event)) if matches(&event) => return Ok(event),
                Ok(Some(_)) => continue,
                Ok(None) => return Err(format!("[{}] exited while waiting for {what}", self.name)),
                Err(_) => return Err(format!("[{}] no {what} within {within:?}", self.name)),
            }
        }
    }

    /// Collects `message` events on a topic for a while.
    ///
    /// Args:
    ///     topic: The topic.
    ///     count: Stop after this many.
    ///     within: The longest to wait.
    ///
    /// Returns:
    ///     The messages' `seq`s in arrival order. Other events are discarded.
    pub async fn messages(&self, topic: &str, count: usize, within: Duration) -> Vec<u64> {
        let mut events = self.events.lock().await;
        let deadline = tokio::time::Instant::now() + within;
        let mut seqs = Vec::new();
        while seqs.len() < count {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(event)) => {
                    if event["event"] == "message" && event["topic"] == topic {
                        seqs.push(event["seq"].as_u64().unwrap_or(0));
                    }
                }
                _ => break,
            }
        }
        seqs
    }
}

impl Driver {
    /// Collects `message` events on any topic for a while.
    ///
    /// Args:
    ///     count: Stop after this many.
    ///     within: The longest to wait.
    ///
    /// Returns:
    ///     Each message's topic and `seq`, in arrival order. Other events
    ///     are discarded.
    pub async fn messages_on_any(&self, count: usize, within: Duration) -> Vec<(String, u64)> {
        let mut events = self.events.lock().await;
        let deadline = tokio::time::Instant::now() + within;
        let mut seen = Vec::new();
        while seen.len() < count {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(event)) => {
                    if event["event"] == "message" {
                        seen.push((
                            event["topic"].as_str().unwrap_or_default().to_string(),
                            event["seq"].as_u64().unwrap_or(0),
                        ));
                    }
                }
                _ => break,
            }
        }
        seen
    }
}

/// Builds `connect` arguments with fast reconnects.
///
/// Args:
///     url: The server's URL.
///     username: The account.
///     password: Its password.
///
/// Returns:
///     The arguments, to adjust before sending.
pub fn connect_args(url: &str, username: &str, password: &str) -> Value {
    json!({
        "url": url,
        "username": username,
        "password": password,
        "session_id": null,
        "clean_start": false,
        "ack_mode": "auto",
        "reconnect": {"initial_ms": 20, "max_ms": 200, "max_attempts": null},
        "request_timeout_ms": 10000,
    })
}
