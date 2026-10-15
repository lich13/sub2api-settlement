use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde_json::{Value, json};
use settlement::{
    config::Config,
    http::App,
    json::hash,
    keys::{self, Keyring, LedgerLock},
    rate::Limiter,
    store::{self, Store},
};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};
use tempfile::TempDir;
use tokio::net::TcpListener;

const LEGACY_SESSION: &str = include_str!("fixtures/session/legacy-restart.json");
const SOURCE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/http/upstream/source.initial.json"
);

fn private_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600,
            "{} must be private",
            path.display()
        );
    }
}

async fn open_store(directory: &Path) -> Store {
    Store::open(&directory.join("settlement.sqlite3"))
        .await
        .unwrap()
}

async fn insert_session(store: &Store, id: &str, token: &str, csrf: &str) {
    let mut db = store.write().await.unwrap();
    store::exec(
        &mut db,
        "INSERT INTO sessions(id,user_id,token,csrf,expires_at) VALUES(?,7,?,?,4102444800)",
        &[json!(id), json!(token), json!(csrf)],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
}

async fn read_session(store: &Store, id: &str) -> Value {
    let mut db = store.read().await.unwrap();
    let value = store::optional(&mut db, "SELECT * FROM sessions WHERE id=?", &[json!(id)])
        .await
        .unwrap()
        .unwrap();
    db.commit().await.unwrap();
    value
}

async fn key_id(store: &Store, id: &str) -> Option<String> {
    let mut db = store.read().await.unwrap();
    let value = store::optional(
        &mut db,
        "SELECT key_id FROM rust_session_keys WHERE session_id=?",
        &[json!(id)],
    )
    .await
    .unwrap()
    .map(|value| value["key_id"].as_str().unwrap().to_owned());
    db.commit().await.unwrap();
    value
}

fn pending_document(old_key: &str, new_key: &str) -> Value {
    json!({
        "version": 1,
        "active": "rotation-new",
        "keys": {
            "legacy-test": old_key,
            "rotation-new": new_key
        }
    })
}

#[tokio::test]
async fn legacy_fernet_rotation_preserves_cookie_csrf_and_records_active_key() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("data");
    std::fs::create_dir_all(&directory).unwrap();

    let vectors: Value = serde_json::from_str(LEGACY_SESSION).unwrap();
    let vector = &vectors["vectors"][0];
    assert_eq!(vector["synthetic"], true);
    let old_key = vector["fernet_key"].as_str().unwrap();
    let old_ciphertext = vector["fernet_ciphertext"].as_str().unwrap();
    let cookie = vector["cookie"].as_str().unwrap();
    let session_id = vector["session_id_sha256"].as_str().unwrap();
    let csrf = vector["csrf"].as_str().unwrap();
    assert_eq!(hash(cookie.as_bytes()), session_id);

    std::fs::write(directory.join("session.key"), old_key).unwrap();
    let store = open_store(&directory).await;
    insert_session(&store, session_id, old_ciphertext, csrf).await;

    let before = read_session(&store, session_id).await;
    let ring = Keyring::load(&directory, &store).await.unwrap();
    assert_eq!(ring.decrypt(old_ciphertext).unwrap(), b"fixture-user-1");

    let lock = LedgerLock::acquire(&directory).unwrap();
    keys::rotate(&directory, &store, "rotation-1")
        .await
        .unwrap();
    drop(lock);

    let ring = Keyring::load(&directory, &store).await.unwrap();
    let after = read_session(&store, session_id).await;
    let active_raw_key = std::fs::read_to_string(directory.join("session.key")).unwrap();
    let active_cipher = fernet::Fernet::new(active_raw_key.trim()).unwrap();

    assert_eq!(ring.active, "rotation-1");
    assert_eq!(after["id"], before["id"]);
    assert_eq!(after["csrf"], before["csrf"]);
    assert_eq!(after["csrf"], csrf);
    assert_eq!(
        String::from_utf8(
            active_cipher
                .decrypt(after["token"].as_str().unwrap())
                .unwrap()
        )
        .unwrap(),
        "fixture-user-1"
    );
    assert_eq!(ring.decrypt(old_ciphertext).unwrap(), b"fixture-user-1");
    assert_eq!(
        key_id(&store, session_id).await.as_deref(),
        Some("rotation-1")
    );
    assert!(!directory.join("session.keys.pending.json").exists());
    private_file(&directory.join("session.key"));
    private_file(&directory.join("session.keys.json"));
    private_file(&directory.join("settlement.lock"));

    store.close().await;
}

#[tokio::test]
async fn pending_journal_recovers_before_and_after_each_durable_rotation_step() {
    for stage in ["journal-only", "database-committed", "raw-key-replaced"] {
        let temporary = TempDir::new().unwrap();
        let directory = temporary.path().join("data");
        std::fs::create_dir_all(&directory).unwrap();
        let old_key = fernet::Fernet::generate_key();
        let new_key = fernet::Fernet::generate_key();
        let old_cipher = fernet::Fernet::new(&old_key)
            .unwrap()
            .encrypt(b"recovery-token");
        let new_cipher = fernet::Fernet::new(&new_key)
            .unwrap()
            .encrypt(b"recovery-token");
        let session_id = hash(format!("cookie-{stage}").as_bytes());
        let csrf = format!("csrf-{stage}");

        std::fs::write(directory.join("session.key"), &old_key).unwrap();
        let store = open_store(&directory).await;
        insert_session(&store, &session_id, &old_cipher, &csrf).await;
        keys::atomic_private(
            &directory.join("session.keys.pending.json"),
            &serde_json::to_vec(&pending_document(&old_key, &new_key)).unwrap(),
        )
        .unwrap();

        if stage != "journal-only" {
            let mut db = store.write().await.unwrap();
            store::exec(
                &mut db,
                "UPDATE sessions SET token=? WHERE id=?",
                &[json!(new_cipher), json!(session_id)],
            )
            .await
            .unwrap();
            store::exec(
                &mut db,
                "INSERT INTO rust_session_keys(session_id,key_id) VALUES(?, 'rotation-new')",
                &[json!(session_id)],
            )
            .await
            .unwrap();
            db.commit().await.unwrap();
        }
        if stage == "raw-key-replaced" {
            keys::atomic_private(&directory.join("session.key"), new_key.as_bytes()).unwrap();
        }

        let ring = Keyring::load(&directory, &store).await.unwrap();
        let recovered = read_session(&store, &session_id).await;
        let raw_key = std::fs::read_to_string(directory.join("session.key")).unwrap();
        let raw_cipher = fernet::Fernet::new(raw_key.trim()).unwrap();

        assert_eq!(ring.active, "rotation-new", "stage: {stage}");
        assert_eq!(recovered["csrf"], csrf, "stage: {stage}");
        assert_eq!(
            String::from_utf8(
                raw_cipher
                    .decrypt(recovered["token"].as_str().unwrap())
                    .unwrap()
            )
            .unwrap(),
            "recovery-token",
            "stage: {stage}"
        );
        assert_eq!(ring.decrypt(&old_cipher).unwrap(), b"recovery-token");
        assert_eq!(
            key_id(&store, &session_id).await.as_deref(),
            Some("rotation-new"),
            "stage: {stage}"
        );
        assert!(!directory.join("session.keys.pending.json").exists());
        store.close().await;
    }
}

#[tokio::test]
async fn failed_rotation_transaction_keeps_sessions_wholly_on_old_key() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("data");
    std::fs::create_dir_all(&directory).unwrap();
    let old_key = fernet::Fernet::generate_key();
    std::fs::write(directory.join("session.key"), &old_key).unwrap();
    let store = open_store(&directory).await;
    let session_id = hash(b"failure-cookie");
    let csrf = "failure-csrf";
    let old_cipher = fernet::Fernet::new(&old_key)
        .unwrap()
        .encrypt(b"unchanged-token");
    insert_session(&store, &session_id, &old_cipher, csrf).await;

    let mut db = store.write().await.unwrap();
    store::exec(
        &mut db,
        "CREATE TRIGGER reject_rotation BEFORE UPDATE OF token ON sessions BEGIN SELECT RAISE(ABORT, 'fixture failure'); END",
        &[],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();

    let lock = LedgerLock::acquire(&directory).unwrap();
    assert!(
        keys::rotate(&directory, &store, "rotation-fail")
            .await
            .is_err()
    );
    drop(lock);

    let failed = read_session(&store, &session_id).await;
    assert_eq!(failed["token"], old_cipher);
    assert_eq!(failed["csrf"], csrf);
    assert_eq!(key_id(&store, &session_id).await, None);
    assert_eq!(
        std::fs::read_to_string(directory.join("session.key")).unwrap(),
        old_key
    );
    assert!(directory.join("session.keys.pending.json").exists());

    let mut db = store.write().await.unwrap();
    store::exec(&mut db, "DROP TRIGGER reject_rotation", &[])
        .await
        .unwrap();
    db.commit().await.unwrap();
    let recovered = Keyring::load(&directory, &store).await.unwrap();
    assert_eq!(recovered.decrypt(&old_cipher).unwrap(), b"unchanged-token");
    assert_eq!(
        key_id(&store, &session_id).await.as_deref(),
        Some("rotation-fail")
    );

    store.close().await;
}

#[tokio::test]
async fn duplicate_rotation_id_and_invalid_keyring_are_rejected() {
    let duplicate_temp = TempDir::new().unwrap();
    let duplicate_dir = duplicate_temp.path().join("data");
    std::fs::create_dir_all(&duplicate_dir).unwrap();
    let store = open_store(&duplicate_dir).await;
    let ring = Keyring::load(&duplicate_dir, &store).await.unwrap();
    let lock = LedgerLock::acquire(&duplicate_dir).unwrap();
    let error = keys::rotate(&duplicate_dir, &store, &ring.active)
        .await
        .unwrap_err();
    drop(lock);
    assert_eq!(error.status, 409);
    assert!(!duplicate_dir.join("session.keys.pending.json").exists());
    store.close().await;

    let invalid_temp = TempDir::new().unwrap();
    let invalid_dir = invalid_temp.path().join("data");
    std::fs::create_dir_all(&invalid_dir).unwrap();
    let raw_key = fernet::Fernet::generate_key();
    std::fs::write(invalid_dir.join("session.key"), &raw_key).unwrap();
    std::fs::write(
        invalid_dir.join("session.keys.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "active": "missing-key",
            "keys": {"valid-id": raw_key}
        }))
        .unwrap(),
    )
    .unwrap();
    let store = open_store(&invalid_dir).await;
    let error = match Keyring::load(&invalid_dir, &store).await {
        Ok(_) => panic!("invalid keyring must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.status, 422);
    store.close().await;
}

#[test]
fn ledger_lock_excludes_a_second_owner_and_releases_after_drop() {
    let temporary = TempDir::new().unwrap();
    let first = LedgerLock::acquire(temporary.path()).unwrap();
    assert!(LedgerLock::acquire(temporary.path()).is_err());
    drop(first);
    let second = LedgerLock::acquire(temporary.path()).unwrap();
    drop(second);
    private_file(&temporary.path().join("settlement.lock"));
}

#[tokio::test]
async fn limiter_separates_sso_ips_and_user_write_buckets() {
    let limiter = Limiter::new(2, 2, vec![]);
    let ip_a: IpAddr = "198.51.100.10".parse().unwrap();
    let ip_b: IpAddr = "198.51.100.11".parse().unwrap();

    limiter.sso(Some(ip_a)).await.unwrap();
    limiter.sso(Some(ip_a)).await.unwrap();
    assert_eq!(limiter.sso(Some(ip_a)).await.unwrap_err().status, 429);
    limiter.sso(Some(ip_b)).await.unwrap();

    limiter.write(7).await.unwrap();
    limiter.write(7).await.unwrap();
    assert_eq!(limiter.write(7).await.unwrap_err().status, 429);
    limiter.write(8).await.unwrap();
}

#[test]
fn x_real_ip_requires_loopback_or_an_exact_trusted_proxy() {
    let forwarded: IpAddr = "203.0.113.30".parse().unwrap();
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    let untrusted: IpAddr = "192.0.2.20".parse().unwrap();
    let trusted: IpAddr = "192.0.2.21".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("X-Real-IP", forwarded.to_string().parse().unwrap());

    let no_proxy_trust = Limiter::new(1, 1, vec![]);
    assert_eq!(
        no_proxy_trust.client_ip(Some(loopback), &headers),
        Some(forwarded)
    );
    assert_eq!(
        no_proxy_trust.client_ip(Some(untrusted), &headers),
        Some(untrusted)
    );
    assert_eq!(no_proxy_trust.client_ip(None, &headers), None);

    let exact_proxy = Limiter::new(1, 1, vec![trusted]);
    assert_eq!(
        exact_proxy.client_ip(Some(trusted), &headers),
        Some(forwarded)
    );
    assert_eq!(
        exact_proxy.client_ip(Some(untrusted), &headers),
        Some(untrusted)
    );
}

async fn fake_auth(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let valid = headers
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        == Some("Bearer fixture-token");
    if valid {
        (
            StatusCode::OK,
            Json(json!({
                "code": 0,
                "data": {"id": 7, "username": "Fixture", "role": "user", "status": "active"}
            })),
        )
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({"code": 1})))
    }
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
        revision: "session-security-test".into(),
        assets: directory.join("assets"),
    }
}

#[tokio::test]
async fn sso_route_uses_limiter_and_sets_retry_after_for_429() {
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
    let mut app = App::new(app_config(
        temporary.path(),
        format!("http://{verify_address}"),
    ))
    .await
    .unwrap();
    app.limiter = Arc::new(Limiter::new(1, 1, vec![]));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app
        .router()
        .into_make_service_with_connect_info::<SocketAddr>();
    let app_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = format!("http://{address}/settlement/sso/start?token=fixture-token&user_id=7");
    let first = client
        .get(&url)
        .header("X-Real-IP", "198.51.100.41")
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::SEE_OTHER);
    let limited = client
        .get(&url)
        .header("X-Real-IP", "198.51.100.41")
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers()["Retry-After"], "60");
    let other_ip = client
        .get(&url)
        .header("X-Real-IP", "198.51.100.42")
        .send()
        .await
        .unwrap();
    assert_eq!(other_ip.status(), StatusCode::SEE_OTHER);

    app_task.abort();
    let _ = app_task.await;
    verify_task.abort();
    let _ = verify_task.await;
    app.service.source.close().await;
    app.service.store.close().await;
}
