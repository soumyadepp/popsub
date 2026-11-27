//! The scenario driver for the Rust client, `popsub_sdk`: reads commands
//! from standard input and writes responses and events to standard output,
//! as `clients/DRIVER.md` describes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use popsub_protocol::{Access, RoleSpec, StartFrom};
use popsub_sdk::{
    AckMode, Client, ClientConfig, Credentials, Error, Event, Message, ReconnectPolicy,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

/// What the driver keeps between commands.
#[derive(Default)]
struct State {
    /// The client, once connected.
    client: Mutex<Option<Client>>,
    /// Messages reported and not yet acknowledged, for `ack` in manual mode.
    held: Mutex<HashMap<(String, u64), Message>>,
    /// Topics the harness unsubscribed from, whose ending isn't reported.
    leaving: Mutex<HashSet<String>>,
}

/// Where lines for standard output go.
type Out = mpsc::UnboundedSender<Value>;

/// Reads commands until standard input closes or `exit`, handling each
/// concurrently, and writes every line through one writer task.
#[tokio::main]
async fn main() {
    let (out, mut lines) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = lines.recv().await {
            let mut text = line.to_string();
            text.push('\n');
            if stdout.write_all(text.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });
    let state = Arc::new(State::default());
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = input.next_line().await {
        let Ok(command) = serde_json::from_str::<Value>(&line) else {
            eprintln!("ignoring a line that isn't JSON: {line}");
            continue;
        };
        let id = command["id"].clone();
        if command["op"] == "exit" {
            let _ = out.send(json!({"id": id, "ok": true}));
            break;
        }
        let (state, out) = (state.clone(), out.clone());
        tokio::spawn(async move {
            let response = match handle(&state, &out, &command).await {
                Ok(Value::Object(mut fields)) => {
                    fields.insert("id".to_string(), id);
                    fields.insert("ok".to_string(), json!(true));
                    Value::Object(fields)
                }
                Ok(_) => json!({"id": id, "ok": true}),
                Err(error) => json!({"id": id, "ok": false, "error": error}),
            };
            let _ = out.send(response);
        });
    }
    if let Some(client) = state.client.lock().await.take() {
        client.close();
    }
    drop(out);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

/// Carries out one command.
///
/// Args:
///     state: The driver's state.
///     out: Where events go.
///     command: The command.
///
/// Returns:
///     The response's result fields, as an object.
///
/// Raises:
///     Value: The response's `error` object.
async fn handle(state: &Arc<State>, out: &Out, command: &Value) -> Result<Value, Value> {
    let op = command["op"].as_str().unwrap_or_default();
    if op == "connect" {
        return connect(state, out, command).await;
    }
    let Some(client) = state.client.lock().await.clone() else {
        return Err(failure("protocol", "connect first"));
    };
    let text = |key: &str| command[key].as_str().unwrap_or_default().to_string();
    match op {
        "subscribe" => {
            let from: StartFrom = serde_json::from_value(command["from"].clone())
                .map_err(|e| failure("config", &e.to_string()))?;
            let topic = text("topic");
            state.leaving.lock().await.remove(&topic);
            let joined = match command["group"].as_str() {
                Some(group) => client.subscribe_shared(&topic, group, from).await,
                None => client.subscribe(&topic, from).await,
            };
            let mut subscription = joined.map_err(error)?;
            let (state, out) = (state.clone(), out.clone());
            tokio::spawn(async move {
                while let Some(message) = subscription.next().await {
                    let _ = out.send(json!({
                        "event": "message",
                        "topic": message.topic,
                        "seq": message.seq,
                        "payload": message.payload,
                        "bytes": message.bytes().ok().map(|bytes| STANDARD.encode(bytes)),
                        "message_id": message.message_id,
                        "qos": message.qos,
                    }));
                    if message.qos == 1 {
                        state
                            .held
                            .lock()
                            .await
                            .insert((message.topic.clone(), message.seq), message);
                    }
                }
                if !state.leaving.lock().await.contains(&topic) {
                    let _ = out.send(json!({"event": "subscription_ended", "topic": topic}));
                }
            });
            Ok(json!({}))
        }
        "unsubscribe" => {
            let topic = text("topic");
            state.leaving.lock().await.insert(topic.clone());
            client.unsubscribe(topic).await.map_err(error)?;
            Ok(json!({}))
        }
        "publish" => {
            match bytes(command)? {
                Some(bytes) => client.publish_bytes(text("topic"), &bytes),
                None => client.publish(text("topic"), text("payload")),
            }
            .map_err(error)?;
            Ok(json!({}))
        }
        "publish_confirmed" => {
            let published = match bytes(command)? {
                Some(bytes) => client.publish_bytes_confirmed(text("topic"), &bytes).await,
                None => {
                    client
                        .publish_confirmed(text("topic"), text("payload"))
                        .await
                }
            }
            .map_err(error)?;
            Ok(json!({"seq": published.seq, "message_id": published.message_id}))
        }
        "ack" => {
            let key = (text("topic"), command["seq"].as_u64().unwrap_or_default());
            let message = state
                .held
                .lock()
                .await
                .remove(&key)
                .ok_or_else(|| failure("protocol", "no such message was reported"))?;
            message.ack();
            Ok(json!({}))
        }
        "admin" => admin(&client, command).await,
        "close" => {
            client.close();
            Ok(json!({}))
        }
        other => Err(failure("protocol", &format!("unknown op {other}"))),
    }
}

/// Connects the client and starts forwarding its events.
///
/// Args:
///     state: The driver's state.
///     out: Where events go.
///     command: The `connect` command.
///
/// Returns:
///     An empty result.
///
/// Raises:
///     Value: The error when connecting fails.
async fn connect(state: &Arc<State>, out: &Out, command: &Value) -> Result<Value, Value> {
    let text = |key: &str| command[key].as_str().unwrap_or_default().to_string();
    let millis =
        |value: &Value, default: u64| Duration::from_millis(value.as_u64().unwrap_or(default));
    let reconnect = &command["reconnect"];
    let mut config = ClientConfig::new(
        text("url"),
        Credentials::password(text("username"), text("password")),
    )
    .reconnect(ReconnectPolicy {
        initial: millis(&reconnect["initial_ms"], 250),
        max: millis(&reconnect["max_ms"], 30_000),
        max_attempts: reconnect["max_attempts"]
            .as_u64()
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    })
    .request_timeout(millis(&command["request_timeout_ms"], 30_000))
    .ack_mode(if command["ack_mode"] == "manual" {
        AckMode::Manual
    } else {
        AckMode::Auto
    });
    if let Some(session) = command["session_id"].as_str() {
        config = config.session(session);
    }
    config.clean_start = command["clean_start"].as_bool().unwrap_or(false);
    let client = Client::connect(config).await.map_err(error)?;
    let mut events = client.events();
    let out = out.clone();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = out.send(event_json(event));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    *state.client.lock().await = Some(client);
    Ok(json!({}))
}

/// Calls an admin method.
///
/// Args:
///     client: The client.
///     command: The `admin` command.
///
/// Returns:
///     `{"result": ...}`: a user, a list of users, or `null`.
///
/// Raises:
///     Value: The error when the call fails or its parameters are invalid.
async fn admin(client: &Client, command: &Value) -> Result<Value, Value> {
    let params = &command["params"];
    let text = |key: &str| params[key].as_str().unwrap_or_default().to_string();
    let parse = |key: &str| -> Result<Value, Value> { Ok(params[key].clone()) };
    let role = || -> Result<RoleSpec, Value> {
        serde_json::from_value(parse("role")?).map_err(|e| failure("config", &e.to_string()))
    };
    let access = || -> Result<Access, Value> {
        serde_json::from_value(parse("access")?).map_err(|e| failure("config", &e.to_string()))
    };
    let admin = client.admin();
    let user = |info| json!({"result": info});
    match command["method"].as_str().unwrap_or_default() {
        "create_user" => admin
            .create_user(text("username"), text("password"), role()?)
            .await
            .map(user)
            .map_err(error),
        "get_user" => admin
            .get_user(text("username"))
            .await
            .map(user)
            .map_err(error),
        "list_users" => admin
            .list_users()
            .await
            .map(|users| json!({"result": users}))
            .map_err(error),
        "set_role" => admin
            .set_role(text("username"), role()?)
            .await
            .map(user)
            .map_err(error),
        "grant" => admin
            .grant(text("username"), text("pattern"), access()?)
            .await
            .map(user)
            .map_err(error),
        "revoke" => admin
            .revoke(text("username"), text("pattern"))
            .await
            .map(user)
            .map_err(error),
        "set_password" => admin
            .set_password(text("username"), text("password"))
            .await
            .map(user)
            .map_err(error),
        "set_enabled" => admin
            .set_enabled(
                text("username"),
                params["enabled"].as_bool().unwrap_or(true),
            )
            .await
            .map(user)
            .map_err(error),
        "delete_user" => admin
            .delete_user(text("username"))
            .await
            .map(|()| json!({"result": null}))
            .map_err(error),
        "list_topics" => admin
            .list_topics()
            .await
            .map(|topics| json!({"result": topics}))
            .map_err(error),
        "get_topic" => admin
            .get_topic(text("topic"))
            .await
            .map(|topic| json!({"result": topic}))
            .map_err(error),
        "purge_topic" => admin
            .purge_topic(text("topic"))
            .await
            .map(|topic| json!({"result": topic}))
            .map_err(error),
        "backup" => admin
            .backup(params["name"].as_str().map(str::to_string))
            .await
            .map(|backup| json!({"result": backup}))
            .map_err(error),
        other => Err(failure(
            "protocol",
            &format!("unknown admin method {other}"),
        )),
    }
}

/// Describes a client event as a driver event.
///
/// Args:
///     event: The event.
///
/// Returns:
///     The driver event.
fn event_json(event: Event) -> Value {
    match event {
        Event::Connected { resumed } => json!({"event": "connected", "resumed": resumed}),
        Event::Disconnected { code, reconnecting } => json!({
            "event": "disconnected",
            "code": code.map(|c| c.code()),
            "reconnecting": reconnecting,
        }),
        Event::Truncated {
            topic,
            first_available,
        } => json!({"event": "truncated", "topic": topic, "first_available": first_available}),
        Event::Stalled { topic, seq } => json!({"event": "stalled", "topic": topic, "seq": seq}),
        Event::AuthExpiring { expires_at_ms } => {
            json!({"event": "auth_expiring", "expires_at_ms": expires_at_ms})
        }
        Event::AuthRefreshed => json!({"event": "auth_refreshed"}),
        Event::Unsubscribed { topic } => json!({"event": "unsubscribed", "topic": topic}),
        Event::ServerError { code, message } => {
            json!({"event": "server_error", "code": code.as_str(), "message": message})
        }
    }
}

/// Reads a publish command's `bytes`: the body to publish as bytes, in base64.
///
/// Args:
///     command: The command.
///
/// Returns:
///     The bytes, or `None` when the command carries a text `payload` instead.
///
/// Raises:
///     Value: A `config` error when `bytes` isn't base64.
fn bytes(command: &Value) -> Result<Option<Vec<u8>>, Value> {
    match command["bytes"].as_str() {
        Some(encoded) => STANDARD
            .decode(encoded)
            .map(Some)
            .map_err(|e| failure("config", &e.to_string())),
        None => Ok(None),
    }
}

/// Describes a client error as a driver error.
///
/// Args:
///     error: The error.
///
/// Returns:
///     The `error` object.
fn error(error: Error) -> Value {
    let message = error.to_string();
    match error {
        Error::Server { code, .. } => {
            json!({"kind": "server", "code": code.as_str(), "message": message})
        }
        Error::Closed(code) => json!({
            "kind": "closed",
            "close_code": code.map(|c| c.code()),
            "message": message,
        }),
        Error::Timeout => failure("timeout", &message),
        Error::Connect(_) => failure("connect", &message),
        Error::Protocol(_) => failure("protocol", &message),
        Error::Config(_) => failure("config", &message),
    }
}

/// Builds an `error` object without a code.
///
/// Args:
///     kind: The error kind.
///     message: What went wrong.
///
/// Returns:
///     The `error` object.
fn failure(kind: &str, message: &str) -> Value {
    json!({"kind": kind, "message": message})
}
