//! WebSocket layer, built on `draad::runtime`'s stateful session system.
//!
//! draad owns the socket plumbing and the reserved `subscribe`/`unsubscribe`
//! protocol (per-client topic filtering); this module is just the
//! application behaviour: maintaining the presence roster and relaying the
//! phone↔TV remote proxy. The roster lives in [`AppContext::clients`], keyed
//! by draad's per-connection id — so a reconnect replaces its own entry and
//! there's no stale-disconnect bookkeeping.

use crate::api::remote::ClientPresence;
use crate::app::AppContext;
use axum::Router;
use axum::routing::get;
use draad::runtime::{Conn, Session, session_handler};
use serde::Deserialize;

pub fn router() -> Router<AppContext> {
    Router::new().route("/", get(session_handler::<CinemaSession>))
}

/// Application client→server frames. draad consumes the reserved
/// `{type:"subscribe"|"unsubscribe"}` frames itself; everything else is
/// deserialised into this enum and handed to [`Session::on_message`].
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    /// Announce/refresh this client's presence (role, device class, pairing).
    Register { presence: ClientPresence },
    /// Alias of `register` for subsequent role changes; handled identically.
    Update { presence: ClientPresence },
    /// Re-broadcast a payload onto an arbitrary topic — the client→client
    /// proxy that drives the remote. Restricted to `remote_*` topics so a
    /// client can't spoof server-owned topics like `streams/stats`.
    Publish {
        topic: String,
        payload: serde_json::Value,
    },
}

/// Per-connection session. Holds no state of its own — the roster lives in
/// `AppContext::clients`.
pub(crate) struct CinemaSession;

#[async_trait::async_trait]
impl Session for CinemaSession {
    type State = AppContext;
    type Msg = ClientMessage;

    async fn on_connect(_conn: &Conn, _state: &AppContext) -> Self {
        CinemaSession
    }

    async fn on_message(&mut self, msg: ClientMessage, conn: &Conn, state: &AppContext) {
        match msg {
            ClientMessage::Register { mut presence } | ClientMessage::Update { mut presence } => {
                // The server-assigned client_id is authoritative: stamp it so the
                // roster and the client agree, and a client can't spoof another's id.
                presence.id = conn.client_id().to_string();
                state.clients.insert(conn.client_id(), presence).await;
                broadcast_roster(conn, state).await;
            }
            ClientMessage::Publish { topic, payload } => {
                if topic.starts_with("remote_") {
                    conn.publish(&topic, &payload);
                }
            }
        }
    }

    async fn on_disconnect(self, conn: &Conn, state: &AppContext) {
        state.clients.remove(conn.client_id()).await;
        broadcast_roster(conn, state).await;
    }
}

/// Push the full roster to every `remote_presence` subscriber.
async fn broadcast_roster(conn: &Conn, state: &AppContext) {
    let roster = state.clients.snapshot().await;
    conn.publish("remote_presence", &roster);
}
