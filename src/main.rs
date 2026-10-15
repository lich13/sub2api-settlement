use futures_util::FutureExt;
use settlement::{config::Config, error::Result, http::App};
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio_util::sync::CancellationToken;

// Large bill results cross async and blocking workers. Share reclaimed pages
// across those workers instead of retaining a separate allocation high-water
// mark for each thread during repeated imports.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "settlement=info".into()),
        )
        .with_target(false)
        .init();
    if let Err(e) = run().await {
        tracing::error!(status = e.status, reason = %e.message, "service stopped");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let config = Config::env()?;
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        let address = if config.bind.ip().is_unspecified() {
            format!("127.0.0.1:{}", config.bind.port())
        } else {
            config.bind.to_string()
        };
        let response = reqwest::Client::new()
            .get(format!("http://{address}/settlement/healthz"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|_| settlement::error::Error::unavailable())?;
        if !response.status().is_success() {
            return Err(settlement::error::Error::unavailable());
        }
        return Ok(());
    }
    let app = App::new(config).await?;
    app.recover_deletions().await?;
    let _ = app.service.sync(true).await;
    let cancel = CancellationToken::new();
    let mut tasks = tokio::task::JoinSet::new();
    for kind in ["sync", "accounts", "deletions"] {
        if kind != "deletions" && !app.config.background {
            continue;
        }
        let app = app.clone();
        let cancel = cancel.clone();
        tasks.spawn(async move {
            let mut failures = 0;
            loop {
                if kind == "deletions" {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = app.deletion_wake.notified() => {},
                        _ = tokio::time::sleep(Duration::from_secs(5)), if app.config.background => {},
                    }
                }
                let tick = AssertUnwindSafe(async {
                    match kind {
                        "sync" => app.service.sync(false).await,
                        "accounts" => {
                            if app.service.inventory.refresh(false).await["error"].is_null() { Ok(()) }
                            else { Err(settlement::error::Error::unavailable()) }
                        },
                        _ => app.pending_deletions().await,
                    }
                }).catch_unwind().await;
                let ok = matches!(tick, Ok(Ok(())));
                if tick.is_err() {
                    tracing::error!(task=kind, status="restarting");
                    if kind == "deletions" { let _ = app.recover_deletions().await; }
                }
                failures = if ok { 0 } else { (failures + 1).min(4) };
                tracing::debug!(task=kind, failures, request_capacity=app.requests.available_permits(), calculation_capacity=app.service.cpu.available_permits());
                if kind != "deletions" {
                    let delay = if kind == "sync" { 60 + failures * 5 } else { 5 };
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(delay)) => {},
                    }
                }
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(app.config.bind).await?;
    let stop = cancel.clone();
    let server = axum::serve(listener, app.router()).with_graceful_shutdown(async move {
        shutdown_signal().await;
        stop.cancel();
    });
    let result = server.await;
    cancel.cancel();
    while let Some(result) = tasks.join_next().await {
        if result.is_err() {
            tracing::error!(task = "background", status = "stopped");
        }
    }
    app.service.source.close().await;
    app.service.store.close().await;
    result?;
    Ok(())
}
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal registration");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
