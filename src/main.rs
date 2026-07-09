use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use clap::Parser;
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

mod api;
mod app;
mod config;
mod downloads;
mod file_system;
mod logging;
mod proxy;
mod raw;
mod streams;
mod subtitles;
mod tmdb;
mod trailer;
mod transcodings;
mod utils;
mod ws;

use app::{AppContext, Result};
use config::Config;

draad::include_generated!(
    crate::AppContext,
    draad::runtime::EventBus,
    custom_ts = "custom"
);

#[derive(Parser)]
#[command(name = "cinema", about = "Cinema media server")]
struct Cli {
    /// Host address to bind to
    #[arg(long, env = "CINEMA_HOST")]
    host: Option<String>,

    /// Port to listen on
    #[arg(short, long, env = "CINEMA_PORT")]
    port: Option<u16>,

    /// Path to data directory
    #[arg(long, env = "CINEMA_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Database URL (e.g. sqlite:./data.db, postgres://user:pass@host/db)
    #[arg(long, env = "CINEMA_DATABASE_URL")]
    database_url: Option<String>,

    /// Path to config file
    #[arg(short, long, default_value = "cinema.toml", env = "CINEMA_CONFIG")]
    config: PathBuf,

    /// Run in development mode
    #[arg(long)]
    dev: bool,
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    match run().await {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<()> {
    // Raise the file descriptor limit for torrent peer connections + streaming
    #[cfg(unix)]
    {
        use std::io::Error;
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        unsafe {
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim);
        }
        rlim.rlim_cur = rlim.rlim_max.min(10240);
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) } != 0 {
            eprintln!(
                "Warning: could not raise file descriptor limit: {}",
                Error::last_os_error()
            );
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        // .event_format(logging::CinemaFormatter)
        .init();

    let cli = Cli::parse();

    let mut config = Config::from_file(&cli.config)?;
    config.apply_env_overrides();

    if let Some(host) = cli.host {
        config.host = host;
    }
    if let Some(port) = cli.port {
        config.port = port;
    }
    if let Some(data_dir) = cli.data_dir {
        config.data_dir = data_dir;
    }
    if cli.database_url.is_some() {
        config.database_url = cli.database_url;
    }

    let config = Arc::new(config);

    // Initialize core services
    let pool = app::create_pool(&config).await?;
    let storage = app::create_storage(&config).await?;
    let event_bus = draad::runtime::EventBus::new();
    let events = Events::new(event_bus.clone());
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let downloads_handle = downloads::Handle::new(
        pool.clone(),
        events.clone(),
        config.clone(),
        storage.clone(),
    );
    let transcodings_handle = transcodings::Handle::new(
        pool.clone(),
        events.clone(),
        downloads_handle.clone(),
        config.clone(),
        storage.clone(),
    );

    let ctx = AppContext {
        db: pool,
        storage,
        config: config.clone(),
        event_bus,
        events,
        conns: draad::runtime::Conns::new(),
        clients: app::ClientRoster::new(),
        http,
        downloads: downloads_handle,
        transcodings: transcodings_handle,
    };

    // Initialize torrent engine
    downloads::TorrentEngine::init(&ctx).await?;

    if let Err(err) = ctx.downloads.boot().await {
        tracing::error!(?err, "Download boot recovery failed");
    }

    if let Err(err) = ctx.transcodings.boot().await {
        tracing::error!(?err, "Pretranscoding boot recovery failed");
    }

    // Live-session idle reaper (drops HLS sessions the client hasn't
    // touched in 120s, freeing capacity slots for other work).
    let reaper_ctx = ctx.clone();
    let hls_session_reaper = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            reaper_ctx
                .transcodings
                .cleanup_idle_live(std::time::Duration::from_secs(120))
                .await;
        }
    });

    // stream stats
    let events = ctx.events.clone();
    tokio::spawn(crate::downloads::TorrentEngine::stream_stats_supervisor(
        events,
    ));

    // Build router
    let mut router = Router::new();

    // Mount schema-generated RPC routes (JSON one-shots).
    info!("mounting rpc at /api/rpc");
    router = router.nest("/api/rpc", rpc_router().with_state(ctx.clone()));

    // Mount the WebSocket bridge for server→client events.
    info!("mounting ws at /api/ws");
    router = router.nest("/api/ws", ws::router().with_state(ctx.clone()));

    // Mount the raw byte-serving routes (range video, HLS segments, image
    // proxy). Their paths come from the `#[draad::raw]` schema and are
    // absolute, so merge them flat rather than nesting under `/api`.
    info!("mounting raw byte routes");
    router = router.merge(raw::router().with_state(ctx.clone()));

    // Frontend: dev proxy or static files
    if cli.dev {
        // The vite dev server is started alongside the backend by `just dev`
        // (via concurrently); here we just proxy the UI through to it.
        let dev_port = 5174u16;
        info!("proxying ui → http://localhost:{dev_port}");
        let dev_proxy = proxy::DevProxy::new(dev_port);
        router = router.fallback(move |req: axum::extract::Request| {
            proxy::dev_proxy_handler(axum::extract::State(dev_proxy.clone()), req)
        });
    } else {
        let build_dir = PathBuf::from("frontend/build");
        if build_dir.exists() {
            info!("mounting ui at /");
            let fallback = ServeFile::new(build_dir.join("index.html"));
            let service = ServeDir::new(&build_dir)
                .append_index_html_on_directories(true)
                .fallback(fallback);
            router = router.fallback_service(service);
        }
    }

    // Start server
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("listening on http://{addr}");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    hls_session_reaper.abort();
    // Shutdown. `transcodings.shutdown()` also stops every live HLS
    // session, so ffmpeg children die before we drop the torrent engine.
    ctx.transcodings.shutdown().await;
    ctx.downloads.shutdown().await;
    downloads::TorrentEngine::get().shutdown().await;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("shutdown signal received, draining connections...");
}
