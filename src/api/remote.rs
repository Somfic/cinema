/// One connected WebSocket client, as seen by every other client. The full
/// roster is broadcast on the `remote_presence` topic whenever it changes, so a
/// phone can discover TVs to pair with and a TV can detect a remote pairing to
/// it. There is no auth/user concept on the server — pairing is presence-based
/// and assumes a trusted local network.
#[draad::ty]
pub struct ClientPresence {
    /// Per-window client id, browser-generated and held in `sessionStorage`
    /// (unique per tab, stable across reloads).
    pub id: String,
    /// Human label shown in the pairing UI (e.g. "Chrome · macOS").
    pub label: String,
    /// Coarse device class from viewport width: `phone` | `tablet` | `desktop`.
    pub kind: String,
    /// Viewport width in CSS pixels. Used for relative sizing — with a
    /// single-user assumption, the smaller of two clients is the candidate
    /// remote and the larger is the TV.
    pub width: i64,
    /// Current role: `browser` | `tv` | `remote`.
    pub mode: String,
    /// When this client is a `remote`, the id of the TV it controls.
    pub paired_to: Option<String>,
}

#[draad::events(namespace = "remote")]
pub trait RemoteEvents {
    /// Full roster of connected clients, re-broadcast on every
    /// connect / disconnect / role change. Topic: `remote_presence`.
    /// Subscribers diff it client-side to drive pairing.
    fn presence(payload: Vec<ClientPresence>);
}

use crate::app::AppContext;
pub use crate::app::CinemaError;
use draad::runtime::Conn;

#[draad::api(namespace = "remote")]
pub trait RemoteApi {
    /// Returns the caller's own presence and pushes it back down their socket
    /// (`remote_self`). Demonstrates injecting the live connection into an HTTP
    /// handler — the `conn` arg is server-filled, so the generated TS is just
    /// `whoami(): Promise<ClientPresence>`. 409s if the caller has no live socket.
    async fn whoami(&self, conn: &Conn) -> Result<ClientPresence, CinemaError>;
}

#[draad::api]
impl RemoteApi for AppContext {
    async fn whoami(&self, conn: &Conn) -> Result<ClientPresence, CinemaError> {
        let me = self
            .clients
            .get(conn.client_id())
            .await
            .ok_or_else(|| CinemaError::NotFound("client not in roster".into()))?;
        conn.send("remote_self", &me);
        Ok(me)
    }
}
