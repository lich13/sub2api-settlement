use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use settlement::{config::Config, http::App, inventory::AccountSnapshot};
use tempfile::TempDir;

fn account(id: i64, name: &str, parent: Option<i64>, deleted: bool) -> Value {
    json!({
        "id": id,
        "name": name,
        "platform": "openai",
        "status": "active",
        "deleted": deleted,
        "parent_account_id": parent,
        "created_at": "2025-01-01T00:00:00+00:00"
    })
}

fn write_fixture(path: &Path, accounts: Vec<Value>, usage: Value) {
    let fixture = json!({
        "accounts": accounts,
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
    std::fs::write(path, serde_json::to_vec(&fixture).unwrap()).unwrap();
}

fn config(directory: &Path, fixture: PathBuf) -> Config {
    Config {
        data_dir: directory.join("data"),
        source_url: String::new(),
        fixture: Some(fixture),
        origin: "https://example.invalid".to_owned(),
        verify_url: "http://127.0.0.1:1".to_owned(),
        bind: "127.0.0.1:0".parse().unwrap(),
        background: false,
        read_only: true,
        external_writes: false,
        test: true,
        revision: "test-fixture".to_owned(),
        assets: directory.join("assets"),
    }
}

fn active_ids(state: &Value) -> BTreeSet<i64> {
    state["active_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_i64().unwrap())
        .collect()
}

fn snapshot_row(id: i64, root_id: i64, name: &str, deleted: bool) -> Value {
    json!({
        "id": id,
        "name": name,
        "platform": "openai",
        "status": "active",
        "deleted": i64::from(deleted),
        "root_id": root_id,
        "created_at": "2025-01-01T00:00:00+00:00"
    })
}

fn snapshot_rows(rows: Vec<Value>) -> BTreeMap<i64, Value> {
    rows.into_iter()
        .map(|row| (row["id"].as_i64().unwrap(), row))
        .collect()
}

fn account_ids(rows: &[Value]) -> BTreeSet<i64> {
    rows.iter().map(|row| row["id"].as_i64().unwrap()).collect()
}

async fn deleted_flag(app: &App, id: i64) -> i64 {
    let mut db = app.service.store.read().await.unwrap();
    let deleted = sqlx::query_scalar::<_, i64>("SELECT deleted FROM accounts WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *db)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("account {id} was not persisted"));
    db.commit().await.unwrap();
    deleted
}

async fn account_root(app: &App, id: i64) -> i64 {
    let mut db = app.service.store.read().await.unwrap();
    let root = sqlx::query_scalar::<_, i64>("SELECT root_id FROM accounts WHERE id=?")
        .bind(id)
        .fetch_one(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    root
}

async fn usage_count(app: &App) -> i64 {
    let mut db = app.service.store.read().await.unwrap();
    let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage")
        .fetch_one(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    count
}

async fn refresh_until_persisted(app: &App) {
    // Account visibility is published before best-effort SQLite persistence.
    // A just-committed pooled connection can still be returning asynchronously,
    // so retry the normal refresh path without requiring an immediate write.
    let started = Instant::now();
    loop {
        let state = app.service.inventory.refresh(true).await;
        assert!(state["error"].is_null());
        if !app.service.inventory.state.read().await.dirty {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "account snapshot was not persisted after the writer became available"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn account_inventory_hides_soft_and_missing_rows_and_preserves_last_good_on_failure() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let initial = vec![
        account(100, "Fixture root", None, false),
        account(101, "Fixture child", Some(100), false),
        account(200, "Soft-delete candidate", None, false),
        account(300, "Missing-row candidate", None, false),
    ];
    write_fixture(&fixture, initial.clone(), json!([]));
    let app = App::new(config(temporary.path(), fixture.clone()))
        .await
        .unwrap();

    let first = app.service.inventory.refresh(true).await;
    assert_eq!(active_ids(&first), BTreeSet::from([100, 200, 300]));
    assert!(first["error"].is_null());

    let soft_deleted = initial
        .iter()
        .cloned()
        .map(|mut row| {
            if row["id"] == 200 {
                row["deleted"] = json!(true);
            }
            row
        })
        .collect();
    write_fixture(
        &fixture,
        soft_deleted,
        json!("soft-delete fixture must not trigger usage reads"),
    );
    let soft = app.service.inventory.refresh(true).await;
    assert_eq!(active_ids(&soft), BTreeSet::from([100, 300]));
    assert!(soft["error"].is_null());
    refresh_until_persisted(&app).await;
    assert_eq!(deleted_flag(&app, 200).await, 1);

    let missing = vec![
        account(100, "Fixture root", None, false),
        account(101, "Fixture child", Some(100), false),
        account(200, "Soft-delete candidate", None, true),
    ];
    write_fixture(
        &fixture,
        missing,
        json!("complete source snapshot with a missing account row"),
    );
    let hard_deleted = app.service.inventory.refresh(true).await;
    assert_eq!(active_ids(&hard_deleted), BTreeSet::from([100]));
    assert!(hard_deleted["error"].is_null());
    refresh_until_persisted(&app).await;
    assert_eq!(deleted_flag(&app, 300).await, 1);

    std::fs::write(&fixture, b"{ invalid source").unwrap();
    let failed = app.service.inventory.refresh(true).await;
    assert_eq!(active_ids(&failed), BTreeSet::from([100]));
    assert!(!failed["error"].is_null());

    app.service.source.close().await;
    app.service.store.close().await;
}

#[tokio::test]
async fn account_inventory_publishes_under_usage_write_lock_without_reading_usage() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_fixture(
        &fixture,
        vec![
            account(100, "Fixture root", None, false),
            account(101, "Fixture child", Some(100), false),
        ],
        json!("deliberately not a usage array"),
    );
    let app = App::new(config(temporary.path(), fixture.clone()))
        .await
        .unwrap();

    let initial = app.service.inventory.refresh(true).await;
    assert_eq!(active_ids(&initial), BTreeSet::from([100]));
    assert!(initial["error"].is_null());
    write_fixture(
        &fixture,
        vec![
            account(100, "Fixture root", None, false),
            account(101, "Fixture child", Some(100), false),
            account(200, "New fixture root", None, false),
        ],
        json!("writer-locked account refresh still must not inspect usage"),
    );

    // The only SQLite writer is held while the account-only source snapshot is
    // published. An invalid usage payload makes an accidental usage scan fail.
    let writer = app.service.store.write().await.unwrap();
    let started = Instant::now();
    let state = tokio::time::timeout(Duration::from_secs(10), app.service.inventory.refresh(true))
        .await
        .expect("account snapshot waited on the accounting writer");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(active_ids(&state), BTreeSet::from([100, 200]));
    assert!(state["error"].is_null());
    assert!(app.service.inventory.state.read().await.dirty);

    writer.rollback().await.unwrap();

    // Confirm the rollback released a writer connection before retrying the
    // deferred snapshot persistence. A short bounded retry covers pool return
    // scheduling without weakening the eventual-persistence assertion.
    let available =
        tokio::time::timeout(Duration::from_secs(2), app.service.store.writes.acquire())
            .await
            .expect("writer connection was not returned after rollback")
            .unwrap();
    drop(available);

    let release_started = Instant::now();
    loop {
        let persisted = app.service.inventory.refresh(true).await;
        assert_eq!(active_ids(&persisted), BTreeSet::from([100, 200]));
        assert!(persisted["error"].is_null());
        if !app.service.inventory.state.read().await.dirty {
            break;
        }
        assert!(
            release_started.elapsed() < Duration::from_secs(3),
            "inventory remained dirty after a writer connection became available"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(deleted_flag(&app, 101).await, 0);
    assert_eq!(account_root(&app, 101).await, 100);
    assert_eq!(deleted_flag(&app, 200).await, 0);
    assert_eq!(account_root(&app, 200).await, 200);
    assert_eq!(usage_count(&app).await, 0);

    app.service.source.close().await;
    app.service.store.close().await;
}

#[tokio::test]
async fn catalog_and_overview_bind_rows_to_their_account_revision() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    write_fixture(
        &fixture,
        vec![account(100, "Fixture root", None, false)],
        json!([]),
    );
    let app = App::new(config(temporary.path(), fixture)).await.unwrap();
    let admin = json!({"id": 1, "name": "Fixture Admin", "role": "admin"});

    let versions = [
        (
            "fixture-snapshot-a",
            vec![
                snapshot_row(100, 100, "Snapshot A root", false),
                snapshot_row(101, 100, "Snapshot A child", false),
                snapshot_row(200, 200, "Snapshot A second root", false),
                snapshot_row(300, 300, "Snapshot A deleted root", true),
            ],
            BTreeSet::from([100, 200]),
        ),
        (
            "fixture-snapshot-b",
            vec![
                snapshot_row(100, 100, "Snapshot B root", false),
                snapshot_row(101, 100, "Snapshot B child", false),
                snapshot_row(200, 200, "Snapshot B deleted root", true),
                snapshot_row(300, 300, "Snapshot B second root", false),
            ],
            BTreeSet::from([100, 300]),
        ),
    ];

    let mut retained_pairs = Vec::new();
    for (revision, rows, expected_ids) in versions {
        *app.service.inventory.state.write().await = AccountSnapshot {
            rows: snapshot_rows(rows),
            revision: revision.to_owned(),
            checked_at: json!("2025-04-01T00:00:00.000000+00:00"),
            error: Value::Null,
            last_attempt: Some(Instant::now()),
            dirty: false,
        };

        let (state, rows) = app.service.inventory.active_snapshot().await;
        assert_eq!(state["revision"], json!(revision));
        assert_eq!(active_ids(&state), expected_ids);
        assert_eq!(account_ids(&rows), expected_ids);
        retained_pairs.push((state, rows));

        let catalog = app.service.catalog(&admin).await.unwrap();
        let overview = app.service.overview(&admin).await.unwrap();
        let account_state = app.service.account_state(&admin).await.unwrap();

        assert_eq!(catalog["account_revision"], json!(revision));
        assert_eq!(overview["account_revision"], json!(revision));
        assert_eq!(account_state["revision"], json!(revision));
        assert_eq!(
            account_ids(catalog["accounts"].as_array().unwrap()),
            expected_ids
        );
        assert_eq!(
            account_ids(overview["accounts"].as_array().unwrap()),
            expected_ids
        );
        assert_eq!(active_ids(&account_state), expected_ids);
    }
    // A later publication cannot mutate a previously captured revision/row pair.
    assert_eq!(retained_pairs[0].0["revision"], "fixture-snapshot-a");
    assert_eq!(
        account_ids(&retained_pairs[0].1),
        BTreeSet::from([100, 200])
    );
    assert_eq!(retained_pairs[1].0["revision"], "fixture-snapshot-b");
    assert_eq!(
        account_ids(&retained_pairs[1].1),
        BTreeSet::from([100, 300])
    );

    app.service.source.close().await;
    app.service.store.close().await;
}
