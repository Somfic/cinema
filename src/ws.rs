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
use tokio::sync::Mutex;

pub fn router() -> Router<AppContext> {
    Router::new().route("/", get(ws_upgrade))
}

async fn ws_upgrade(State(ctx): State<AppContext>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, ctx))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Subscribe { topic: String },
    Unsubscribe { topic: String },
}

async fn handle_socket(socket: WebSocket, ctx: AppContext) {
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
        }
    }

    pump.abort();
}
