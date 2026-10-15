use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode, header},
    routing::get,
};
use reqwest::{Client, Method, Response};
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

const END: &str = "2025-02-01T00:00:00+00:00";

struct Session {
    cookie: String,
    csrf: String,
    user_id: i64,
}

struct TestServer {
    app: App,
    client: Client,
    base: String,
    _temporary: TempDir,
    auth_task: JoinHandle<()>,
    api_task: JoinHandle<()>,
}

impl TestServer {
    async fn start() -> Self {
        let temporary = TempDir::new().expect("isolated test directory");
        let fixture_path = temporary.path().join("source.json");
        let mut source: Value = serde_json::from_str(include_str!("compat/source.json"))
            .expect("synthetic source fixture parses");
        source["usage"].as_array_mut().unwrap().push(json!({
            "id": 10,
            "account_id": 100,
            "user_id": 3,
            "amount": "3",
            "created_at": "2025-01-10T00:00:00+00:00",
            "request_id": "fixture-2",
            "model": "fixture-model",
            "input_tokens": 10,
            "output_tokens": 20,
            "cache_read_tokens": 0,
            "cache_creation_tokens": 0
        }));
        std::fs::write(&fixture_path, serde_json::to_vec(&source).unwrap())
            .expect("write isolated source fixture");

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

        let app = App::new(test_config(
            temporary.path(),
            fixture_path,
            format!("http://{auth_address}"),
        ))
        .await
        .expect("isolated app");
        app.service
            .sync(true)
            .await
            .expect("initial synthetic sync");

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
            app,
            client,
            base: format!("http://{address}/settlement/api/v1"),
            _temporary: temporary,
            auth_task,
            api_task,
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
        .expect("SSO session is saved");
        db.commit().await.unwrap();
        Session {
            cookie,
            csrf: row["csrf"].as_str().unwrap().to_owned(),
            user_id,
        }
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get(&self, session: Option<&Session>, path: &str) -> Response {
        let mut request = self.client.get(self.api_url(path));
        if let Some(session) = session {
            request = request
                .header(header::COOKIE, &session.cookie)
                .header("X-Settlement-User", session.user_id.to_string());
        }
        request.send().await.unwrap()
    }

    async fn post(&self, session: &Session, path: &str, body: &Value, key: &str) -> Response {
        self.client
            .request(Method::POST, self.api_url(path))
            .header(header::COOKIE, &session.cookie)
            .header(header::ORIGIN, "https://example.invalid")
            .header("X-Settlement-User", session.user_id.to_string())
            .header("X-CSRF-Token", &session.csrf)
            .header("Idempotency-Key", key)
            .json(body)
            .send()
            .await
            .unwrap()
    }

    async fn snapshot(&self) -> Value {
        let mut db = self.app.service.store.read().await.unwrap();
        let mut snapshot = serde_json::Map::new();
        for table in [
            "cycles",
            "settlements",
            "orders",
            "bill_imports",
            "bill_previews",
            "audit",
            "requests",
        ] {
            snapshot.insert(
                table.to_owned(),
                Value::Array(
                    all(
                        &mut db,
                        &format!("SELECT * FROM {table} ORDER BY rowid"),
                        &[],
                    )
                    .await
                    .unwrap(),
                ),
            );
        }
        db.commit().await.unwrap();
        Value::Object(snapshot)
    }

    async fn set_cycle_state(&self, cycle_id: i64, version: i64, status: &str) {
        let mut db = self.app.service.store.write().await.unwrap();
        settlement::store::exec(
            &mut db,
            "UPDATE cycles SET version=?,status=? WHERE id=?",
            &[json!(version), json!(status), json!(cycle_id)],
        )
        .await
        .unwrap();
        db.commit().await.unwrap();
    }

    async fn set_preview_expiry(&self, preview_id: &str, expires_at: &str) {
        let mut db = self.app.service.store.write().await.unwrap();
        settlement::store::exec(
            &mut db,
            "UPDATE bill_previews SET expires_at=? WHERE id=?",
            &[json!(expires_at), json!(preview_id)],
        )
        .await
        .unwrap();
        db.commit().await.unwrap();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.api_task.abort();
        self.auth_task.abort();
    }
}

async fn fake_auth(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let (id, username, role) = match token {
        "Bearer fixture-admin-token" => (1, "Fixture Admin", "admin"),
        "Bearer fixture-other-admin-token" => (6, "Fixture Other Admin", "admin"),
        "Bearer fixture-user-token" => (3, "Fixture User", "user"),
        _ => return (StatusCode::UNAUTHORIZED, Json(json!({"code": 1}))),
    };
    (
        StatusCode::OK,
        Json(json!({"code":0,"data":{"id":id,"username":username,"role":role,"status":"active"}})),
    )
}

fn test_config(directory: &Path, fixture: PathBuf, verify_url: String) -> Config {
    Config {
        data_dir: directory.join("data"),
        source_url: String::new(),
        fixture: Some(fixture),
        origin: "https://example.invalid".to_owned(),
        verify_url,
        bind: "127.0.0.1:0".parse().unwrap(),
        background: false,
        read_only: false,
        external_writes: false,
        test: true,
        revision: "unmatched-export-test".to_owned(),
        assets: directory.join("assets"),
    }
}

async fn create_cycle(server: &TestServer, account_id: i64, key: &str) -> i64 {
    let created = server
        .app
        .service
        .mutate(
            &json!({"id":1,"name":"Fixture Admin","role":"admin"}),
            key,
            "create",
            &json!({
                "account_id":account_id,
                "name":format!("Synthetic account {account_id}"),
                "cost":"100.00",
                "creditor_id":2,
                "start":"2025-01-01T00:00:00+00:00"
            }),
            Prepared::default(),
            None,
        )
        .await
        .expect("create synthetic cycle");
    created["id"].as_i64().unwrap()
}

#[tokio::test]
async fn unmatched_bill_csv_requires_a_fresh_owned_admin_preview_and_is_read_only() {
    let server = TestServer::start().await;
    let admin = server.login("fixture-admin-token", 1).await;
    let other_admin = server.login("fixture-other-admin-token", 6).await;
    let user = server.login("fixture-user-token", 3).await;
    let cycle_id = create_cycle(&server, 100, "unmatched-cycle").await;
    let other_cycle_id = create_cycle(&server, 200, "other-unmatched-cycle").await;

    let path = format!("/cycles/{cycle_id}/bill-previews");
    let csv = concat!(
        "Time,Billed Cost,Model,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens,Request ID\r\n",
        "2025-01-01T00:00:00Z,1.25,fixture-model,10,20,0,0,fixture-1\r\n",
        "2025-01-10T00:00:00Z,2.00,fixture-model,10,20,0,0,fixture-2\r\n",
        "2025-01-25T00:00:00Z,0.000000000000000123,fixture-model,0,0,0,0,\"=SUM(1,2)\"\r\n",
        "2025-02-01T00:00:00Z,0.03,fixture-model,0,0,0,0,@SUM(A1:A2)\r\n",
    );
    let created_preview = server
        .post(
            &admin,
            &path,
            &json!({
                "version":1,
                "end":END,
                "csv":csv,
                "file_name":"synthetic.csv"
            }),
            "unmatched-preview",
        )
        .await;
    assert_eq!(created_preview.status(), StatusCode::OK);
    let preview = created_preview.json::<Value>().await.unwrap();
    let preview_id = preview["id"].as_str().expect("preview id").to_owned();
    assert_eq!(preview["matched_count"], 1);
    assert_eq!(preview["unmatched_count"], 2);
    assert_eq!(preview["outside_count"], 1);

    let download_path = format!("/cycles/{cycle_id}/bill-previews/{preview_id}/unmatched.csv");
    let unauthenticated = server.get(None, &download_path).await;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let ordinary_user = server.get(Some(&user), &download_path).await;
    assert_eq!(ordinary_user.status(), StatusCode::FORBIDDEN);

    let other_admin_response = server.get(Some(&other_admin), &download_path).await;
    assert_eq!(other_admin_response.status(), StatusCode::NOT_FOUND);

    let mismatched_cycle = server
        .get(
            Some(&admin),
            &format!("/cycles/{other_cycle_id}/bill-previews/{preview_id}/unmatched.csv"),
        )
        .await;
    assert_eq!(mismatched_cycle.status(), StatusCode::NOT_FOUND);

    let before_download = server.snapshot().await;
    let response = server.get(Some(&admin), &download_path).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/csv; charset=utf-8"
    );
    assert!(
        response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("settlement-unmatched.csv")
    );
    let bytes = response.bytes().await.unwrap();
    assert!(bytes.starts_with(b"\xef\xbb\xbf"));
    let csv_text = std::str::from_utf8(&bytes[3..]).unwrap();
    assert!(csv_text.as_bytes().windows(2).any(|pair| pair == b"\r\n"));
    assert!(!csv_text.replace("\r\n", "").contains(['\r', '\n']));
    let mut reader = csv::ReaderBuilder::new().from_reader(csv_text.as_bytes());
    let headers = reader.headers().unwrap().clone();
    assert_eq!(headers.len(), 10);
    let rows = reader.records().map(Result::unwrap).collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);

    let ambiguous = rows
        .iter()
        .find(|row| row.get(3) == Some("fixture-2"))
        .expect("ambiguous detail included");
    assert_eq!(ambiguous.get(8), Some("2.00"));
    assert_eq!(ambiguous.get(9), Some("存在歧义"));

    let missing = rows
        .iter()
        .find(|row| row.get(3) == Some("'=SUM(1,2)"))
        .expect("missing formula detail included and escaped");
    assert_eq!(missing.get(8), Some("1.23E-16"));
    assert_eq!(missing.get(9), Some("未找到本地请求"));

    let outside = rows
        .iter()
        .find(|row| row.get(3) == Some("'@SUM(A1:A2)"))
        .expect("outside formula detail included and escaped");
    assert_eq!(outside.get(8), Some("0.03"));
    assert_eq!(outside.get(9), Some("周期范围外"));
    assert!(rows.iter().all(|row| row.get(3) != Some("fixture-1")));
    assert_eq!(
        server.snapshot().await,
        before_download,
        "download changes no ledger rows"
    );

    server.set_cycle_state(cycle_id, 2, "open").await;
    let stale_version = server.get(Some(&admin), &download_path).await;
    assert_eq!(stale_version.status(), StatusCode::CONFLICT);
    server.set_cycle_state(cycle_id, 1, "open").await;

    server
        .set_preview_expiry(&preview_id, "2000-01-01T00:00:00.000000+00:00")
        .await;
    let expired = server.get(Some(&admin), &download_path).await;
    assert_eq!(expired.status(), StatusCode::CONFLICT);
    server
        .set_preview_expiry(&preview_id, "2999-01-01T00:00:00.000000+00:00")
        .await;

    server.set_cycle_state(cycle_id, 1, "settled").await;
    let settled = server.get(Some(&admin), &download_path).await;
    assert_eq!(settled.status(), StatusCode::CONFLICT);
}
