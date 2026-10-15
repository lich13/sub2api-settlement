use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use settlement::{
    config::Config,
    http::App,
    reminders::{ReminderConfig, Reminders, signature},
    store,
};
use std::{
    collections::VecDeque,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

const SOURCE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/http/upstream/source.initial.json"
);
const SECRET: &str = "fixture-reminder-secret-with-at-least-32-bytes";

#[derive(Clone, Debug)]
struct CapturedRequest {
    event: String,
    timestamp: String,
    signature: String,
    body: String,
}

#[derive(Default)]
struct HookState {
    requests: Mutex<Vec<CapturedRequest>>,
    responses: Mutex<VecDeque<StatusCode>>,
}

struct FakeHook {
    url: String,
    state: Arc<HookState>,
    task: JoinHandle<()>,
}

impl FakeHook {
    async fn start(responses: impl IntoIterator<Item = StatusCode>) -> Self {
        let state = Arc::new(HookState {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
        });
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/reminders", post(receive_reminder))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url: format!("http://{address}/reminders"),
            state,
            task,
        }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    async fn close(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn receive_reminder(
    State(state): State<Arc<HookState>>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let captured = CapturedRequest {
        event: headers
            .get("X-Settlement-Event")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        timestamp: headers
            .get("X-Settlement-Timestamp")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        signature: headers
            .get("X-Settlement-Signature")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        body,
    };
    state.requests.lock().unwrap().push(captured);
    state
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(StatusCode::OK)
}

struct TestApp {
    app: App,
    _temporary: TempDir,
}

impl TestApp {
    async fn new(read_only: bool, external_writes: bool) -> Self {
        let temporary = TempDir::new().unwrap();
        let app = App::new(config(temporary.path(), read_only, external_writes))
            .await
            .unwrap();
        Self {
            app,
            _temporary: temporary,
        }
    }

    fn enable_reminders(&mut self, url: &str) {
        self.app.reminders = Arc::new(
            Reminders::new(Some(ReminderConfig::new(url, SECRET, 0, true).unwrap())).unwrap(),
        );
    }

    async fn close(self) {
        self.app.service.source.close().await;
        self.app.service.store.close().await;
    }
}

fn config(directory: &Path, read_only: bool, external_writes: bool) -> Config {
    Config {
        data_dir: directory.join("data"),
        source_url: String::new(),
        fixture: Some(SOURCE_FIXTURE.into()),
        origin: "http://127.0.0.1:18082".into(),
        verify_url: "http://127.0.0.1:1".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        background: false,
        read_only,
        external_writes,
        test: true,
        revision: "reminders-test".into(),
        assets: directory.join("assets"),
    }
}

async fn seed_orders(app: &App, amounts: &[(i64, i64, i64)]) {
    let mut db = app.service.store.write().await.unwrap();
    for settlement_id in 1..=amounts.len() as i64 {
        store::exec(
            &mut db,
            "INSERT INTO settlements(id,cycle_id,expense_id,snapshot,digest,created_at) VALUES(?,NULL,NULL,'{}','fixture-digest',?)",
            &[json!(settlement_id), json!("2020-01-01T00:00:00.000000+00:00")],
        )
        .await
        .unwrap();
    }
    for (index, (payer_id, creditor_id, amount_fen)) in amounts.iter().enumerate() {
        let settlement_id = index as i64 + 1;
        store::exec(
            &mut db,
            "INSERT INTO orders(settlement_id,payer_id,creditor_id,payer_name,creditor_name,account_name,amount_fen,status,created_at) VALUES(?,?,?,?,?,?,?,'pending',?)",
            &[
                json!(settlement_id),
                json!(payer_id),
                json!(creditor_id),
                json!(format!("Synthetic User {payer_id}")),
                json!(format!("Synthetic User {creditor_id}")),
                json!("fixture-account"),
                json!(amount_fen),
                json!("2020-01-01T00:00:00.000000+00:00"),
            ],
        )
        .await
        .unwrap();
    }
    db.commit().await.unwrap();
}

async fn reminder_rows(app: &App) -> Vec<Value> {
    let mut db = app.service.store.read().await.unwrap();
    let rows = store::all(&mut db, "SELECT * FROM rust_reminders ORDER BY id", &[])
        .await
        .unwrap();
    drop(db);
    rows
}

async fn force_due(app: &App, status: &str) {
    let mut db = app.service.store.write().await.unwrap();
    store::exec(
        &mut db,
        "UPDATE rust_reminders SET status=?,next_attempt='2000-01-01T00:00:00.000000+00:00'",
        &[json!(status)],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
}

async fn set_orders_created_at(app: &App, created_at: &str) {
    let mut db = app.service.store.write().await.unwrap();
    store::exec(
        &mut db,
        "UPDATE orders SET created_at=?",
        &[json!(created_at)],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
}

#[tokio::test]
async fn reminders_are_disabled_by_default_without_database_or_webhook_writes() {
    let hook = FakeHook::start([]).await;
    let app = TestApp::new(false, true).await;
    seed_orders(&app.app, &[(7, 9, 12_400), (9, 7, 4_000)]).await;

    assert!(!app.app.reminders.enabled());
    app.app.reminders.tick(&app.app).await.unwrap();

    assert!(reminder_rows(&app.app).await.is_empty());
    assert!(hook.requests().is_empty());
    app.close().await;
    hook.close().await;
}

#[tokio::test]
async fn reminders_wait_24_hours_then_send_the_signed_private_net_amount_once_per_day() {
    let hook = FakeHook::start([StatusCode::OK]).await;
    let mut app = TestApp::new(false, true).await;
    app.enable_reminders(&hook.url);
    seed_orders(&app.app, &[(7, 9, 12_400), (9, 7, 4_000)]).await;

    let recent = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
    set_orders_created_at(&app.app, &recent).await;
    app.app.reminders.tick(&app.app).await.unwrap();
    assert!(hook.requests().is_empty());
    assert!(reminder_rows(&app.app).await.is_empty());

    set_orders_created_at(&app.app, "2020-01-01T00:00:00.000000+00:00").await;
    app.app.reminders.tick(&app.app).await.unwrap();
    app.app.reminders.tick(&app.app).await.unwrap();

    let requests = hook.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    let payload: Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(payload["event"], "pending_payment");
    assert_eq!(payload["event_id"].as_str(), Some(request.event.as_str()));
    assert_eq!(payload["payer_id"], 7);
    assert_eq!(payload["creditor_id"], 9);
    assert_eq!(payload["amount_fen"], 8_400);
    assert_eq!(payload["order_count"], 2);
    assert_eq!(
        payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "amount_fen",
            "created_at",
            "creditor_id",
            "currency",
            "event",
            "event_id",
            "order_count",
            "payer_id",
        ]
    );
    assert!(!request.body.contains("Synthetic User"));
    assert!(!request.body.contains("fixture-account"));
    assert!(!request.body.contains("token"));
    assert_eq!(
        request.signature,
        format!(
            "v1={}",
            signature(SECRET, &request.timestamp, &request.body)
        )
    );
    assert_eq!(request.event, payload["event_id"].as_str().unwrap());

    let rows = reminder_rows(&app.app).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "sent");
    assert_eq!(rows[0]["attempts"], 1);
    app.close().await;
    hook.close().await;
}

#[tokio::test]
async fn failed_delivery_reuses_event_and_recovery_requeues_an_interrupted_send() {
    let hook = FakeHook::start([StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK]).await;
    let mut app = TestApp::new(false, true).await;
    app.enable_reminders(&hook.url);
    seed_orders(&app.app, &[(7, 9, 12_400), (9, 7, 4_000)]).await;

    app.app.reminders.tick(&app.app).await.unwrap();
    let first = hook.requests();
    assert_eq!(first.len(), 1);
    assert_eq!(reminder_rows(&app.app).await[0]["status"], "queued");
    force_due(&app.app, "sending").await;
    app.app.reminders.recover(&app.app).await.unwrap();
    assert_eq!(reminder_rows(&app.app).await[0]["status"], "queued");
    app.app.reminders.tick(&app.app).await.unwrap();

    let requests = hook.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].event, requests[1].event);
    assert_eq!(requests[0].body, requests[1].body);
    let rows = reminder_rows(&app.app).await;
    assert_eq!(rows[0]["status"], "sent");
    assert_eq!(rows[0]["attempts"], 2);
    app.close().await;
    hook.close().await;
}

#[tokio::test]
async fn reminders_stop_after_five_failed_attempts() {
    let hook = FakeHook::start([StatusCode::SERVICE_UNAVAILABLE; 5]).await;
    let mut app = TestApp::new(false, true).await;
    app.enable_reminders(&hook.url);
    seed_orders(&app.app, &[(7, 9, 12_400), (9, 7, 4_000)]).await;

    for attempt in 0..5 {
        if attempt > 0 {
            force_due(&app.app, "queued").await;
        }
        app.app.reminders.tick(&app.app).await.unwrap();
    }
    app.app.reminders.tick(&app.app).await.unwrap();

    let requests = hook.requests();
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| request.event == requests[0].event)
    );
    let rows = reminder_rows(&app.app).await;
    assert_eq!(rows[0]["status"], "failed");
    assert_eq!(rows[0]["attempts"], 5);
    app.close().await;
    hook.close().await;
}

#[tokio::test]
async fn changed_or_paid_orders_and_zero_net_pairs_are_not_sent() {
    let hook = FakeHook::start([StatusCode::SERVICE_UNAVAILABLE]).await;
    let mut app = TestApp::new(false, true).await;
    app.enable_reminders(&hook.url);
    seed_orders(&app.app, &[(7, 9, 12_400), (9, 7, 4_000)]).await;
    app.app.reminders.tick(&app.app).await.unwrap();
    assert_eq!(hook.requests().len(), 1);

    let mut db = app.app.service.store.write().await.unwrap();
    store::exec(
        &mut db,
        "UPDATE orders SET amount_fen=6_000 WHERE id=2",
        &[],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
    force_due(&app.app, "queued").await;
    app.app.reminders.tick(&app.app).await.unwrap();
    assert_eq!(hook.requests().len(), 1);
    assert_eq!(reminder_rows(&app.app).await[0]["status"], "obsolete");
    app.close().await;
    hook.close().await;

    let hook = FakeHook::start([StatusCode::SERVICE_UNAVAILABLE]).await;
    let mut app = TestApp::new(false, true).await;
    app.enable_reminders(&hook.url);
    seed_orders(&app.app, &[(7, 9, 12_400), (9, 7, 4_000)]).await;
    app.app.reminders.tick(&app.app).await.unwrap();
    assert_eq!(hook.requests().len(), 1);

    let mut db = app.app.service.store.write().await.unwrap();
    store::exec(&mut db, "UPDATE orders SET status='paid'", &[])
        .await
        .unwrap();
    db.commit().await.unwrap();
    force_due(&app.app, "queued").await;
    app.app.reminders.tick(&app.app).await.unwrap();
    assert_eq!(hook.requests().len(), 1);
    assert_eq!(reminder_rows(&app.app).await[0]["status"], "obsolete");
    app.close().await;
    hook.close().await;

    let hook = FakeHook::start([]).await;
    let mut app = TestApp::new(false, true).await;
    app.enable_reminders(&hook.url);
    seed_orders(&app.app, &[(7, 9, 4_000), (9, 7, 4_000)]).await;
    app.app.reminders.tick(&app.app).await.unwrap();
    assert!(hook.requests().is_empty());
    assert!(reminder_rows(&app.app).await.is_empty());
    app.close().await;
    hook.close().await;
}

#[tokio::test]
async fn reminders_respect_read_only_and_external_write_boundaries() {
    for (read_only, external_writes) in [(true, true), (false, false)] {
        let hook = FakeHook::start([]).await;
        let mut app = TestApp::new(read_only, external_writes).await;
        app.enable_reminders(&hook.url);
        seed_orders(&app.app, &[(7, 9, 12_400), (9, 7, 4_000)]).await;

        app.app.reminders.recover(&app.app).await.unwrap();
        app.app.reminders.tick(&app.app).await.unwrap();

        assert!(hook.requests().is_empty());
        assert!(reminder_rows(&app.app).await.is_empty());
        app.close().await;
        hook.close().await;
    }
}

#[test]
fn reminder_webhook_requires_https_except_for_test_loopback_urls() {
    assert!(ReminderConfig::new("http://127.0.0.1:8123/reminders", SECRET, 0, true).is_ok());
    assert!(ReminderConfig::new("http://127.0.0.1:8123/reminders", SECRET, 0, false).is_err());
    assert!(ReminderConfig::new("http://example.invalid/reminders", SECRET, 0, true).is_err());
    assert!(ReminderConfig::new("https://example.invalid/reminders", SECRET, 0, false).is_ok());
    assert!(
        ReminderConfig::new(
            "https://user:pass@example.invalid/reminders",
            SECRET,
            0,
            false
        )
        .is_err()
    );
}
