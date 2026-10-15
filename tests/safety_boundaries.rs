use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    routing::{delete, get},
};
use serde_json::{Value, json};
use settlement::{billing, config::Config, http::App};
use sqlx::Row;
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const CSV_HEADER: &str = "Time,Billed Cost,Model,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens,Padding\n";
const CSV_ROW_PREFIX: &str = "2025-01-15T00:00:00.000000+00:00,1.00,fixture-model,10,20,0,0,\"";
const CSV_ROW_SUFFIX: &str = "\"\n";
const CSV_LIMIT: usize = 10 * 1024 * 1024;

fn csv_with_size(size: usize) -> String {
    let fixed = CSV_HEADER.len() + CSV_ROW_PREFIX.len() + CSV_ROW_SUFFIX.len();
    assert!(size >= fixed);
    format!(
        "{CSV_HEADER}{CSV_ROW_PREFIX}{}{CSV_ROW_SUFFIX}",
        "x".repeat(size - fixed)
    )
}

fn fixture_usage(id: i64, amount: &str) -> Value {
    json!({
        "id": id,
        "account_id": 100,
        "user_id": 1,
        "amount": amount,
        "created_at": "2025-01-15T00:00:00.000000+00:00"
    })
}

fn write_source(path: &Path, usage: Vec<Value>) {
    let value = json!({
        "accounts": [{
            "id": 100,
            "name": "Fixture root",
            "platform": "openai",
            "status": "active",
            "deleted": false,
            "parent_account_id": null,
            "created_at": "2024-12-01T00:00:00+00:00"
        }],
        "users": [{
            "id": 1,
            "name": "Fixture Admin",
            "role": "admin",
            "status": "active",
            "deleted": false
        }],
        "cleanups": [],
        "usage": usage
    });
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn config(directory: &Path, fixture: PathBuf, origin: String, verify_url: String) -> Config {
    Config {
        data_dir: directory.join("data"),
        source_url: String::new(),
        fixture: Some(fixture),
        origin,
        verify_url,
        bind: "127.0.0.1:0".parse().unwrap(),
        background: false,
        read_only: true,
        external_writes: false,
        test: true,
        revision: "test-fixture".to_owned(),
        assets: directory.join("assets"),
    }
}

async fn serve(
    listener: TcpListener,
    router: Router,
) -> (
    SocketAddr,
    CancellationToken,
    JoinHandle<std::io::Result<()>>,
) {
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let signal = stop.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
    });
    (address, stop, task)
}

async fn verify_identity() -> Json<Value> {
    Json(json!({
        "code": 0,
        "data": {
            "id": 1,
            "username": "Fixture Admin",
            "role": "admin",
            "status": "active"
        }
    }))
}

async fn count_external_delete(State(calls): State<Arc<AtomicUsize>>) -> StatusCode {
    calls.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK
}

#[test]
fn csv_enforces_ten_mib_byte_limit() {
    assert!(billing::parse_csv(&csv_with_size(CSV_LIMIT)).is_ok());
    assert!(billing::parse_csv(&csv_with_size(CSV_LIMIT + 1)).is_err());
}

#[test]
fn csv_accepts_fifty_thousand_rows_and_rejects_the_next_row() {
    let row = "2025-01-15T00:00:00.000000+00:00,1.00,fixture-model,10,20,0,0\n";
    let input = format!(
        "Time,Billed Cost,Model,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens\n{}",
        row.repeat(50_000)
    );
    let parsed = billing::parse_csv(&input).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 50_000);

    let over_limit = format!("{input}{row}");
    assert!(billing::parse_csv(&over_limit).is_err());
}

#[tokio::test]
async fn read_only_instance_rejects_mutations_and_skips_external_deletion() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_source(&fixture, vec![fixture_usage(1, "1.00")]);

    let delete_calls = Arc::new(AtomicUsize::new(0));
    let verify_router = Router::new()
        .route("/api/v1/auth/me", get(verify_identity))
        .route("/api/v1/admin/accounts/{id}", delete(count_external_delete))
        .with_state(delete_calls.clone());
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (upstream, stop_upstream, upstream_task) = serve(upstream_listener, verify_router).await;

    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_address = api_listener.local_addr().unwrap();
    let origin = format!("http://{api_address}");
    let app = App::new(config(
        temporary.path(),
        fixture,
        origin.clone(),
        format!("http://{upstream}"),
    ))
    .await
    .unwrap();
    let (served_at, stop_api, api_task) = serve(api_listener, app.router()).await;
    assert_eq!(served_at, api_address);

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let verifier_probe = client
        .get(format!("http://{upstream}/api/v1/auth/me"))
        .send()
        .await
        .unwrap();
    assert_eq!(verifier_probe.status(), StatusCode::OK);
    let sso = client
        .get(format!(
            "{origin}/settlement/sso/start?token=fixture-admin&user_id=1"
        ))
        .send()
        .await
        .unwrap();
    let sso_status = sso.status();
    let sso_headers = sso.headers().clone();
    let sso_body = sso.text().await.unwrap();
    assert_eq!(
        sso_status,
        StatusCode::SEE_OTHER,
        "SSO response: {sso_body}"
    );
    let cookie = sso_headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let identity = client
        .get(format!("{origin}/settlement/api/v1/me"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(identity.status(), StatusCode::OK);
    let csrf = identity.json::<Value>().await.unwrap()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();

    let mutation = client
        .post(format!("{origin}/settlement/api/v1/cycles"))
        .header(header::COOKIE, &cookie)
        .header("X-Settlement-User", "1")
        .header(header::ORIGIN, &origin)
        .header("X-CSRF-Token", csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(mutation.status(), StatusCode::FORBIDDEN);
    assert!(app.process_deletion(999).await.unwrap_err().status == 403);
    app.pending_deletions().await.unwrap();
    assert_eq!(delete_calls.load(Ordering::SeqCst), 0);

    stop_api.cancel();
    stop_upstream.cancel();
    api_task.await.unwrap().unwrap();
    upstream_task.await.unwrap().unwrap();
    app.service.source.close().await;
    app.service.store.close().await;
}

async fn count_rows(app: &App, table: &str) -> i64 {
    let mut db = app.service.store.read().await.unwrap();
    let count = sqlx::query(&format!("SELECT count(*) AS count FROM {table}"))
        .fetch_one(&mut *db)
        .await
        .unwrap()
        .try_get("count")
        .unwrap();
    db.commit().await.unwrap();
    count
}

#[tokio::test]
async fn sync_rebuilds_partial_durable_stage_before_publishing_source_rows() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_source(&fixture, vec![fixture_usage(1, "1.00")]);
    let app = App::new(config(
        temporary.path(),
        fixture.clone(),
        "https://example.invalid".to_owned(),
        "http://127.0.0.1:1".to_owned(),
    ))
    .await
    .unwrap();
    app.service.sync(true).await.unwrap();
    assert_eq!(count_rows(&app, "usage").await, 1);

    // Source batches are committed to rust_sync_stage independently. A killed
    // process can leave the same durable partial state produced here by a row
    // that fails normalization after the first 2048-row batch.
    let mut usage = (1..=2049)
        .map(|id| fixture_usage(id, "1.00"))
        .collect::<Vec<_>>();
    usage.last_mut().unwrap()["amount"] = json!("invalid-fixture-amount");
    write_source(&fixture, usage.clone());
    assert!(app.service.sync(true).await.is_err());
    assert_eq!(count_rows(&app, "rust_sync_stage").await, 2048);
    assert_eq!(count_rows(&app, "usage").await, 1);

    usage.last_mut().unwrap()["amount"] = json!("1.00");
    write_source(&fixture, usage);
    app.service.sync(true).await.unwrap();
    assert_eq!(count_rows(&app, "rust_sync_stage").await, 2049);
    assert_eq!(count_rows(&app, "usage").await, 2049);
    app.service.source.close().await;
    app.service.store.close().await;
}
