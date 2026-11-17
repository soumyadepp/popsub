//! The scenarios every client library must pass. Each starts its own
//! server and drivers, so a failure can't leak into the next one.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::driver::{Driver, connect_args};
use crate::server::{Backend, ScenarioServer};

/// How long to wait for an event that should arrive.
const WAIT: Duration = Duration::from_secs(5);

/// How long to watch for something that should not happen.
const QUIET: Duration = Duration::from_millis(800);

/// What a scenario returns: nothing, or why it failed.
pub type Outcome = Result<(), String>;

/// A scenario: given the driver command, runs to an outcome.
pub type Scenario = fn(String) -> Pin<Box<dyn Future<Output = Outcome> + Send>>;

/// Every scenario, by name, in the order they run.
pub const SCENARIOS: &[(&str, Scenario)] = &[
    ("connect_rejects_bad_credentials", |d| {
        Box::pin(connect_rejects_bad_credentials(d))
    }),
    ("publish_and_receive", |d| Box::pin(publish_and_receive(d))),
    ("replay_last", |d| Box::pin(replay_last(d))),
    ("unsubscribe_stops_delivery", |d| {
        Box::pin(unsubscribe_stops_delivery(d))
    }),
    ("server_errors_have_codes", |d| {
        Box::pin(server_errors_have_codes(d))
    }),
    ("reconnect_resubscribes_without_gaps", |d| {
        Box::pin(reconnect_resubscribes_without_gaps(d))
    }),
    ("confirmed_publish_is_resent_once", |d| {
        Box::pin(confirmed_publish_is_resent_once(d))
    }),
    ("manual_ack_redelivery_is_deduplicated", |d| {
        Box::pin(manual_ack_redelivery_is_deduplicated(d))
    }),
    ("durable_session_resumes_after_restart", |d| {
        Box::pin(durable_session_resumes_after_restart(d))
    }),
    ("taken_over_session_stops_the_old_client", |d| {
        Box::pin(taken_over_session_stops_the_old_client(d))
    }),
    ("disabled_account_stops_its_client", |d| {
        Box::pin(disabled_account_stops_its_client(d))
    }),
    ("revoked_access_ends_the_subscription", |d| {
        Box::pin(revoked_access_ends_the_subscription(d))
    }),
    ("admin_round_trip", |d| Box::pin(admin_round_trip(d))),
];

/// Fails the scenario unless a condition holds.
///
/// Args:
///     condition: The condition.
///     why: What was expected, with what happened.
///
/// Raises:
///     String: `why`, when the condition is false.
fn check(condition: bool, why: impl FnOnce() -> String) -> Outcome {
    if condition { Ok(()) } else { Err(why()) }
}

/// Starts a driver and connects it.
///
/// Args:
///     command: The driver command.
///     name: A name for messages.
///     args: The `connect` arguments.
///
/// Returns:
///     The connected driver.
///
/// Raises:
///     String: When it can't start or connect.
async fn connected(command: &str, name: &str, args: Value) -> Result<Driver, String> {
    let driver = Driver::spawn(command, name)?;
    driver.ok("connect", args).await?;
    Ok(driver)
}

/// Connects a driver as the default admin.
///
/// Args:
///     command: The driver command.
///     server: The server.
///     name: A name for messages.
///
/// Returns:
///     The connected driver.
///
/// Raises:
///     String: When it can't start or connect.
async fn admin(command: &str, server: &ScenarioServer, name: &str) -> Result<Driver, String> {
    connected(
        command,
        name,
        connect_args(&server.url(), "admin", "password"),
    )
    .await
}

/// Waits until the broker has no QoS 1 message in flight.
///
/// Args:
///     server: The server.
///
/// Raises:
///     String: When messages are still in flight after five seconds.
async fn nothing_in_flight(server: &ScenarioServer) -> Outcome {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if server.broker().stats().in_flight == 0 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(format!(
        "{} messages still unacknowledged on the server",
        server.broker().stats().in_flight
    ))
}

/// Checks that a rejected login fails `connect` with `invalid_credentials`
/// at once, without retrying.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn connect_rejects_bad_credentials(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let client = Driver::spawn(&command, "client")?;
    let mut args = connect_args(&server.url(), "admin", "not-the-password");
    args["reconnect"]["max_attempts"] = json!(10);
    let started = Instant::now();
    let error = client.fails("connect", args).await?;
    check(
        error["kind"] == "server" && error["code"] == "invalid_credentials",
        || format!("expected a server error invalid_credentials, got {error}"),
    )?;
    check(started.elapsed() < Duration::from_secs(3), || {
        format!(
            "a rejected login took {:?}; it shouldn't retry",
            started.elapsed()
        )
    })
}

/// Checks a confirmed publish's result and delivery, and a QoS 0 publish.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn publish_and_receive(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let client = admin(&command, &server, "client").await?;
    client
        .ok("subscribe", json!({"topic": "chat", "from": "new"}))
        .await?;
    let published = client
        .ok(
            "publish_confirmed",
            json!({"topic": "chat", "payload": "hello"}),
        )
        .await?;
    check(published["seq"] == 1, || {
        format!("the first publish should be seq 1: {published}")
    })?;
    check(
        published["message_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        || format!("publish_confirmed returned no message_id: {published}"),
    )?;
    let message = client
        .event(WAIT, "the confirmed message", |e| {
            e["event"] == "message" && e["topic"] == "chat"
        })
        .await?;
    check(
        message["seq"] == 1
            && message["payload"] == "hello"
            && message["qos"] == 1
            && message["message_id"] == published["message_id"],
        || format!("unexpected message {message} for {published}"),
    )?;
    client
        .ok("publish", json!({"topic": "chat", "payload": "quick"}))
        .await?;
    let quick = client
        .event(WAIT, "the QoS 0 message", |e| {
            e["event"] == "message" && e["topic"] == "chat"
        })
        .await?;
    check(
        quick["seq"] == 2 && quick["payload"] == "quick" && quick["qos"] == 0,
        || format!("unexpected QoS 0 message {quick}"),
    )
}

/// Checks that subscribing with `{"last": 3}` replays exactly the last
/// three messages, in order.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn replay_last(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    for n in 1..=5 {
        server.publish("history", &format!("m{n}"), 0).await?;
    }
    let client = admin(&command, &server, "client").await?;
    client
        .ok(
            "subscribe",
            json!({"topic": "history", "from": {"last": 3}}),
        )
        .await?;
    let seqs = client.messages("history", 3, WAIT).await;
    check(seqs == [3, 4, 5], || {
        format!("expected seqs [3, 4, 5], got {seqs:?}")
    })?;
    let extra = client.messages("history", 1, QUIET).await;
    check(extra.is_empty(), || {
        format!("unexpected extra messages {extra:?}")
    })
}

/// Checks that nothing arrives on a topic after unsubscribing from it.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn unsubscribe_stops_delivery(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let client = admin(&command, &server, "client").await?;
    client
        .ok("subscribe", json!({"topic": "news", "from": "new"}))
        .await?;
    server.publish("news", "before", 1).await?;
    let before = client.messages("news", 1, WAIT).await;
    check(before == [1], || {
        format!("expected [1] before unsubscribing, got {before:?}")
    })?;
    client.ok("unsubscribe", json!({"topic": "news"})).await?;
    server.publish("news", "after", 1).await?;
    let after = client.messages("news", 1, QUIET).await;
    check(after.is_empty(), || {
        format!("got {after:?} after unsubscribing")
    })?;
    nothing_in_flight(&server).await
}

/// Checks that refusals reach the application as server errors with their
/// codes: an invalid topic, a missing user, a forbidden admin request and a
/// publish without permission.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn server_errors_have_codes(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let boss = admin(&command, &server, "admin").await?;
    let expect = |error: &Value, code: &str| {
        check(error["kind"] == "server" && error["code"] == code, || {
            format!("expected a server error {code}, got {error}")
        })
    };
    expect(
        &boss
            .fails("subscribe", json!({"topic": "bad/#", "from": "new"}))
            .await?,
        "invalid_topic",
    )?;
    expect(
        &boss
            .fails(
                "admin",
                json!({"method": "get_user", "params": {"username": "nobody"}}),
            )
            .await?,
        "user_not_found",
    )?;
    boss.ok(
        "admin",
        json!({"method": "create_user", "params": {"username": "nora", "password": "nora-password-1", "role": {"kind": "user"}}}),
    )
    .await?;
    let nora = connected(
        &command,
        "nora",
        connect_args(&server.url(), "nora", "nora-password-1"),
    )
    .await?;
    expect(
        &nora
            .fails(
                "publish_confirmed",
                json!({"topic": "secret", "payload": "x"}),
            )
            .await?,
        "not_authorized",
    )?;
    expect(
        &nora
            .fails("admin", json!({"method": "list_users", "params": {}}))
            .await?,
        "forbidden",
    )
}

/// Checks that after the connection drops, the client reconnects,
/// resubscribes from the next message it expects, and receives what was
/// published while it was away exactly once, in order.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn reconnect_resubscribes_without_gaps(command: String) -> Outcome {
    let mut server = ScenarioServer::start(Backend::Memory).await?;
    let client = admin(&command, &server, "client").await?;
    client
        .ok("subscribe", json!({"topic": "feed", "from": "new"}))
        .await?;
    for n in 1..=3 {
        server.publish("feed", &format!("m{n}"), 1).await?;
    }
    let first = client.messages("feed", 3, WAIT).await;
    check(first == [1, 2, 3], || {
        format!("expected [1, 2, 3], got {first:?}")
    })?;
    server.pause().await;
    let down = client
        .event(WAIT, "a disconnected event", |e| {
            e["event"] == "disconnected"
        })
        .await?;
    check(down["reconnecting"] == true, || {
        format!("the client should reconnect after going_away: {down}")
    })?;
    for n in 4..=6 {
        server.publish("feed", &format!("m{n}"), 1).await?;
    }
    server.resume().await?;
    client
        .event(WAIT, "a connected event", |e| e["event"] == "connected")
        .await?;
    let missed = client.messages("feed", 3, WAIT).await;
    check(missed == [4, 5, 6], || {
        format!("expected the missed [4, 5, 6] after reconnecting, got {missed:?}")
    })?;
    server.publish("feed", "m7", 1).await?;
    let rest = client
        .messages("feed", 2, QUIET.max(Duration::from_secs(1)))
        .await;
    check(rest == [7], || {
        format!("expected only [7] afterwards, got {rest:?}")
    })?;
    nothing_in_flight(&server).await
}

/// Checks that a confirmed publish made while disconnected is sent after
/// reconnecting, confirmed, and stored once.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn confirmed_publish_is_resent_once(command: String) -> Outcome {
    let mut server = ScenarioServer::start(Backend::Memory).await?;
    let client = admin(&command, &server, "client").await?;
    server.pause().await;
    client
        .event(WAIT, "a disconnected event", |e| {
            e["event"] == "disconnected"
        })
        .await?;
    let publish = client.send(
        "publish_confirmed",
        json!({"topic": "orders", "payload": "once"}),
    );
    let resume = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        server.resume().await
    };
    let (published, resumed) = tokio::join!(publish, resume);
    resumed?;
    let published = published?;
    check(published["ok"] == true && published["seq"] == 1, || {
        format!("the publish should be confirmed as seq 1: {published}")
    })?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stored = server.last_seq("orders").await;
    check(stored == 1, || {
        format!("expected 1 stored record, found {stored}")
    })
}

/// Checks that in manual mode an unacknowledged QoS 1 message the server
/// resends reaches the application once, and acknowledging it clears it on
/// the server.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn manual_ack_redelivery_is_deduplicated(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let mut args = connect_args(&server.url(), "admin", "password");
    args["ack_mode"] = json!("manual");
    let client = connected(&command, "client", args).await?;
    client
        .ok("subscribe", json!({"topic": "tasks", "from": "new"}))
        .await?;
    server.publish("tasks", "job", 1).await?;
    let first = client.messages("tasks", 1, WAIT).await;
    check(first == [1], || format!("expected [1], got {first:?}"))?;
    let again = client
        .messages("tasks", 1, Duration::from_millis(2500))
        .await;
    check(again.is_empty(), || {
        format!("a resend reached the application: {again:?}")
    })?;
    client
        .ok("ack", json!({"topic": "tasks", "seq": 1}))
        .await?;
    nothing_in_flight(&server).await
}

/// Checks that a durable session resumes after a server restart: the
/// subscription comes back without resubscribing, acknowledged messages
/// aren't delivered again, and new ones arrive.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn durable_session_resumes_after_restart(command: String) -> Outcome {
    let mut server = ScenarioServer::start(Backend::File).await?;
    let mut args = connect_args(&server.url(), "admin", "password");
    args["session_id"] = json!("phone");
    args["ack_mode"] = json!("manual");
    let client = connected(&command, "client", args).await?;
    client
        .ok("subscribe", json!({"topic": "jobs", "from": "resume"}))
        .await?;
    server.publish("jobs", "one", 1).await?;
    server.publish("jobs", "two", 1).await?;
    let before = client.messages("jobs", 2, WAIT).await;
    check(before == [1, 2], || {
        format!("expected [1, 2], got {before:?}")
    })?;
    client.ok("ack", json!({"topic": "jobs", "seq": 1})).await?;
    let deadline = Instant::now() + WAIT;
    while server.broker().stats().in_flight != 1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.restart().await?;
    let up = client
        .event(WAIT, "a connected event after the restart", |e| {
            e["event"] == "connected"
        })
        .await?;
    check(
        up["resumed"]
            .as_array()
            .is_some_and(|topics| topics.iter().any(|t| t == "jobs")),
        || format!("the session should resume jobs: {up}"),
    )?;
    server.publish("jobs", "three", 1).await?;
    let after = client.messages("jobs", 2, QUIET).await;
    check(after == [3], || {
        format!("expected only the new [3] after the restart, got {after:?}")
    })?;
    client.ok("ack", json!({"topic": "jobs", "seq": 2})).await?;
    client.ok("ack", json!({"topic": "jobs", "seq": 3})).await?;
    nothing_in_flight(&server).await
}

/// Checks that when a second connection takes over a session, the first
/// client stops for good with `session_taken_over` and its requests fail
/// as closed.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn taken_over_session_stops_the_old_client(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let mut args = connect_args(&server.url(), "admin", "password");
    args["session_id"] = json!("shared");
    let old = connected(&command, "old", args.clone()).await?;
    let _new = connected(&command, "new", args).await?;
    let down = old
        .event(WAIT, "a disconnected event", |e| {
            e["event"] == "disconnected"
        })
        .await?;
    check(
        down["code"] == 4003 && down["reconnecting"] == false,
        || format!("expected code 4003 without reconnecting, got {down}"),
    )?;
    let error = old
        .fails("publish_confirmed", json!({"topic": "t", "payload": "x"}))
        .await?;
    check(
        error["kind"] == "closed" && error["close_code"] == 4003,
        || format!("expected a closed error with close_code 4003, got {error}"),
    )
}

/// Checks that disabling an account stops its client for good with
/// `account_disabled`.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn disabled_account_stops_its_client(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let boss = admin(&command, &server, "admin").await?;
    boss.ok(
        "admin",
        json!({"method": "create_user", "params": {"username": "dora", "password": "dora-password-1", "role": {"kind": "user"}}}),
    )
    .await?;
    let dora = connected(
        &command,
        "dora",
        connect_args(&server.url(), "dora", "dora-password-1"),
    )
    .await?;
    boss.ok(
        "admin",
        json!({"method": "set_enabled", "params": {"username": "dora", "enabled": false}}),
    )
    .await?;
    let down = dora
        .event(WAIT, "a disconnected event", |e| {
            e["event"] == "disconnected"
        })
        .await?;
    check(
        down["code"] == 4009 && down["reconnecting"] == false,
        || format!("expected code 4009 without reconnecting, got {down}"),
    )
}

/// Checks that revoking a user's access ends its live subscription: the
/// client reports `unsubscribed` and the application's subscription ends.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn revoked_access_ends_the_subscription(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let boss = admin(&command, &server, "admin").await?;
    boss.ok(
        "admin",
        json!({"method": "create_user", "params": {"username": "rita", "password": "rita-password-1", "role": {"kind": "user", "topics": [{"pattern": "news/#", "access": "subscribe"}]}}}),
    )
    .await?;
    let rita = connected(
        &command,
        "rita",
        connect_args(&server.url(), "rita", "rita-password-1"),
    )
    .await?;
    rita.ok("subscribe", json!({"topic": "news/today", "from": "new"}))
        .await?;
    boss.ok(
        "admin",
        json!({"method": "revoke", "params": {"username": "rita", "pattern": "news/#"}}),
    )
    .await?;
    let mut seen_unsubscribed = false;
    let mut seen_ended = false;
    while !(seen_unsubscribed && seen_ended) {
        let event = rita
            .event(WAIT, "unsubscribed and subscription_ended", |e| {
                (e["event"] == "unsubscribed" || e["event"] == "subscription_ended")
                    && e["topic"] == "news/today"
            })
            .await?;
        seen_unsubscribed |= event["event"] == "unsubscribed";
        seen_ended |= event["event"] == "subscription_ended";
    }
    server.publish("news/today", "late", 1).await?;
    let late = rita.messages("news/today", 1, QUIET).await;
    check(late.is_empty(), || format!("got {late:?} after the revoke"))
}

/// Checks every admin call and its results: create, a duplicate create,
/// get, list, grant, revoke, a password change the user can log in with,
/// forbidden for that user, delete, and get after delete.
///
/// Args:
///     command: The driver command.
///
/// Raises:
///     String: Why it failed.
async fn admin_round_trip(command: String) -> Outcome {
    let server = ScenarioServer::start(Backend::Memory).await?;
    let boss = admin(&command, &server, "admin").await?;
    let call = |method: &str, params: Value| json!({"method": method, "params": params});
    let created = boss
        .ok(
            "admin",
            call(
                "create_user",
                json!({"username": "erin", "password": "erin-password-1", "role": {"kind": "user", "topics": [{"pattern": "news/#", "access": "both"}]}}),
            ),
        )
        .await?;
    let user = &created["result"];
    check(
        user["username"] == "erin" && user["enabled"] == true && user["role"]["kind"] == "user",
        || format!("unexpected created user {created}"),
    )?;
    let duplicate = boss
        .fails(
            "admin",
            call(
                "create_user",
                json!({"username": "erin", "password": "erin-password-1", "role": {"kind": "user"}}),
            ),
        )
        .await?;
    check(duplicate["code"] == "user_exists", || {
        format!("expected user_exists, got {duplicate}")
    })?;
    let fetched = boss
        .ok("admin", call("get_user", json!({"username": "erin"})))
        .await?;
    check(fetched["result"] == created["result"], || {
        format!("get_user {fetched} differs from create_user {created}")
    })?;
    let listed = boss.ok("admin", call("list_users", json!({}))).await?;
    let names: Vec<&str> = listed["result"]
        .as_array()
        .map(|users| {
            users
                .iter()
                .filter_map(|u| u["username"].as_str())
                .collect()
        })
        .unwrap_or_default();
    check(names == ["admin", "erin"], || {
        format!("expected [admin, erin], got {listed}")
    })?;
    let granted = boss
        .ok(
            "admin",
            call(
                "grant",
                json!({"username": "erin", "pattern": "orders", "access": "publish"}),
            ),
        )
        .await?;
    check(
        granted["result"]["role"]["topics"].as_array().map(Vec::len) == Some(2),
        || format!("expected two grants after grant, got {granted}"),
    )?;
    let revoked = boss
        .ok(
            "admin",
            call("revoke", json!({"username": "erin", "pattern": "orders"})),
        )
        .await?;
    check(
        revoked["result"]["role"]["topics"].as_array().map(Vec::len) == Some(1),
        || format!("expected one grant after revoke, got {revoked}"),
    )?;
    boss.ok(
        "admin",
        call(
            "set_password",
            json!({"username": "erin", "password": "erin-password-2"}),
        ),
    )
    .await?;
    let erin = connected(
        &command,
        "erin",
        connect_args(&server.url(), "erin", "erin-password-2"),
    )
    .await?;
    let forbidden = erin.fails("admin", call("list_users", json!({}))).await?;
    check(forbidden["code"] == "forbidden", || {
        format!("expected forbidden for a non-admin, got {forbidden}")
    })?;
    let deleted = boss
        .ok("admin", call("delete_user", json!({"username": "erin"})))
        .await?;
    check(deleted["result"].is_null(), || {
        format!("delete_user should return null, got {deleted}")
    })?;
    let gone = boss
        .fails("admin", call("get_user", json!({"username": "erin"})))
        .await?;
    check(gone["code"] == "user_not_found", || {
        format!("expected user_not_found after delete, got {gone}")
    })
}
