use crate::api::remote::ClientPresence;
use crate::app::AppContext;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

/// Monotonic per-connection sequence, used to disambiguate a reconnect that
/// reuses the same client id from the stale connection it replaced.
static CONN_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn router() -> Router<AppContext> {
    Router::new().route("/", get(ws_upgrade))
}

async fn ws_upgrade(State(ctx): State<AppContext>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, ctx))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Subscribe {
        topic: String,
    },
    Unsubscribe {
        topic: String,
    },
    /// Announce/refresh this client's presence (role, device class, pairing).
    Register {
        presence: ClientPresence,
    },
    /// Alias of `register` for subsequent role changes; handled identically.
    Update {
        presence: ClientPresence,
    },
    /// Re-broadcast a payload onto an arbitrary topic — the client→client
    /// proxy that drives the remote. Restricted to `remote_*` topics so a
    /// client can't spoof server-owned topics like `streams_stats`.
    Publish {
        topic: String,
        payload: serde_json::Value,
    },
}

async fn handle_socket(socket: WebSocket, ctx: AppContext) {
    let conn = CONN_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut client_id: Option<String> = None;
    let (mut sink, mut stream) = socket.split();
    let subs: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut bus_rx = ctx.events.subscribe();

    let subs_for_pump = subs.clone();
    let pump = tokio::spawn(async move {
        loop {
            let event = match bus_rx.recv().await {
                Ok(e) => e,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let should_send = subs_for_pump.lock().await.contains(&event.topic);
            if !should_send {
                continue;
            }
            let frame = serde_json::json!({
                "type": "event",
                "topic": event.topic,
                "payload": event.payload,
            });
            let text: axum::extract::ws::Utf8Bytes = frame.to_string().into();
            if sink.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = stream.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) else {
            continue;
        };
        match client_msg {
            ClientMessage::Subscribe { topic } => {
                subs.lock().await.insert(topic);
            }
            ClientMessage::Unsubscribe { topic } => {
                subs.lock().await.remove(&topic);
            }
            ClientMessage::Register { presence } | ClientMessage::Update { presence } => {
                client_id = Some(presence.id.clone());
                ctx.presence.upsert(conn, presence).await;
                ctx.events
                    .publish("remote_presence", ctx.presence.snapshot().await);
            }
            ClientMessage::Publish { topic, payload } => {
                if topic.starts_with("remote_") {
                    ctx.events.publish(topic, payload);
                }
            }
        }
    }

    // Connection closed: drop this client from the roster and let peers know.
    if let Some(id) = client_id {
        ctx.presence.remove(&id, conn).await;
        ctx.events
            .publish("remote_presence", ctx.presence.snapshot().await);
    }

    pump.abort();
}
