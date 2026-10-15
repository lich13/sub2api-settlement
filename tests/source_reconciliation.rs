use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use settlement::{
    config::Config,
    source::{Fingerprint, Scope, Snapshot, Source},
};
use tempfile::TempDir;

fn config(fixture: PathBuf) -> Config {
    Config {
        data_dir: "test-data".into(),
        source_url: String::new(),
        fixture: Some(fixture),
        origin: "https://example.invalid".to_owned(),
        verify_url: "http://127.0.0.1:1".to_owned(),
        bind: "127.0.0.1:0".parse().expect("static loopback address"),
        background: false,
        read_only: true,
        external_writes: false,
        test: true,
        revision: "source-reconciliation-fixture".to_owned(),
        assets: "test-assets".into(),
    }
}

fn usage(id: i64, account_id: i64, user_id: i64, amount: &str, created_at: &str) -> Value {
    json!({
        "id": id,
        "account_id": account_id,
        "user_id": user_id,
        "amount": amount,
        "created_at": created_at,
        "request_id": format!("request-{id}"),
        "input_tokens": 1,
        "output_tokens": 2
    })
}

fn write_fixture(path: &Path, rows: &[Value]) {
    let fixture = json!({
        "accounts": [],
        "users": [],
        "cleanups": [],
        "usage": rows
    });
    std::fs::write(path, serde_json::to_vec(&fixture).unwrap()).unwrap();
}

async fn fingerprints(rows: &[Value]) -> Vec<Fingerprint> {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_fixture(&fixture, rows);
    let source = Source::new(&config(fixture)).await.unwrap();
    let mut snapshot = source.snapshot(None).await.unwrap();
    let result = snapshot.fingerprints().await.unwrap();
    drop(snapshot);
    source.close().await;
    result
}

async fn ids(snapshot: &mut Snapshot) -> Vec<i64> {
    let mut result = Vec::new();
    while let Some(batch) = snapshot.next().await.unwrap() {
        result.extend(batch.into_iter().map(|row| row["id"].as_i64().unwrap()));
    }
    result
}

#[tokio::test]
async fn fixture_fingerprints_are_order_independent_and_detect_identity_details() {
    let original = vec![
        usage(
            1,
            100,
            1,
            "0.000000000000000001",
            "2025-01-15T12:00:00.000000+00:00",
        ),
        usage(
            2,
            100,
            2,
            "1.230000000000000000",
            "2025-01-15T13:00:00.000000+00:00",
        ),
    ];
    let baseline = fingerprints(&original).await;
    let mut reversed = original.clone();
    reversed.reverse();
    assert_eq!(baseline, fingerprints(&reversed).await);

    let baseline_bucket = baseline
        .iter()
        .find(|fingerprint| fingerprint.account_id == 100 && fingerprint.day == "2025-01-15")
        .unwrap();
    assert_eq!(baseline_bucket.count, 2);
    assert_eq!(baseline_bucket.amount, "1.230000000000000001");

    let mut changed_user = original.clone();
    changed_user[1]["user_id"] = json!(3);
    let user_bucket = fingerprints(&changed_user)
        .await
        .into_iter()
        .find(|fingerprint| fingerprint.account_id == 100 && fingerprint.day == "2025-01-15")
        .unwrap();
    assert_eq!(user_bucket.count, baseline_bucket.count);
    assert_eq!(user_bucket.amount, baseline_bucket.amount);
    assert_ne!(user_bucket.digest, baseline_bucket.digest);

    let mut changed_tokens = original;
    changed_tokens[0]["input_tokens"] = json!(999);
    let token_bucket = fingerprints(&changed_tokens)
        .await
        .into_iter()
        .find(|fingerprint| fingerprint.account_id == 100 && fingerprint.day == "2025-01-15")
        .unwrap();
    assert_eq!(token_bucket.count, baseline_bucket.count);
    assert_eq!(token_bucket.amount, baseline_bucket.amount);
    assert_ne!(token_bucket.digest, baseline_bucket.digest);
}

#[tokio::test]
async fn fixture_incremental_and_restricted_streams_honor_time_and_known_id_boundaries() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_fixture(
        &fixture,
        &[
            usage(8, 200, 1, "1.00", "2025-01-14T23:00:00.000000+00:00"),
            usage(9, 100, 1, "1.00", "2025-01-14T23:59:59.999999+00:00"),
            usage(10, 100, 1, "1.00", "2025-01-15T00:00:00.000000+00:00"),
            usage(11, 200, 1, "1.00", "2025-01-14T12:00:00.000000+00:00"),
            usage(12, 100, 1, "1.00", "2025-01-15T23:59:59.999999+00:00"),
            usage(13, 100, 1, "1.00", "2025-01-16T00:00:00.000000+00:00"),
            usage(14, 200, 1, "1.00", "2025-01-15T12:00:00.000000+00:00"),
        ],
    );
    let source = Source::new(&config(fixture)).await.unwrap();

    let mut incremental = source
        .incremental(10, "2025-01-15T00:00:00.000000+00:00")
        .await
        .unwrap();
    assert_eq!(ids(&mut incremental).await, vec![10, 11, 12, 13, 14]);

    let scope = Scope {
        root: 100,
        accounts: vec![100],
        start: "2025-01-15T00:00:00.000000+00:00".to_owned(),
        end: "2025-01-16T00:00:00.000000+00:00".to_owned(),
        known_ids: vec![8],
    };
    incremental.restrict(vec![scope]).await.unwrap();
    assert_eq!(ids(&mut incremental).await, vec![8, 10, 12]);

    incremental.unrestrict().await.unwrap();
    assert_eq!(ids(&mut incremental).await, vec![8, 9, 10, 11, 12, 13, 14]);
    source.close().await;
}

#[tokio::test]
async fn fixture_stream_rejects_file_mutation_after_snapshot_open() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_fixture(
        &fixture,
        &[
            usage(1, 100, 1, "1.00", "2025-01-15T12:00:00.000000+00:00"),
            usage(2, 100, 1, "2.00", "2025-01-15T12:01:00.000000+00:00"),
        ],
    );
    let source = Source::new(&config(fixture.clone())).await.unwrap();
    let mut snapshot = source.snapshot(None).await.unwrap();

    OpenOptions::new()
        .append(true)
        .open(&fixture)
        .unwrap()
        .write_all(b"\n")
        .unwrap();

    let mut received_rows = 0;
    loop {
        match snapshot.next().await {
            Ok(Some(batch)) => received_rows += batch.len(),
            Ok(None) => panic!("mutated fixture unexpectedly completed its stream"),
            Err(error) => {
                assert_eq!(error.status, 503);
                break;
            }
        }
    }
    assert_eq!(received_rows, 2);
    source.close().await;
}
