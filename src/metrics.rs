use crate::{
    error::{Error, Result},
    http::App,
    json::int,
    time,
};
use axum::{
    extract::{ConnectInfo, Request, State},
    response::{IntoResponse, Response},
};
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
};
use subtle::ConstantTimeEq;

#[derive(Default)]
pub struct Metrics {
    pub enabled: bool,
    token: Option<String>,
    conflicts: AtomicU64,
}
impl Metrics {
    pub fn environment(test: bool) -> Result<Self> {
        if test {
            return Ok(Self::default());
        }
        Self::new(
            std::env::var("SETTLEMENT_METRICS_ENABLED").as_deref() == Ok("true"),
            std::env::var("SETTLEMENT_METRICS_TOKEN")
                .ok()
                .filter(|value| !value.is_empty()),
        )
    }
    pub fn new(enabled: bool, token: Option<String>) -> Result<Self> {
        let token = token.filter(|value| !value.is_empty());
        if token.as_ref().is_some_and(|v| {
            v.len() < 32 || v.len() > 1024 || !v.is_ascii() || v.chars().any(char::is_whitespace)
        }) {
            return Err(Error::invalid(
                "监控令牌须为 32 至 1024 位非空白 ASCII 字符",
            ));
        }
        Ok(Self {
            enabled,
            token,
            conflicts: AtomicU64::new(0),
        })
    }
    pub fn conflict(&self) {
        self.conflicts.fetch_add(1, Ordering::Relaxed);
    }
    fn authorize(&self, request: &Request) -> Result<()> {
        if !self.enabled {
            return Err(Error::new(404, "Not Found"));
        }
        if request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_some_and(|peer| peer.0.ip().is_loopback())
        {
            return Ok(());
        }
        let provided = request
            .headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !self
            .token
            .as_ref()
            .is_some_and(|v| bool::from(v.as_bytes().ct_eq(provided.as_bytes())))
        {
            return Err(Error::new(401, "监控认证失败"));
        }
        Ok(())
    }
}
pub async fn endpoint(State(app): State<App>, request: Request) -> Result<Response> {
    app.metrics.authorize(&request)?;
    let status = app
        .service
        .operation_status(&serde_json::json!({"role":"admin"}))
        .await?;
    let last = status["last_sync"]
        .as_str()
        .and_then(|s| time::parse(s).ok())
        .map(|t| t.timestamp())
        .unwrap_or(0);
    let lag = if last > 0 {
        (time::epoch() as i64 - last).max(0)
    } else {
        0
    };
    let values = [
        (
            "settlement_sync_lag_seconds",
            "Seconds since the last successful usage sync; zero before the first sync.",
            "gauge",
            lag as u64,
        ),
        (
            "settlement_last_successful_sync_timestamp_seconds",
            "Unix timestamp of the last successful usage sync; zero if never synchronized.",
            "gauge",
            last.max(0) as u64,
        ),
        (
            "settlement_sync_healthy",
            "One when a successful sync exists and no sync error is pending.",
            "gauge",
            u64::from(last > 0 && status["sync_error"].is_null()),
        ),
        (
            "settlement_pending_deletion_tasks",
            "Account deletion tasks not completed or cancelled.",
            "gauge",
            int(&status, "pending_deletions") as u64,
        ),
        (
            "settlement_write_conflicts_total",
            "HTTP write conflicts, including version, stale preview and idempotency conflicts, since process start.",
            "counter",
            app.metrics.conflicts.load(Ordering::Relaxed),
        ),
    ];
    let output = values
        .iter()
        .map(|(name, help, kind, value)| {
            format!("# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n")
        })
        .collect::<String>();
    Ok((
        [("Content-Type", "text/plain; version=0.0.4; charset=utf-8")],
        output,
    )
        .into_response())
}
