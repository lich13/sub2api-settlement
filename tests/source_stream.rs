use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use settlement::{
    config::Config,
    http::App,
    source::{Scope, Source},
};
use sqlx::Row;
use tempfile::TempDir;

const CREATED_AT: &str = "2025-01-15T00:00:00.000000+00:00";

fn config(directory: &Path, fixture: PathBuf) -> Config {
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
        revision: "test-fixture".to_owned(),
        assets: directory.join("assets"),
    }
}

fn usage(id: i64) -> Value {
    json!({
        "id": id,
        "account_id": 100,
        "user_id": 1,
        "amount": "1.00",
        "created_at": CREATED_AT
    })
}

fn usage_at(id: i64, account_id: i64, created_at: &str) -> Value {
    let mut row = usage(id);
    row["account_id"] = json!(account_id);
    row["created_at"] = json!(created_at);
    row
}

fn write_fixture(path: &Path, rows: &[Value]) {
    let fixture = json!({
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
        "usage": rows
    });
    std::fs::write(path, serde_json::to_vec(&fixture).unwrap()).unwrap();
}

async fn snapshot_rows(source: &Source) -> Vec<Value> {
    let mut snapshot = source.snapshot(None).await.unwrap();
    let mut output = Vec::new();
    while let Some(batch) = snapshot.next().await.unwrap() {
        output.extend(batch);
    }
    output
}

#[tokio::test]
async fn fixture_stream_crosses_256_kib_and_preserves_escaped_unicode_fields() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let mut row = usage(1);
    let long_value = format!("{}\"\\雪🧪", "x".repeat(256 * 1024));
    row["request_id"] = json!(long_value);
    row["model"] = json!("quote \" slash \\ unicode 雪🧪");
    write_fixture(&fixture, &[row]);

    let source = Source::new(&config(temporary.path(), fixture))
        .await
        .unwrap();
    let rows = snapshot_rows(&source).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["request_id"], long_value);
    assert_eq!(rows[0]["model"], "quote \" slash \\ unicode 雪🧪");
    source.close().await;
}

#[tokio::test]
async fn fixture_stream_accepts_empty_usage_and_missing_optional_details() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_fixture(&fixture, &[]);
    let source = Source::new(&config(temporary.path(), fixture.clone()))
        .await
        .unwrap();
    assert!(snapshot_rows(&source).await.is_empty());

    let mut row = usage(2);
    for field in [
        "request_id",
        "upstream_request_id",
        "model",
        "requested_model",
        "upstream_model",
        "upstream_response_model",
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_creation_tokens",
    ] {
        row.as_object_mut().unwrap().remove(field);
    }
    write_fixture(&fixture, &[row]);
    // The source caches the header by modified time and length; the changed
    // fixture size forces a fresh header read.
    let rows = snapshot_rows(&source).await;
    assert_eq!(rows.len(), 1);
    for field in [
        "request_id",
        "upstream_request_id",
        "model",
        "requested_model",
        "upstream_model",
        "upstream_response_model",
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_creation_tokens",
    ] {
        assert!(rows[0][field].is_null(), "{field} should default to null");
    }
    source.close().await;
}

#[tokio::test]
async fn fixture_scope_compares_instants_and_rechecks_known_rows_after_they_move() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let mut rows = vec![
        usage_at(1, 100, "2025-01-14T23:59:59.999999+00:00"),
        usage_at(2, 100, "2025-01-15T00:00:00+00:00"),
        usage_at(3, 100, "2025-01-15T08:00:00+08:00"),
        usage_at(4, 100, "2025-01-15T04:00:00-04:00"),
        usage_at(5, 100, "2025-01-15T11:59:59.999999+00:00"),
        usage_at(6, 100, "2025-01-15T12:00:00+00:00"),
        usage_at(7, 200, "2025-01-15T09:00:00+08:00"),
        usage_at(8, 100, "2025-01-15T10:00:00+08:00"),
    ];
    write_fixture(&fixture, &rows);

    let source = Source::new(&config(temporary.path(), fixture.clone()))
        .await
        .unwrap();
    let scope = Scope {
        root: 100,
        accounts: vec![100],
        start: "2025-01-15T08:00:00+08:00".to_owned(),
        end: "2025-01-15T20:00:00+08:00".to_owned(),
        known_ids: vec![8],
    };

    let first = {
        let mut snapshot = source.snapshot(Some(vec![scope.clone()])).await.unwrap();
        let mut selected = Vec::new();
        while let Some(batch) = snapshot.next().await.unwrap() {
            selected.extend(batch);
        }
        selected
    };
    let first_ids = first
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(first_ids, vec![2, 3, 4, 5, 8]);
    assert!(
        !first_ids.contains(&1),
        "the instant before the lower bound must be excluded"
    );
    assert!(
        first_ids.contains(&2),
        "an equivalent UTC start must be included"
    );
    assert!(
        first_ids.contains(&3),
        "the exact local start must be included"
    );
    assert!(
        first_ids.contains(&5),
        "the instant before the upper bound must be included"
    );
    assert!(!first_ids.contains(&6), "the upper bound must be exclusive");
    assert!(
        !first_ids.contains(&7),
        "accounts outside the scope must be excluded"
    );

    rows[7]["account_id"] = json!(200);
    rows[7]["created_at"] = json!("2025-01-16T00:00:00.000000+00:00");
    rows[7]["request_id"] = json!("fixture-known-row-moved-outside-scope");
    write_fixture(&fixture, &rows);

    let second = {
        let mut snapshot = source.snapshot(Some(vec![scope])).await.unwrap();
        let mut selected = Vec::new();
        while let Some(batch) = snapshot.next().await.unwrap() {
            selected.extend(batch);
        }
        selected
    };
    let moved = second.iter().find(|row| row["id"] == 8).unwrap();
    assert_eq!(moved["account_id"], 200);
    assert_eq!(moved["created_at"], "2025-01-16T00:00:00.000000+00:00");
    assert_eq!(
        second
            .iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3, 4, 5, 8]
    );

    source.close().await;
}

async fn ledger_usage(app: &App) -> Vec<(i64, String, String)> {
    let mut db = app.service.store.read().await.unwrap();
    let rows = sqlx::query("SELECT id,amount,created_at FROM usage ORDER BY id")
        .fetch_all(&mut *db)
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.try_get("id").unwrap(),
                row.try_get("amount").unwrap(),
                row.try_get("created_at").unwrap(),
            )
        })
        .collect();
    db.commit().await.unwrap();
    rows
}

#[tokio::test]
async fn malformed_fixture_row_after_first_batch_does_not_publish_partial_usage() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_fixture(&fixture, &[usage(1)]);
    let app = App::new(config(temporary.path(), fixture.clone()))
        .await
        .unwrap();
    app.service.sync(true).await.unwrap();
    let before = ledger_usage(&app).await;
    assert_eq!(before.len(), 1);

    let mut rows = (1..=2049).map(usage).collect::<Vec<_>>();
    rows[2048].as_object_mut().unwrap().remove("amount");
    write_fixture(&fixture, &rows);

    assert!(app.service.sync(true).await.is_err());
    assert_eq!(ledger_usage(&app).await, before);

    let mut db = app.service.store.read().await.unwrap();
    let staged: i64 = sqlx::query("SELECT count(*) AS count FROM rust_sync_stage")
        .fetch_one(&mut *db)
        .await
        .unwrap()
        .try_get("count")
        .unwrap();
    db.commit().await.unwrap();
    assert_eq!(
        staged, 2048,
        "the parser must fail after streaming one full batch"
    );

    app.service.source.close().await;
    app.service.store.close().await;
}
