use std::path::{Path, PathBuf};
use std::sync::Arc;

use reqwest::Client;
use serde::Serialize;
use tokio::sync::broadcast;

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

// ── Database ──

pub type Pool = sqlx::AnyPool;

pub async fn create_pool(config: &Config) -> Result<Pool> {
    sqlx::any::install_default_drivers();

    let url = if let Some(ref database_url) = config.database_url {
        database_url.clone()
    } else if let Ok(database_url) =
        std::env::var("CINEMA_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
    {
        database_url
    } else {
        let dir = config.data_dir.join("db");
        tokio::fs::create_dir_all(&dir).await?;
        let db_path = dir.join("data.db");
        format!("sqlite:{}?mode=rwc", db_path.display())
    };

    tracing::info!("connecting to database at {url}");

    let pool = sqlx::any::AnyPoolOptions::new()
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

// ── Event Bus ──

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct Event {
    pub topic: String,
    pub payload: serde_json::Value,
}

#[derive(Clone)]
pub struct EventBus(Arc<broadcast::Sender<Event>>);

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self(Arc::new(tx))
    }

    pub fn publish(&self, topic: impl Into<String>, payload: impl Serialize) {
        let event = Event {
            topic: topic.into(),
            payload: serde_json::to_value(payload).unwrap(),
        };
        let _ = self.0.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.subscribe()
    }
}

// ── App Context ──

#[derive(Clone)]
pub struct AppContext {
    pub db: Pool,
    pub storage: Storage,
    pub config: Arc<Config>,
    pub events: EventBus,
    pub http: Client,
}
