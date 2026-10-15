use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, State},
    http::{HeaderMap, Request, StatusCode, header},
    routing::get,
};
use serde_json::{Value, json};
use settlement::{
    config::Config,
    http::App,
    json::dumps,
    metrics::{self, Metrics},
    service::operations::csv_bytes,
    store,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

const SOURCE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/http/upstream/source.initial.json"
);

struct TestServer {
    app: App,
    address: SocketAddr,
    client: reqwest::Client,
    verify_task: JoinHandle<()>,
    api_task: JoinHandle<()>,
    _temporary: TempDir,
}

impl TestServer {
    async fn start() -> Self {
        let verify_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let verify_address = verify_listener.local_addr().unwrap();
        let verify_task = tokio::spawn(async move {
            axum::serve(
                verify_listener,
                Router::new().route("/api/v1/auth/me", get(fake_auth)),
            )
            .await
            .unwrap();
        });

        let temporary = TempDir::new().unwrap();
        let app = App::new(app_config(
            temporary.path(),
            format!("http://{verify_address}"),
        ))
        .await
        .unwrap();
        let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = api_listener.local_addr().unwrap();
        let router = app
            .router()
            .into_make_service_with_connect_info::<SocketAddr>();
        let api_task = tokio::spawn(async move {
            axum::serve(api_listener, router).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        Self {
            app,
            address,
            client,
            verify_task,
            api_task,
            _temporary: temporary,
        }
    }

    async fn login(&self, token: &str, user_id: i64) -> String {
        let response = self
            .client
            .get(format!(
                "http://{}/settlement/sso/start?token={token}&user_id={user_id}",
                self.address
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    fn api_url(&self, path: &str) -> String {
        format!("http://{}/settlement/api/v1/{path}", self.address)
    }

    async fn close(self) {
        self.api_task.abort();
        let _ = self.api_task.await;
        self.verify_task.abort();
        let _ = self.verify_task.await;
        self.app.service.source.close().await;
        self.app.service.store.close().await;
    }
}

async fn fake_auth(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let (id, username, role) = match token {
        "Bearer fixture-admin-token" => (9, "Fixture Admin", "admin"),
        "Bearer fixture-user-token" => (7, "Fixture User", "user"),
        _ => return (StatusCode::UNAUTHORIZED, Json(json!({"code": 1}))),
    };
    (
        StatusCode::OK,
        Json(json!({
            "code": 0,
            "data": {"id": id, "username": username, "role": role, "status": "active"}
        })),
    )
}

fn app_config(directory: &Path, verify_url: String) -> Config {
    Config {
        data_dir: directory.join("data"),
        source_url: String::new(),
        fixture: Some(SOURCE_FIXTURE.into()),
        origin: "http://127.0.0.1:18082".into(),
        verify_url,
        bind: "127.0.0.1:0".parse().unwrap(),
        background: false,
        read_only: true,
        external_writes: false,
        test: true,
        revision: "operations-test".into(),
        assets: directory.join("assets"),
    }
}

async fn seed_audit(app: &App) -> Value {
    let original = json!({
        "message": "\t=SUM(1,2),\r\nfixture detail",
        "nested": {"marker": "audit-payload-must-stay-unchanged"}
    });
    let mut db = app.service.store.write().await.unwrap();
    for (id, action, created_at, payload) in [
        (
            1,
            "create_cycle",
            "2026-10-01T00:00:00.000000+00:00",
            json!({"cycle": "fixture-1"}),
        ),
        (
            2,
            "settle",
            "2026-10-02T00:00:00.000000+00:00",
            json!({"settlement": "fixture-2"}),
        ),
        (
            3,
            "settle",
            "2026-10-03T00:00:00.000000+00:00",
            original.clone(),
        ),
        (
            4,
            "create_cycle",
            "2026-10-04T00:00:00.000000+00:00",
            json!({"cycle": "fixture-4"}),
        ),
    ] {
        store::exec(
            &mut db,
            "INSERT INTO audit(id,actor_id,action,entity_id,payload,created_at) VALUES(?,?,?,?,?,?)",
            &[
                json!(id),
                json!(9),
                json!(action),
                json!(id * 10),
                json!(dumps(&payload)),
                json!(created_at),
            ],
        )
        .await
        .unwrap();
    }
    db.commit().await.unwrap();
    original
}

#[tokio::test]
async fn operations_routes_require_admin_and_audit_queries_page_filter_and_preserve_payload() {
    let server = TestServer::start().await;
    let original_payload = seed_audit(&server.app).await;
    let user_cookie = server.login("fixture-user-token", 7).await;
    let admin_cookie = server.login("fixture-admin-token", 9).await;

    for path in ["audit", "operations/status", "audit.csv"] {
        let response = server
            .client
            .get(server.api_url(path))
            .header(header::COOKIE, &user_cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }

    let status = server
        .client
        .get(server.api_url("operations/status"))
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);

    let first = server
        .client
        .get(server.api_url("audit"))
        .query(&[("limit", "2")])
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.json::<Value>().await.unwrap();
    assert_eq!(first["items"][0]["id"], 4);
    assert_eq!(first["items"][1]["id"], 3);
    assert_eq!(first["next_before"], 3);
    assert_eq!(first["items"][1]["payload"], original_payload);

    let second = server
        .client
        .get(server.api_url("audit"))
        .query(&[("limit", "2"), ("before", "3")])
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second = second.json::<Value>().await.unwrap();
    assert_eq!(second["items"][0]["id"], 2);
    assert_eq!(second["items"][1]["id"], 1);
    assert!(second["next_before"].is_null());
    let page_ids = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap())
        .map(|item| item["id"].as_i64().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(page_ids.len(), 4);

    let filtered = server
        .client
        .get(server.api_url("audit"))
        .query(&[
            ("action", "settle"),
            ("from", "2026-10-02T00:00:00.000Z"),
            ("to", "2026-10-04T00:00:00.000Z"),
        ])
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered = filtered.json::<Value>().await.unwrap();
    assert_eq!(filtered["items"].as_array().unwrap().len(), 2);
    assert_eq!(filtered["items"][0]["id"], 3);
    assert_eq!(filtered["items"][1]["id"], 2);
    assert_eq!(filtered["items"][0]["payload"], original_payload);

    for query in [
        vec![("from", "not-a-date"), ("to", "2026-10-04T00:00:00Z")],
        vec![
            ("from", "2026-10-04T00:00:00Z"),
            ("to", "2026-10-04T00:00:00Z"),
        ],
        vec![("action", "settle;drop")],
    ] {
        let response = server
            .client
            .get(server.api_url("audit"))
            .query(&query)
            .header(header::COOKIE, &admin_cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let exported = server
        .client
        .get(server.api_url("audit.csv"))
        .query(&[("action", "settle")])
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(exported.status(), StatusCode::OK);
    assert_eq!(
        exported.headers()[header::CONTENT_TYPE],
        "text/csv; charset=utf-8"
    );
    let bytes = exported.bytes().await.unwrap();
    assert!(bytes.starts_with(b"\xef\xbb\xbf"));
    let mut reader = csv::ReaderBuilder::new().from_reader(&bytes[3..]);
    let records = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].get(5), Some(dumps(&original_payload).as_str()));

    server.close().await;
}

#[test]
fn csv_prefixes_formulas_after_whitespace_controls_and_bom_without_losing_csv_cells() {
    let cells = [
        "=1+1,comma\nsecond line",
        " \r@SUM(1,2)",
        "\t+cmd",
        "\u{001b}-1",
        "\u{feff}@SUM(1,2)",
        "ordinary, comma\nand newline",
    ];
    let bytes = csv_bytes(
        &["value"],
        cells.iter().map(|cell| vec![(*cell).to_owned()]),
    )
    .unwrap();
    assert!(bytes.starts_with(b"\xef\xbb\xbf"));

    let mut reader = csv::ReaderBuilder::new().from_reader(&bytes[3..]);
    let actual = reader
        .records()
        .map(|record| record.unwrap().get(0).unwrap().to_owned())
        .collect::<Vec<_>>();
    let expected = cells
        .iter()
        .map(|cell| {
            if cell == &"ordinary, comma\nand newline" {
                (*cell).to_owned()
            } else {
                format!("'{cell}")
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

fn metrics_request(
    peer: Option<SocketAddr>,
    forwarded_for: Option<&str>,
    bearer: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();
    if let Some(value) = forwarded_for {
        request
            .headers_mut()
            .insert("X-Forwarded-For", value.parse().unwrap());
    }
    if let Some(value) = bearer {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {value}").parse().unwrap(),
        );
    }
    if let Some(peer) = peer {
        request.extensions_mut().insert(ConnectInfo(peer));
    }
    request
}

async fn metrics_body(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn metrics_default_off_checks_actual_peer_or_bearer_and_exposes_no_personal_data() {
    let temporary = TempDir::new().unwrap();
    let mut app = App::new(app_config(
        temporary.path(),
        "http://example.invalid".into(),
    ))
    .await
    .unwrap();
    assert!(!app.metrics.enabled);
    let disabled = metrics::endpoint(State(app.clone()), metrics_request(None, None, None))
        .await
        .unwrap_err();
    assert_eq!(disabled.status, StatusCode::NOT_FOUND.as_u16());

    let private_marker = "metrics-private-fixture@example.invalid";
    let mut db = app.service.store.write().await.unwrap();
    store::exec(
        &mut db,
        "INSERT INTO users(id,name,role,status) VALUES(9,?,'admin','active')",
        &[json!(private_marker)],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();

    let token = "fixture-metrics-token-with-at-least-thirty-two-characters";
    assert!(Metrics::new(true, Some("short".into())).is_err());
    assert!(Metrics::new(false, Some(String::new())).is_ok());
    app.metrics = Arc::new(Metrics::new(true, Some(token.into())).unwrap());
    app.metrics.conflict();
    app.metrics.conflict();
    let remote: SocketAddr = "192.0.2.20:43120".parse().unwrap();

    for request in [
        metrics_request(Some(remote), Some("127.0.0.1"), None),
        metrics_request(Some(remote), None, Some("wrong-token")),
    ] {
        let error = metrics::endpoint(State(app.clone()), request)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED.as_u16());
    }

    let output = metrics::endpoint(
        State(app.clone()),
        metrics_request(Some(remote), None, Some(token)),
    )
    .await
    .unwrap();
    assert_eq!(output.status(), StatusCode::OK);
    let output = metrics_body(output).await;
    assert!(output.contains("settlement_write_conflicts_total 2"));
    assert!(output.contains("settlement_pending_deletion_tasks 0"));
    assert!(!output.contains(private_marker));
    assert!(!output.contains(token));

    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40100);
    let output = metrics::endpoint(
        State(app.clone()),
        metrics_request(Some(loopback), Some("203.0.113.44"), None),
    )
    .await
    .unwrap();
    assert_eq!(output.status(), StatusCode::OK);
    app.service.source.close().await;
    app.service.store.close().await;
}
