use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::FromRef;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::api::remote::ClientPresence;
use crate::config::Config;

// ── Error ──

pub type Result<T> = std::result::Result<T, Error>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("config error: {0}")]
    TomlError(#[from] toml::de::Error),
    #[error("database error: {0}")]
    DatabaseError(#[from] sqlx::Error),
    #[error("http client error: {0}")]
    HttpClientError(#[from] reqwest::Error),
    #[error("failed to read config '{path}': {source}")]
    ConfigReadError {
        path: String,
        source: std::io::Error,
    },
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("migration error: {0}")]
    MigrationError(#[from] sqlx::migrate::MigrateError),
    #[error("address parse error: {0}")]
    AddressParseError(#[from] std::net::AddrParseError),
    #[error("{0}")]
    Generic(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    NotFound(String),
    #[error("json error: {0}")]
    JsonError(#[from] serde_json::Error),
}

// draad 0.2 returns the trait's `Result<_, Error>` straight to axum, so the
// error type owns its HTTP mapping (0.1 routed it through a generated shim).
// The body shape — `{ kind, message }` — matches the frontend's
// `schema/error.ts::RpcErrorPayload`.
impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let (status, kind) = match &self {
            Error::NotFound(_) => (StatusCode::NOT_FOUND, "NotFound"),
            Error::InvalidInput(_) => (StatusCode::BAD_REQUEST, "InvalidInput"),
            Error::HttpClientError(_) => (StatusCode::BAD_GATEWAY, "HttpClient"),
            Error::TomlError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Toml"),
            Error::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database"),
            Error::ConfigReadError { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "ConfigRead"),
            Error::IoError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Io"),
            Error::MigrationError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Migration"),
            Error::AddressParseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "AddressParse"),
            Error::Generic(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Generic"),
            Error::JsonError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Json"),
        };
        let body = axum::Json(serde_json::json!({
            "kind": kind,
            "message": self.to_string(),
        }));
        (status, body).into_response()
    }
}

// ── Database ──

pub type Pool = sqlx::PgPool;

pub async fn create_pool(config: &Config) -> Result<sqlx::PgPool> {
    let url = if let Some(ref database_url) = config.database_url {
        database_url.clone()
    } else if let Ok(database_url) =
        std::env::var("CINEMA_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
    {
        database_url
    } else {
        panic!(
            "Database url not provided. Please set the `CINEMA_DATABASE_URL` environmental variable!"
        );
    };

    tracing::info!("connecting to database at {url}");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

// ── Storage ──

#[derive(Clone)]
pub struct Storage(Arc<PathBuf>);

impl Storage {
    pub fn path(&self) -> &Path {
        &self.0
    }
    pub fn join(&self, p: impl AsRef<Path>) -> PathBuf {
        self.0.join(p)
    }
}

pub async fn create_storage(config: &Config) -> Result<Storage> {
    let path = config.data_dir.join("fs");
    tokio::fs::create_dir_all(&path).await?;
    Ok(Storage(Arc::new(path)))
}

// ── App Context ──
//
// draad owns the transport: `EventBus` is the broadcast bus the generated
// emitters publish to, and `Conns` is its registry of live WebSocket sockets
// (keyed by the server-assigned `client_id`). Cinema owns the presence
// *payload*: `ClientRoster` maps each `client_id` to who that client is, for
// phone↔TV pairing. `FromRef` lets `session_handler` and generated
// `conn: &Conn` handlers pull these out of the composite state.

/// Presence payload roster, keyed by draad's server-assigned `client_id`.
#[derive(Clone, Default)]
pub struct ClientRoster(Arc<Mutex<HashMap<String, ClientPresence>>>);

impl ClientRoster {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn insert(&self, client_id: &str, presence: ClientPresence) {
        self.0.lock().await.insert(client_id.to_string(), presence);
    }
    pub async fn remove(&self, client_id: &str) {
        self.0.lock().await.remove(client_id);
    }
    pub async fn get(&self, client_id: &str) -> Option<ClientPresence> {
        self.0.lock().await.get(client_id).cloned()
    }
    pub async fn snapshot(&self) -> Vec<ClientPresence> {
        self.0.lock().await.values().cloned().collect()
    }
}

#[derive(Clone, FromRef)]
pub struct AppContext {
    pub db: Pool,
    pub storage: Storage,
    pub config: Arc<Config>,
    pub event_bus: draad::runtime::EventBus,
    pub events: crate::Events,
    /// draad's registry of live WS connections — server→client addressing and
    /// the basis for the `Caller` / `conn: &Conn` injection.
    pub conns: draad::runtime::Conns,
    /// Presence payload roster, keyed by `client_id`. Maintained by
    /// `crate::ws`; broadcast on `remote_presence`.
    pub clients: ClientRoster,
    pub http: Client,
    pub downloads: crate::downloads::Handle,
}
