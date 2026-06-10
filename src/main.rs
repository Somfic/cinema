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
pub(crate) mod downloads;
mod file_system;
mod hls;
mod logging;
mod proxy;
mod raw;
mod streams;
mod subtitles;
mod tmdb;
mod trailer;
mod ws;

use app::{AppContext, EventBus, Result};
use config::Config;

draad::include_generated!(crate::AppContext, crate::EventBus);

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
        .event_format(logging::CinemaFormatter)
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
    let events = app::EventBus::new();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let (downloads_handle, manager) =
        downloads::DownloadManager::new(pool.clone(), config.clone(), http.clone());

    let ctx = AppContext {
        db: pool,
        storage,
        config: config.clone(),
        events,
        presence: app::Presence::default(),
        http,
        downloads: downloads_handle,
    };

    // Initialize torrent engine
    downloads::TorrentEngine::init(&ctx).await?;

    // Start download manager
    tokio::spawn(manager.run());

    // HLS session cleanup reaper
    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            hls::cleanup_idle(120).await;
        }
    });

    // stream stats
    {
        let events = Events::new(ctx.events.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(333)).await;
                let engine = downloads::TorrentEngine::get();
                for hash in engine.active_info_hashes() {
                    let Ok(stats) = engine.stats(&hash) else {
                        continue;
                    };
                    let (download_speed_mbps, peers) = match &stats.live {
                        Some(live) => (live.download_speed.mbps, live.snapshot.peer_stats.live),
                        None => (0.0, 0),
                    };
                    events.streams.emit_stats(&api::streams::StreamStatsUpdate {
                        info_hash: hash,
                        progress_bytes: stats.progress_bytes,
                        total_bytes: stats.total_bytes,
                        download_speed_mbps,
                        peers,
                        finished: stats.finished,
                    });
                }
                // Piece bitmaps for files currently being streamed. Only
                // pushed for active streams (not every file in every
                // torrent), keeping the wire chatter bounded.
                for (hash, file_idx) in engine.active_streams().await {
                    let Ok(pieces) = engine.piece_map(&hash, file_idx, 200) else {
                        continue;
                    };
                    events.streams.emit_pieces(&api::streams::PiecesUpdate {
                        info_hash: hash,
                        file_idx: file_idx as i64,
                        pieces,
                    });
                }
            }
        });
    }

    // Build router
    let mut router = Router::new();

    // Mount schema-generated RPC routes (JSON one-shots).
    info!("mounting rpc at /api/rpc");
    router = router.nest("/api/rpc", rpc_router().with_state(ctx.clone()));

    // Mount the WebSocket bridge for server→client events.
    info!("mounting ws at /api/ws");
    router = router.nest("/api/ws", ws::router().with_state(ctx.clone()));

    // Mount the raw HTTP endpoints (range video, HLS segments, image proxy).
    info!("mounting raw routes at /api");
    router = router.nest("/api", raw::router().with_state(ctx.clone()));

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

    // Shutdown
    hls::stop_all().await;
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
