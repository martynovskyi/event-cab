use std::{future::IntoFuture, path::Path, process::ExitCode, time::Duration};

use events::{MIGRATOR, config::Config, openapi, routes, state::AppState};
use sqlx::postgres::PgPoolOptions;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: events [serve | healthcheck | openapi <dir>]";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["serve"] => serve().await,
        ["healthcheck"] => healthcheck().await,
        ["openapi", dir] => match openapi::write(Path::new(dir)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("failed to write OpenAPI documents to {dir}: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match run(cfg).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal");
            eprintln!("fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&cfg.database_url)
        .await
        .map_err(|e| format!("cannot connect to database: {e}"))?;
    MIGRATOR
        .run(&pool)
        .await
        .map_err(|e| format!("migrations failed: {e}"))?;

    let public = TcpListener::bind(("0.0.0.0", cfg.public_port)).await?;
    let admin = TcpListener::bind(("0.0.0.0", cfg.admin_port)).await?;
    let state = AppState::new(pool.clone(), cfg);

    let limiter = state.limiter.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(600));
        loop {
            tick.tick().await;
            limiter.prune();
        }
    });

    let (stop_tx, stop_rx) = watch::channel(());
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutdown requested, draining connections");
        let _ = stop_tx.send(());
    });
    let stopped = |mut rx: watch::Receiver<()>| async move {
        let _ = rx.changed().await;
    };

    tracing::info!(
        public = %public.local_addr()?,
        admin = %admin.local_addr()?,
        "listening"
    );
    tokio::try_join!(
        axum::serve(public, routes::public::router(state.clone()))
            .with_graceful_shutdown(stopped(stop_rx.clone()))
            .into_future(),
        axum::serve(admin, routes::admin::router(state))
            .with_graceful_shutdown(stopped(stop_rx))
            .into_future(),
    )?;
    pool.close().await;
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/// Docker healthcheck: GET 127.0.0.1:PUBLIC_PORT/health, exit 0 on 200.
async fn healthcheck() -> ExitCode {
    let port = std::env::var("PUBLIC_PORT")
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
        .unwrap_or(3001);
    let check = async {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await?;
        s.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut status = [0u8; 12];
        s.read_exact(&mut status).await?;
        Ok::<_, std::io::Error>(&status == b"HTTP/1.1 200")
    };
    match tokio::time::timeout(Duration::from_secs(5), check).await {
        Ok(Ok(true)) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
