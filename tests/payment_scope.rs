use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode, header},
    routing::get,
};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use settlement::{
    config::Config,
    http::App,
    json::hash,
    service::Prepared,
    store::{all, optional},
};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

const START: &str = "2025-01-01T00:00:00+00:00";
const END: &str = "2025-02-01T00:00:00+00:00";

fn admin() -> Value {
    json!({"id":1,"name":"Fixture Admin","role":"admin"})
}

fn payer() -> Value {
    json!({"id":3,"name":"Fixture Payer","role":"user"})
}

struct Session {
    cookie: String,
    csrf: String,
    user_id: i64,
}

struct ApiHarness {
    _temporary: TempDir,
    fixture_path: PathBuf,
    app: App,
    client: Client,
    base: String,
    auth_task: Option<JoinHandle<()>>,
    api_task: Option<JoinHandle<()>>,
}

impl ApiHarness {
    async fn new() -> Self {
        let temporary = TempDir::new().expect("isolated test directory");
        let fixture_path = temporary.path().join("source.json");
        std::fs::write(&fixture_path, include_str!("compat/source.json"))
            .expect("write synthetic source fixture");

        let auth_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let auth_address = auth_listener.local_addr().unwrap();
        let auth_task = tokio::spawn(async move {
            axum::serve(
                auth_listener,
                Router::new().route("/api/v1/auth/me", get(fake_auth)),
            )
            .await
            .unwrap();
        });

        let mut config = test_config(temporary.path(), fixture_path.clone());
        config.verify_url = format!("http://{auth_address}");
        let app = App::new(config).await.expect("isolated app");
        app.service.sync(true).await.expect("initial fixture sync");

        let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = api_listener.local_addr().unwrap();
        let router = app
            .router()
            .into_make_service_with_connect_info::<SocketAddr>();
        let api_task = tokio::spawn(async move {
            axum::serve(api_listener, router).await.unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        Self {
            _temporary: temporary,
            fixture_path,
            app,
            client,
            base: format!("http://{address}/settlement/api/v1"),
            auth_task: Some(auth_task),
            api_task: Some(api_task),
        }
    }

    async fn login(&self, token: &str, user_id: i64) -> Session {
        let response = self
            .client
            .get(format!(
                "http://{}/settlement/sso/start?token={token}&user_id={user_id}",
                self.base
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let raw = cookie.split_once('=').unwrap().1;
        let mut db = self.app.service.store.read().await.unwrap();
        let row = optional(
            &mut db,
            "SELECT csrf FROM sessions WHERE id=?",
            &[json!(hash(raw.as_bytes()))],
        )
        .await
        .unwrap()
        .expect("SSO session was saved");
        db.commit().await.unwrap();
        Session {
            cookie,
            csrf: row["csrf"].as_str().unwrap().to_owned(),
            user_id,
        }
    }

    async fn get(&self, session: &Session, path: &str) -> (StatusCode, Value) {
        self.request(Method::GET, session, path, None, None).await
    }

    async fn post(
        &self,
        session: &Session,
        path: &str,
        body: &Value,
        key: &str,
    ) -> (StatusCode, Value) {
        self.request(Method::POST, session, path, Some(body), Some(key))
            .await
    }

    async fn request(
        &self,
        method: Method,
        session: &Session,
        path: &str,
        body: Option<&Value>,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base, path))
            .header(header::COOKIE, &session.cookie)
            .header(header::ORIGIN, "https://example.invalid")
            .header("X-Settlement-User", session.user_id.to_string());
        if let Some(key) = key {
            request = request
                .header("X-CSRF-Token", &session.csrf)
                .header("Idempotency-Key", key)
                .json(body.expect("write requests include a JSON body"));
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn rows(&self, table: &str) -> Vec<Value> {
        assert!(
            [
                "cycles",
                "settlements",
                "orders",
                "netting_payments",
                "account_deletions",
                "rust_deletion_scopes",
                "rust_payment_notes"
            ]
            .contains(&table)
        );
        let mut db = self.app.service.store.read().await.unwrap();
        let rows = all(
            &mut db,
            &format!("SELECT * FROM {table} ORDER BY rowid"),
            &[],
        )
        .await
        .unwrap();
        db.commit().await.unwrap();
        rows
    }

    fn write_source(&self, source: &Value) {
        std::fs::write(&self.fixture_path, serde_json::to_vec(source).unwrap())
            .expect("write synthetic source fixture");
    }

    async fn restart_ledger(&mut self) -> App {
        if let Some(task) = self.api_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.auth_task.take() {
            task.abort();
            let _ = task.await;
        }
        let directory = self._temporary.path().to_owned();
        self.app.service.source.close().await;
        self.app.service.store.close().await;
        App::new(test_config(&directory, self.fixture_path.clone()))
            .await
            .expect("reopen isolated ledger")
    }
}

impl Drop for ApiHarness {
    fn drop(&mut self) {
        if let Some(task) = &self.api_task {
            task.abort();
        }
        if let Some(task) = &self.auth_task {
            task.abort();
        }
    }
}

fn test_config(directory: &Path, fixture: PathBuf) -> Config {
    Config {
        data_dir: directory.join("data"),
        source_url: String::new(),
        fixture: Some(fixture),
        origin: "https://example.invalid".to_owned(),
        verify_url: "http://127.0.0.1:1".to_owned(),
        bind: "127.0.0.1:0".parse().unwrap(),
        background: false,
        read_only: false,
        external_writes: false,
        test: true,
        revision: "payment-scope-test".to_owned(),
        assets: directory.join("assets"),
    }
}

async fn fake_auth(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let (id, username, role) = match token {
        "Bearer fixture-admin-token" => (1, "Fixture Admin", "admin"),
        "Bearer fixture-creditor-token" => (2, "Fixture Creditor", "user"),
        "Bearer fixture-payer-token" => (3, "Fixture Payer", "user"),
        "Bearer fixture-other-token" => (4, "Fixture Other", "user"),
        "Bearer fixture-stranger-token" => (5, "Fixture Stranger", "user"),
        _ => return (StatusCode::UNAUTHORIZED, Json(json!({"code":1}))),
    };
    (
        StatusCode::OK,
        Json(json!({"code":0,"data":{"id":id,"username":username,"role":role,"status":"active"}})),
    )
}

async fn create_expense(
    app: &App,
    name: &str,
    cost: &str,
    creditor_id: i64,
    members: &[i64],
    key: &str,
) -> Value {
    let payload = json!({
        "name": name,
        "cost": cost,
        "incurred_on": "2025-01-20",
        "creditor_id": creditor_id,
        "allocation_mode": "equal",
        "members": members.iter().map(|id| json!({"user_id":id,"percent":null})).collect::<Vec<_>>(),
    });
    app.service
        .mutate(
            &admin(),
            key,
            "create-expense",
            &payload,
            Prepared::default(),
            None,
        )
        .await
        .expect("create synthetic expense")
}

async fn settle_expense(app: &App, expense_id: i64, key: &str) {
    let preview = app
        .service
        .preview(
            &admin(),
            &json!({"ids":[],"expense_ids":[expense_id],"end":END}),
        )
        .await
        .expect("preview synthetic expense");
    let payload = json!({"preview_id":preview["id"],"renewals":[]});
    let prepared = app
        .service
        .prepare_settlement(&admin(), &payload)
        .await
        .expect("prepare synthetic expense settlement");
    app.service
        .mutate(&admin(), key, "settle", &payload, prepared, None)
        .await
        .expect("settle synthetic expense");
}

async fn create_netting_pair(app: &App, prefix: &str) {
    let forward = create_expense(
        app,
        "Forward cents",
        "0.03",
        2,
        &[3, 4],
        &format!("{prefix}-forward"),
    )
    .await;
    settle_expense(
        app,
        forward["id"].as_i64().unwrap(),
        &format!("{prefix}-forward-settle"),
    )
    .await;
    let reverse = create_expense(
        app,
        "Reverse cent",
        "0.01",
        3,
        &[2, 4],
        &format!("{prefix}-reverse"),
    )
    .await;
    settle_expense(
        app,
        reverse["id"].as_i64().unwrap(),
        &format!("{prefix}-reverse-settle"),
    )
    .await;
}

async fn create_cycle(app: &App, account_id: i64, key: &str) -> Value {
    app.service
        .mutate(
            &admin(),
            key,
            "create",
            &json!({"account_id":account_id,"name":format!("Fixture account {account_id}"),"cost":"100.00","creditor_id":2,"start":START}),
            Prepared::default(),
            None,
        )
        .await
        .expect("create isolated cycle")
}

async fn cycle_preview(web: &ApiHarness, session: &Session, cycle_id: i64, key: &str) -> Value {
    let (status, preview) = web
        .post(
            session,
            "/previews",
            &json!({"ids":[cycle_id],"expense_ids":[],"end":END}),
            key,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "preview response: {preview}");
    preview
}

#[tokio::test]
async fn payment_memos_validate_permissions_idempotency_immutability_and_reopen() {
    let mut web = ApiHarness::new().await;
    create_netting_pair(&web.app, "memo-payment").await;
    let payer_session = web.login("fixture-payer-token", 3).await;
    let creditor_session = web.login("fixture-creditor-token", 2).await;
    let stranger_session = web.login("fixture-stranger-token", 5).await;
    let admin_session = web.login("fixture-admin-token", 1).await;
    let balance = web
        .app
        .service
        .netting_balance(&payer(), 2, 3)
        .await
        .expect("fixture pair balance");
    for (key, body) in [
        (
            "memo-note-too-long",
            json!({"user_a":2,"user_b":3,"digest":balance["digest"],"note":"备".repeat(501)}),
        ),
        (
            "memo-reference-too-long",
            json!({"user_a":2,"user_b":3,"digest":balance["digest"],"reference":"号".repeat(129)}),
        ),
    ] {
        let (status, _) = web
            .post(&payer_session, "/netting/payments", &body, key)
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert!(web.rows("netting_payments").await.is_empty());

    let note = "备".repeat(500);
    let reference = "号".repeat(128);
    let payload =
        json!({"user_a":2,"user_b":3,"digest":balance["digest"],"note":note,"reference":reference});
    let (status, created) = web
        .post(
            &payer_session,
            "/netting/payments",
            &payload,
            "memo-payment-key",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "payment response: {created}");
    let (replay_status, replay) = web
        .post(
            &payer_session,
            "/netting/payments",
            &payload,
            "memo-payment-key",
        )
        .await;
    assert_eq!(replay_status, StatusCode::OK);
    assert_eq!(replay, created);
    let mut changed = payload.clone();
    changed["note"] = json!("不同备注");
    let (changed_status, _) = web
        .post(
            &payer_session,
            "/netting/payments",
            &changed,
            "memo-payment-key",
        )
        .await;
    assert_eq!(changed_status, StatusCode::CONFLICT);
    assert_eq!(web.rows("netting_payments").await.len(), 1);
    assert_eq!(web.rows("rust_payment_notes").await.len(), 1);

    let payment_id = created["id"].as_i64().unwrap();
    let (payer_status, detail) = web
        .get(&payer_session, &format!("/netting/payments/{payment_id}"))
        .await;
    assert_eq!(payer_status, StatusCode::OK);
    assert_eq!(detail["note"], note);
    assert_eq!(detail["reference"], reference);
    let (creditor_status, _) = web
        .get(
            &creditor_session,
            &format!("/netting/payments/{payment_id}"),
        )
        .await;
    assert_eq!(creditor_status, StatusCode::OK);
    let (stranger_status, _) = web
        .get(
            &stranger_session,
            &format!("/netting/payments/{payment_id}"),
        )
        .await;
    assert_eq!(stranger_status, StatusCode::NOT_FOUND);
    let (all_status, _) = web
        .get(&stranger_session, "/netting/payments?scope=all")
        .await;
    assert_eq!(all_status, StatusCode::FORBIDDEN);
    let (admin_status, all) = web.get(&admin_session, "/netting/payments?scope=all").await;
    assert_eq!(admin_status, StatusCode::OK);
    assert_eq!(all["payments"][0]["note"], note);

    let mut db = web.app.service.store.write().await.unwrap();
    let immutable = sqlx::query("UPDATE rust_payment_notes SET note='tampered' WHERE payment_id=?")
        .bind(payment_id)
        .execute(&mut *db)
        .await
        .expect_err("payment memo updates must be rejected");
    assert!(
        immutable
            .to_string()
            .contains("payment notes are immutable")
    );
    db.rollback().await.unwrap();

    create_netting_pair(&web.app, "legacy-payment").await;
    let balance = web
        .app
        .service
        .netting_balance(&payer(), 2, 3)
        .await
        .expect("legacy pair balance");
    let legacy_payload = json!({"user_a":2,"user_b":3,"digest":balance["digest"]});
    let (legacy_status, legacy_payment) = web
        .post(
            &payer_session,
            "/netting/payments",
            &legacy_payload,
            "legacy-payment-key",
        )
        .await;
    assert_eq!(
        legacy_status,
        StatusCode::OK,
        "legacy request: {legacy_payment}"
    );
    let (legacy_detail_status, legacy_detail) = web
        .get(
            &payer_session,
            &format!("/netting/payments/{}", legacy_payment["id"]),
        )
        .await;
    assert_eq!(legacy_detail_status, StatusCode::OK);
    assert!(legacy_detail.get("note").is_none());
    assert!(legacy_detail.get("reference").is_none());
    assert_eq!(web.rows("rust_payment_notes").await.len(), 1);

    let reopened = web.restart_ledger().await;
    let persisted = reopened
        .service
        .netting_payments(&payer(), "mine", Some(payment_id))
        .await
        .expect("payment notes survive reopening the ledger");
    assert_eq!(persisted["note"], note);
    assert_eq!(persisted["reference"], reference);
}

#[tokio::test]
async fn deletion_scope_is_root_family_scoped_and_rejects_stale_settlement_before_queueing() {
    let web = ApiHarness::new().await;
    let admin_session = web.login("fixture-admin-token", 1).await;
    let payer_session = web.login("fixture-payer-token", 3).await;

    let (scope_status, scope) = web
        .get(&admin_session, "/accounts/100/deletion-scope")
        .await;
    assert_eq!(scope_status, StatusCode::OK);
    let targets = scope["targets"].as_array().unwrap();
    assert_eq!(
        targets
            .iter()
            .map(|target| target["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [100, 101, 102]
    );
    assert_eq!(targets[0]["shadow"], false);
    assert!(targets[1..].iter().all(|target| target["shadow"] == true));
    let (other_scope_status, other_scope) = web
        .get(&admin_session, "/accounts/200/deletion-scope")
        .await;
    assert_eq!(other_scope_status, StatusCode::OK);
    assert_eq!(other_scope["targets"].as_array().unwrap().len(), 1);
    assert_eq!(other_scope["targets"][0]["id"], 200);
    let (user_status, _) = web
        .get(&payer_session, "/accounts/100/deletion-scope")
        .await;
    assert_eq!(user_status, StatusCode::FORBIDDEN);

    let cycle = create_cycle(&web.app, 100, "stale-scope-cycle").await;
    let cycle_id = cycle["id"].as_i64().unwrap();
    let cycle_key = cycle_id.to_string();
    let preview = cycle_preview(&web, &admin_session, cycle_id, "stale-scope-preview").await;
    let (noncanonical_status, _) = web
        .post(
            &admin_session,
            "/settlements",
            &json!({
                "preview_id":preview["id"],"renewals":[],"delete_cycle_ids":[cycle_id],
                "delete_scopes":{(format!("0{cycle_id}")):scope["digest"]}
            }),
            "noncanonical-scope-cycle-id",
        )
        .await;
    assert_eq!(noncanonical_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(web.rows("settlements").await.len(), 0);

    let mut source: Value =
        serde_json::from_slice(&std::fs::read(&web.fixture_path).unwrap()).unwrap();
    source["accounts"].as_array_mut().unwrap().push(json!({
        "id":103,"name":"Fixture child 103","platform":"openai","status":"active",
        "deleted":false,"parent_account_id":100,"created_at":"2025-01-01T00:00:00+00:00"
    }));
    web.write_source(&source);
    web.app
        .service
        .sync(true)
        .await
        .expect("sync newly observed shadow account");

    let stale_payload = json!({
        "preview_id":preview["id"],
        "renewals":[],
        "delete_cycle_ids":[cycle_id],
        "delete_scopes":{(cycle_key.clone()):scope["digest"]}
    });
    let (stale_status, stale_result) = web
        .post(
            &admin_session,
            "/settlements",
            &stale_payload,
            "stale-scope-settle",
        )
        .await;
    assert_eq!(
        stale_status,
        StatusCode::CONFLICT,
        "stale scope response: {stale_result}"
    );
    assert_eq!(web.rows("settlements").await.len(), 0);
    assert_eq!(web.rows("account_deletions").await.len(), 0);
    assert_eq!(web.rows("orders").await.len(), 0);
    assert_eq!(web.rows("cycles").await[0]["status"], "open");
    assert!(!web.app.config.external_writes);
    web.app
        .pending_deletions()
        .await
        .expect("external deletion is disabled in test config");
    assert_eq!(web.rows("account_deletions").await.len(), 0);
}

#[tokio::test]
async fn explicit_deletion_scope_is_saved_and_retry_can_reconfirm_while_legacy_requests_still_work()
{
    let web = ApiHarness::new().await;
    let admin_session = web.login("fixture-admin-token", 1).await;
    let (scope_status, scope) = web
        .get(&admin_session, "/accounts/100/deletion-scope")
        .await;
    assert_eq!(scope_status, StatusCode::OK);

    let cycle = create_cycle(&web.app, 100, "confirmed-delete-cycle").await;
    let cycle_id = cycle["id"].as_i64().unwrap();
    let preview = cycle_preview(&web, &admin_session, cycle_id, "confirmed-delete-preview").await;
    let cycle_key = cycle_id.to_string();
    let payload = json!({
        "preview_id":preview["id"],"renewals":[],"delete_cycle_ids":[cycle_id],
        "delete_scopes":{(cycle_key):scope["digest"]}
    });
    let (status, result) = web
        .post(
            &admin_session,
            "/settlements",
            &payload,
            "confirmed-delete-settle",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "settlement response: {result}");
    let job_id = result["deletions"][0]["id"].as_i64().unwrap();
    assert_eq!(result["deletions"][0]["status"], "queued");
    assert_eq!(web.rows("settlements").await.len(), 1);
    let saved_scope = web
        .rows("rust_deletion_scopes")
        .await
        .into_iter()
        .find(|row| row["deletion_id"] == job_id)
        .expect("confirmed deletion scope was saved");
    assert_eq!(saved_scope["digest"], scope["digest"]);
    assert_eq!(
        serde_json::from_str::<Value>(saved_scope["snapshot"].as_str().unwrap()).unwrap(),
        scope
    );
    assert!(!web.app.config.external_writes);
    web.app
        .pending_deletions()
        .await
        .expect("queued job cannot call upstream in test config");
    assert_eq!(web.rows("account_deletions").await[0]["status"], "queued");

    let mut db = web.app.service.store.write().await.unwrap();
    sqlx::query("UPDATE account_deletions SET status='failed' WHERE id=?")
        .bind(job_id)
        .execute(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    let mut source: Value =
        serde_json::from_slice(&std::fs::read(&web.fixture_path).unwrap()).unwrap();
    source["accounts"].as_array_mut().unwrap().push(json!({
        "id":103,"name":"Fixture child 103","platform":"openai","status":"active",
        "deleted":false,"parent_account_id":100,"created_at":"2025-01-01T00:00:00+00:00"
    }));
    web.write_source(&source);
    web.app
        .service
        .sync(true)
        .await
        .expect("sync newly observed shadow account");
    let (current_status, current_scope) = web
        .get(&admin_session, "/accounts/100/deletion-scope")
        .await;
    assert_eq!(current_status, StatusCode::OK);
    assert_ne!(current_scope["digest"], scope["digest"]);

    let retry_path = format!("/account-deletions/{job_id}/retry");
    let (stale_retry_status, _) = web
        .post(
            &admin_session,
            &retry_path,
            &json!({"version":1}),
            "retry-old-scope",
        )
        .await;
    assert_eq!(stale_retry_status, StatusCode::CONFLICT);
    assert_eq!(web.rows("account_deletions").await[0]["status"], "failed");
    let (retry_status, retried) = web
        .post(
            &admin_session,
            &retry_path,
            &json!({"version":1,"scope_digest":current_scope["digest"]}),
            "retry-confirm-new-scope",
        )
        .await;
    assert_eq!(retry_status, StatusCode::OK, "retry response: {retried}");
    assert_eq!(retried["status"], "queued");
    let updated_scope = web
        .rows("rust_deletion_scopes")
        .await
        .into_iter()
        .find(|row| row["deletion_id"] == job_id)
        .unwrap();
    assert_eq!(updated_scope["digest"], current_scope["digest"]);
    let updated_snapshot: Value =
        serde_json::from_str(updated_scope["snapshot"].as_str().unwrap()).unwrap();
    assert!(
        updated_snapshot["targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|target| target["id"] == 103)
    );
    web.app
        .pending_deletions()
        .await
        .expect("reconfirmed retry remains queued without external writes");
    assert_eq!(web.rows("account_deletions").await[0]["status"], "queued");

    let legacy_cycle = create_cycle(&web.app, 200, "legacy-delete-cycle").await;
    let legacy_cycle_id = legacy_cycle["id"].as_i64().unwrap();
    let legacy_preview = cycle_preview(
        &web,
        &admin_session,
        legacy_cycle_id,
        "legacy-delete-preview",
    )
    .await;
    let legacy_payload = json!({"preview_id":legacy_preview["id"],"renewals":[],"delete_cycle_ids":[legacy_cycle_id]});
    let (legacy_status, legacy_result) = web
        .post(
            &admin_session,
            "/settlements",
            &legacy_payload,
            "legacy-delete-settle",
        )
        .await;
    assert_eq!(
        legacy_status,
        StatusCode::OK,
        "legacy settlement: {legacy_result}"
    );
    let legacy_job_id = legacy_result["deletions"][0]["id"].as_i64().unwrap();
    assert!(
        web.rows("rust_deletion_scopes")
            .await
            .iter()
            .all(|row| row["deletion_id"] != legacy_job_id)
    );
    let mut db = web.app.service.store.write().await.unwrap();
    sqlx::query("UPDATE account_deletions SET status='failed' WHERE id=?")
        .bind(legacy_job_id)
        .execute(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    let legacy_retry_path = format!("/account-deletions/{legacy_job_id}/retry");
    let (legacy_retry_status, legacy_retry) = web
        .post(
            &admin_session,
            &legacy_retry_path,
            &json!({"version":1}),
            "legacy-retry-without-scope",
        )
        .await;
    assert_eq!(
        legacy_retry_status,
        StatusCode::OK,
        "legacy retry: {legacy_retry}"
    );
    assert_eq!(legacy_retry["status"], "queued");
}
