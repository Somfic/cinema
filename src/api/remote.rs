use cinema_schema::{cinema_events, cinema_type};

/// One connected WebSocket client, as seen by every other client. The full
/// roster is broadcast on the `remote_presence` topic whenever it changes, so a
/// phone can discover TVs to pair with and a TV can detect a remote pairing to
/// it. There is no auth/user concept on the server — pairing is presence-based
/// and assumes a trusted local network.
#[cinema_type]
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

#[cinema_events(namespace = "remote")]
pub trait RemoteEvents {
    /// Full roster of connected clients, re-broadcast on every
    /// connect / disconnect / role change. Topic: `remote_presence`.
    /// Subscribers diff it client-side to drive pairing.
    fn presence(payload: Vec<ClientPresence>);
}
