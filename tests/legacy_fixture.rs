use std::{
    collections::HashMap, net::TcpListener as PortReservation, path::Path, process::Stdio,
    time::Duration,
};

use axum::{Json, Router, extract::State, http::HeaderMap, routing::get};
use serde_json::{Value, json};
use sqlx::{
    Connection, Row,
    sqlite::{SqliteConnectOptions, SqliteConnection},
};
use tokio::{
    process::{Child, Command},
    task::JoinHandle,
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const FINANCIAL_TABLES: &[&str] = &[
    "cycles",
    "settlements",
    "settlement_usage",
    "orders",
    "batches",
    "netting_payments",
];

async fn connect(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .expect("open synthetic ledger")
}

async fn financial_rows(db: &mut SqliteConnection) -> Vec<Vec<Value>> {
    let mut output = Vec::new();
    for table in FINANCIAL_TABLES {
        output.push(
            sqlx::query(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .fetch_all(&mut *db)
                .await
                .expect("read financial fixture")
                .iter()
                .map(|row| settlement::store::row(row).unwrap())
                .collect(),
        );
    }
    output
}

async fn verify_identity(
    State(users): State<HashMap<String, Value>>,
    headers: HeaderMap,
) -> Json<Value> {
    let token = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    Json(match users.get(token) {
        Some(user) => json!({"code":0,"data":user}),
        None => json!({"code":1}),
    })
}

struct Runtime {
    child: Child,
    origin: String,
}

impl Runtime {
    async fn start(directory: &Path, upstream: &str, port: u16) -> Self {
        let origin = format!("http://127.0.0.1:{port}");
        let child = Command::new(env!("CARGO_BIN_EXE_settlement"))
            .env("DATA_DIR", directory)
            .env("SETTLEMENT_ENV", "test")
            .env("SETTLEMENT_TEST_NOW", "2025-04-01T00:00:00+00:00")
            .env("SETTLEMENT_BIND", format!("127.0.0.1:{port}"))
            .env("SETTLEMENT_BACKGROUND", "false")
            .env("SETTLEMENT_READ_ONLY", "false")
            .env("SETTLEMENT_EXTERNAL_WRITES", "false")
            .env("SOURCE_DATABASE_URL", "")
            .env("SOURCE_FIXTURE", directory.join("source.json"))
            .env("PUBLIC_ORIGIN", &origin)
            .env("SUB2API_VERIFY_URL", upstream)
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start isolated Rust service");
        let mut runtime = Self { child, origin };
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        for _ in 0..200 {
            if let Ok(response) = client
                .get(format!("{}/settlement/healthz", runtime.origin))
                .send()
                .await
                && response.status().is_success()
            {
                return runtime;
            }
            assert!(
                runtime.child.try_wait().unwrap().is_none(),
                "fixture service exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("fixture service did not become healthy");
    }

    async fn stop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            let status = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .await
                .unwrap();
            assert!(status.success());
        }
        #[cfg(not(unix))]
        self.child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .expect("bounded fixture shutdown")
            .expect("wait for fixture service");
        #[cfg(unix)]
        assert!(status.success());
        #[cfg(not(unix))]
        let _ = status;
    }
}

struct Identity {
    cookie: String,
    csrf: String,
}

async fn request(
    runtime: &Runtime,
    identity: &Identity,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> Value {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let mut request = client
        .request(
            method.parse().unwrap(),
            format!("{}/settlement/api/v1{path}", runtime.origin),
        )
        .header("Cookie", format!("settlement_session={}", identity.cookie))
        .header("Origin", &runtime.origin)
        .header("X-CSRF-Token", &identity.csrf)
        .header("X-Settlement-User", "1")
        .header("Idempotency-Key", key);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("synthetic HTTP request");
    let status = response.status();
    let value: Value = response.json().await.expect("JSON response");
    assert!(
        status.is_success(),
        "{method} {path}: status {status}, response {value}"
    );
    value
}

#[tokio::test]
async fn legacy_written_ledger_and_session_continue_across_rust_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let database = directory.join("settlement.sqlite3");
    let sql = std::fs::read_to_string(format!("{FIXTURES}/ledger/legacy-written.sql")).unwrap();
    let mut db = connect(&database).await;
    sqlx::raw_sql(&sql)
        .execute(&mut db)
        .await
        .expect("load actual legacy SQL dump");
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut db)
            .await
            .unwrap()
            .is_empty()
    );
    let before = financial_rows(&mut db).await;
    assert!(before[3].len() > 1, "sample contains legacy orders");

    let vectors: Value = serde_json::from_slice(
        &std::fs::read(format!("{FIXTURES}/session/legacy-restart.json")).unwrap(),
    )
    .unwrap();
    let vectors = vectors["vectors"].as_array().unwrap();
    assert!(vectors.iter().all(|v| v["synthetic"] == true));
    let key = vectors[0]["fernet_key"].as_str().unwrap();
    let cipher = fernet::Fernet::new(key).unwrap();
    std::fs::write(directory.join("session.key"), key).unwrap();
    std::fs::copy(
        format!("{FIXTURES}/ledger/legacy-written-source.json"),
        directory.join("source.json"),
    )
    .unwrap();
    let sessions =
        sqlx::query("SELECT s.*,u.name,u.role FROM sessions s JOIN users u ON u.id=s.user_id")
            .fetch_all(&mut db)
            .await
            .unwrap();
    let mut users = HashMap::new();
    let mut identity = None;
    for session in sessions {
        let sid: String = session.get("id");
        let uid: i64 = session.get("user_id");
        let encrypted: String = session.get("token");
        let token = String::from_utf8(
            cipher
                .decrypt(&encrypted)
                .expect("legacy ciphertext decrypts"),
        )
        .unwrap();
        users.insert(token, json!({"id":uid,"username":session.get::<String,_>("name"),"role":session.get::<String,_>("role"),"status":"active"}));
        if uid == 1 {
            let vector = vectors
                .iter()
                .find(|v| v["session_id_sha256"] == sid)
                .expect("old administrator cookie is frozen");
            let cookie = vector["cookie"].as_str().unwrap().to_owned();
            assert_eq!(settlement::json::hash(cookie.as_bytes()), sid);
            assert!(
                session.get::<f64, _>("expires_at") > 1_743_465_600.0,
                "old session remains valid at the fixture clock"
            );
            identity = Some(Identity {
                cookie,
                csrf: session.get("csrf"),
            });
        }
    }
    let identity = identity.expect("fixture administrator session");
    db.close().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let server: JoinHandle<()> = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/api/v1/auth/me", get(verify_identity))
                .with_state(users),
        )
        .await
        .unwrap();
    });
    let reserve = PortReservation::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let mut runtime = Runtime::start(directory, &upstream, port).await;
    assert_eq!(
        request(&runtime, &identity, "GET", "/me", None, "").await["id"],
        1
    );
    let expense = request(
        &runtime,
        &identity,
        "POST",
        "/expenses",
        Some(json!({
            "name":"Legacy continuation fixture", "cost":"17.03", "incurred_on":"2025-04-01",
            "creditor_id":2, "allocation_mode":"equal", "members":[{"user_id":2},{"user_id":3}]
        })),
        "legacy-continued-expense",
    )
    .await;
    let preview = request(
        &runtime,
        &identity,
        "POST",
        "/previews",
        Some(json!({"expense_ids":[expense["id"]],"end":"2025-04-01T00:00:00Z"})),
        "",
    )
    .await;
    let shares = preview["expenses"][0]["allocations"].as_array().unwrap();
    assert_eq!(
        shares.iter().find(|r| r["user_id"] == 2).unwrap()["amount_fen"],
        852
    );
    assert_eq!(
        shares.iter().find(|r| r["user_id"] == 3).unwrap()["amount_fen"],
        851
    );
    let payload = json!({"preview_id":preview["id"]});
    let settled = request(
        &runtime,
        &identity,
        "POST",
        "/settlements",
        Some(payload.clone()),
        "legacy-continued-settlement",
    )
    .await;
    assert_eq!(
        settled,
        request(
            &runtime,
            &identity,
            "POST",
            "/settlements",
            Some(payload.clone()),
            "legacy-continued-settlement"
        )
        .await
    );
    runtime.stop().await;
    let mut runtime = Runtime::start(directory, &upstream, port).await;
    assert_eq!(
        request(&runtime, &identity, "GET", "/me", None, "").await["id"],
        1
    );
    assert_eq!(
        settled,
        request(
            &runtime,
            &identity,
            "POST",
            "/settlements",
            Some(payload),
            "legacy-continued-settlement"
        )
        .await
    );
    runtime.stop().await;
    server.abort();
    let _ = server.await;

    let mut db = connect(&database).await;
    let after = financial_rows(&mut db).await;
    for ((table, original), mut current) in FINANCIAL_TABLES.iter().zip(before).zip(after) {
        if *table == "cycles" {
            for row in &mut current {
                assert_eq!(row.get("vendor_id"), Some(&Value::Null));
                assert_eq!(row.get("vendor_period_id"), Some(&Value::Null));
                let object = row.as_object_mut().expect("cycle row object");
                object.remove("vendor_id");
                object.remove("vendor_period_id");
            }
        }
        assert_eq!(
            &current[..original.len()],
            &original,
            "frozen old {table} rows changed"
        );
    }
    let orders =
        sqlx::query("SELECT amount_fen,payer_id,creditor_id FROM orders WHERE expense_id=?")
            .bind(expense["id"].as_i64().unwrap())
            .fetch_all(&mut db)
            .await
            .unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].get::<i64, _>("amount_fen"), 851);
    assert_eq!(orders[0].get::<i64, _>("payer_id"), 3);
    assert_eq!(orders[0].get::<i64, _>("creditor_id"), 2);
    let writes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM requests WHERE key='legacy-continued-settlement'")
            .fetch_one(&mut db)
            .await
            .unwrap();
    assert_eq!(writes, 1);
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut db)
            .await
            .unwrap()
            .is_empty()
    );
    db.close().await.unwrap();
}
