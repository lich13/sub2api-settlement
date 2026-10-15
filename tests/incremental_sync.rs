use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use settlement::{config::Config, http::App};
use sqlx::Row;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

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
        revision: "incremental-sync-test".to_owned(),
        assets: directory.join("assets"),
    }
}

fn account(id: i64, parent: Option<i64>) -> Value {
    json!({
        "id": id,
        "name": format!("Fixture account {id}"),
        "platform": "openai",
        "status": "active",
        "deleted": false,
        "parent_account_id": parent,
        "created_at": "2024-01-01T00:00:00+00:00"
    })
}

fn usage(id: i64, account_id: i64, user_id: i64, amount: &str, created_at: &str) -> Value {
    json!({
        "id": id,
        "account_id": account_id,
        "user_id": user_id,
        "amount": amount,
        "created_at": created_at,
        "request_id": format!("fixture-request-{id}"),
        "input_tokens": 1,
        "output_tokens": 2
    })
}

fn write_fixture(path: &Path, revision: usize, accounts: &[Value], rows: &[Value]) {
    let fixture = json!({
        "revision": "r".repeat(revision),
        "accounts": accounts,
        "users": [
            {"id": 101, "name": "Fixture user 101", "role": "admin", "status": "active", "deleted": false},
            {"id": 102, "name": "Fixture user 102", "role": "user", "status": "active", "deleted": false}
        ],
        "cleanups": [],
        "usage": rows
    });
    std::fs::write(path, serde_json::to_vec(&fixture).unwrap()).unwrap();
}

async fn new_app(directory: &TempDir, fixture: &Path) -> App {
    App::new(config(directory.path(), fixture.to_owned()))
        .await
        .unwrap()
}

async fn sync_now(app: &App, force: bool) -> settlement::error::Result<()> {
    *app.service.sync_lock.lock().await = None;
    app.service.sync(force).await
}

async fn close_app(app: &App) {
    app.service.source.close().await;
    app.service.store.close().await;
}

async fn sync_state(app: &App) -> (i64, String, String, i64, String) {
    let mut db = app.service.store.read().await.unwrap();
    let row = sqlx::query(
        "SELECT watermark,last_incremental,last_reconciled,scanned_rows,mode \
         FROM rust_sync_state WHERE id=1",
    )
    .fetch_one(&mut *db)
    .await
    .unwrap();
    let result = (
        row.try_get("watermark").unwrap(),
        row.try_get("last_incremental").unwrap(),
        row.try_get("last_reconciled").unwrap(),
        row.try_get("scanned_rows").unwrap(),
        row.try_get("mode").unwrap(),
    );
    db.commit().await.unwrap();
    result
}

async fn usage_row(app: &App, id: i64) -> Option<(i64, i64, String)> {
    let mut db = app.service.store.read().await.unwrap();
    let row = sqlx::query("SELECT root_id,user_id,amount FROM usage WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *db)
        .await
        .unwrap()
        .map(|row| {
            (
                row.try_get("root_id").unwrap(),
                row.try_get("user_id").unwrap(),
                row.try_get("amount").unwrap(),
            )
        });
    db.commit().await.unwrap();
    row
}

async fn ids(app: &App, sql: &str) -> Vec<i64> {
    let mut db = app.service.store.read().await.unwrap();
    let values = sqlx::query_scalar::<_, i64>(sql)
        .fetch_all(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    values
}

async fn fingerprints(app: &App) -> Vec<(i64, String, i64, String, String)> {
    let mut db = app.service.store.read().await.unwrap();
    let rows = sqlx::query(
        "SELECT account_id,day,record_count,amount,digest \
         FROM rust_usage_fingerprints ORDER BY account_id,day",
    )
    .fetch_all(&mut *db)
    .await
    .unwrap()
    .iter()
    .map(|row| {
        (
            row.try_get("account_id").unwrap(),
            row.try_get("day").unwrap(),
            row.try_get("record_count").unwrap(),
            row.try_get("amount").unwrap(),
            row.try_get("digest").unwrap(),
        )
    })
    .collect();
    db.commit().await.unwrap();
    rows
}

fn minutes_before(value: &str, minutes: i64) -> String {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
        .checked_sub_signed(chrono::Duration::minutes(minutes))
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Micros, true)
}

#[tokio::test]
async fn incremental_sync_includes_backdated_new_ids_and_recent_old_ids_only() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None)];
    let original = vec![
        usage(10, 100, 101, "1.00", "2018-01-01T00:00:00+00:00"),
        usage(20, 100, 101, "2.00", "2018-01-02T00:00:00+00:00"),
    ];
    write_fixture(&fixture, 1, &accounts, &original);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();
    let (watermark, last_incremental, _, _, _) = sync_state(&app).await;
    assert_eq!(watermark, 20);

    let mut changed = original;
    changed[1]["created_at"] = json!(minutes_before(&last_incremental, 5));
    changed[1]["user_id"] = json!(102);
    changed[1]["amount"] = json!("2.50");
    changed.push(usage(30, 100, 101, "3.00", "2010-01-01T00:00:00+00:00"));
    write_fixture(&fixture, 2, &accounts, &changed);

    sync_now(&app, false).await.unwrap();
    let (next_watermark, _, _, scanned, mode) = sync_state(&app).await;
    assert_eq!(mode, "incremental");
    assert_eq!(
        next_watermark, 30,
        "the backdated new ID advances the cursor"
    );
    assert_eq!(scanned, 2, "the unchanged ancient bucket is not staged");
    assert_eq!(
        usage_row(&app, 20).await.unwrap(),
        (100, 102, "2.50".to_owned())
    );
    assert!(usage_row(&app, 30).await.is_some());
    assert_eq!(usage_row(&app, 10).await.unwrap().1, 101);
    close_app(&app).await;
}

#[tokio::test]
async fn daily_fingerprint_detects_same_total_user_and_token_changes_by_bucket() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None), account(200, None)];
    let mut rows = vec![
        usage(1, 100, 101, "1.00", "2024-01-01T00:00:00+00:00"),
        usage(2, 100, 101, "2.00", "2024-01-01T12:00:00+00:00"),
        usage(3, 100, 101, "4.00", "2024-01-02T00:00:00+00:00"),
        usage(4, 200, 101, "5.00", "2024-01-03T00:00:00+00:00"),
    ];
    write_fixture(&fixture, 1, &accounts, &rows);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();
    let before = fingerprints(&app).await;
    assert_eq!(before[0].2, 2);
    assert_eq!(before[0].3, "3.00");

    rows[0]["user_id"] = json!(102);
    rows[0]["input_tokens"] = json!(77);
    write_fixture(&fixture, 2, &accounts, &rows);
    let mut db = app.service.store.write().await.unwrap();
    sqlx::query(
        "UPDATE rust_sync_state SET last_reconciled='2000-01-01T00:00:00.000000+00:00' WHERE id=1",
    )
    .execute(&mut *db)
    .await
    .unwrap();
    db.commit().await.unwrap();

    sync_now(&app, false).await.unwrap();
    let (watermark, _, _, scanned, mode) = sync_state(&app).await;
    assert_eq!(mode, "daily");
    assert_eq!(watermark, 4);
    assert_eq!(scanned, 2, "only the changed account/day bucket is reread");
    let after = fingerprints(&app).await;
    assert_eq!(after[0].2, before[0].2);
    assert_eq!(after[0].3, before[0].3);
    assert_ne!(after[0].4, before[0].4);
    assert_eq!(usage_row(&app, 1).await.unwrap().1, 102);
    let mut db = app.service.store.read().await.unwrap();
    let detail =
        sqlx::query_scalar::<_, String>("SELECT payload FROM usage_details WHERE usage_id=1")
            .fetch_one(&mut *db)
            .await
            .unwrap();
    db.commit().await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&detail).unwrap()["input_tokens"],
        77
    );
    close_app(&app).await;
}

#[tokio::test]
async fn full_sync_removes_source_presence_but_preserves_local_usage_copy() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None)];
    let rows = [usage(1, 100, 101, "7.00", "2024-01-01T00:00:00+00:00")];
    write_fixture(&fixture, 1, &accounts, &rows);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();
    assert_eq!(
        ids(&app, "SELECT id FROM rust_source_presence ORDER BY id").await,
        vec![1]
    );

    write_fixture(&fixture, 2, &accounts, &[]);
    sync_now(&app, true).await.unwrap();
    assert!(
        usage_row(&app, 1).await.is_some(),
        "local ledger rows are retained"
    );
    assert!(
        ids(&app, "SELECT id FROM rust_source_presence ORDER BY id")
            .await
            .is_empty()
    );
    close_app(&app).await;
}

#[tokio::test]
async fn changed_parent_child_ownership_forces_a_full_scan() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let initial_accounts = [
        account(100, None),
        account(200, None),
        account(101, Some(100)),
    ];
    let rows = [
        usage(1, 101, 101, "1.00", "2018-01-01T00:00:00+00:00"),
        usage(2, 200, 101, "2.00", "2018-01-02T00:00:00+00:00"),
    ];
    write_fixture(&fixture, 1, &initial_accounts, &rows);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();
    assert_eq!(usage_row(&app, 1).await.unwrap().0, 100);

    let changed_accounts = [
        account(100, None),
        account(200, None),
        account(101, Some(200)),
    ];
    write_fixture(&fixture, 2, &changed_accounts, &rows);
    sync_now(&app, false).await.unwrap();
    let (watermark, _, _, scanned, mode) = sync_state(&app).await;
    assert_eq!(mode, "full");
    assert_eq!(watermark, 2);
    assert_eq!(
        scanned, 2,
        "both old records are included after ownership changes"
    );
    assert_eq!(usage_row(&app, 1).await.unwrap().0, 200);
    close_app(&app).await;
}

#[tokio::test]
async fn failed_daily_reconciliation_does_not_publish_checkpoint_or_fingerprints() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None)];
    let original = [usage(1, 100, 101, "1.00", "2024-01-01T00:00:00+00:00")];
    write_fixture(&fixture, 1, &accounts, &original);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();
    let (watermark, last_incremental, _, scanned_rows, mode) = sync_state(&app).await;
    let before_fingerprints = fingerprints(&app).await;
    assert_eq!(mode, "full");

    let mut db = app.service.store.write().await.unwrap();
    sqlx::query(
        "UPDATE rust_sync_state SET last_reconciled='2000-01-01T00:00:00.000000+00:00' WHERE id=1",
    )
    .execute(&mut *db)
    .await
    .unwrap();
    db.commit().await.unwrap();

    let malformed = json!({
        "id": 99,
        "account_id": 100,
        "user_id": 101,
        "created_at": "2024-01-02T00:00:00+00:00"
    });
    write_fixture(&fixture, 2, &accounts, &[original[0].clone(), malformed]);
    assert!(sync_now(&app, false).await.is_err());

    let (after_watermark, after_incremental, after_reconciled, after_scanned, after_mode) =
        sync_state(&app).await;
    assert_eq!(after_watermark, watermark);
    assert_eq!(after_incremental, last_incremental);
    assert_eq!(after_reconciled, "2000-01-01T00:00:00.000000+00:00");
    assert_eq!(after_scanned, scanned_rows);
    assert_eq!(after_mode, "full");
    assert_eq!(fingerprints(&app).await, before_fingerprints);
    close_app(&app).await;
}

#[tokio::test]
async fn incremental_checkpoint_survives_app_restart() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None)];
    let rows = [usage(1, 100, 101, "1.00", "2018-01-01T00:00:00+00:00")];
    write_fixture(&fixture, 1, &accounts, &rows);
    let first = new_app(&temporary, &fixture).await;
    sync_now(&first, true).await.unwrap();
    let (watermark, _, _, _, _) = sync_state(&first).await;
    close_app(&first).await;

    let restarted = new_app(&temporary, &fixture).await;
    sync_now(&restarted, false).await.unwrap();
    let (after_watermark, _, _, scanned, mode) = sync_state(&restarted).await;
    assert_eq!(after_watermark, watermark);
    assert_eq!(scanned, 0);
    assert_eq!(mode, "incremental");
    close_app(&restarted).await;
}

#[tokio::test]
async fn sync_selection_rechecks_old_rows_in_the_entire_selected_range() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None)];
    let original = vec![
        usage(1, 100, 101, "1.00", "2020-01-01T04:00:00+00:00"),
        usage(2, 100, 101, "2.00", "2020-01-01T12:00:00+00:00"),
        usage(3, 100, 101, "3.00", "2020-01-03T00:00:00+00:00"),
    ];
    write_fixture(&fixture, 1, &accounts, &original);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();

    let mut db = app.service.store.write().await.unwrap();
    let cycle_id = sqlx::query("INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,end) VALUES(100,'Fixture cycle',0,101,'2020-01-01T00:00:00.000000+00:00','2020-01-02T00:00:00.000000+00:00')")
        .execute(&mut *db)
        .await
        .unwrap()
        .last_insert_rowid();
    db.commit().await.unwrap();

    let mut changed = original;
    changed[0]["user_id"] = json!(102);
    changed[0]["amount"] = json!("1.50");
    changed.push(usage(4, 100, 102, "4.00", "2020-01-01T20:00:00+00:00"));
    write_fixture(&fixture, 2, &accounts, &changed);
    app.service
        .sync_selection(&[json!(cycle_id)], "2020-01-02T00:00:00.000000+00:00", None)
        .await
        .unwrap();

    assert_eq!(
        usage_row(&app, 1).await.unwrap(),
        (100, 102, "1.50".to_owned())
    );
    assert!(usage_row(&app, 4).await.is_some());
    assert_eq!(usage_row(&app, 3).await.unwrap().1, 101);
    assert_eq!(
        ids(&app, "SELECT id FROM rust_sync_stage ORDER BY id").await,
        vec![1, 2, 4],
        "selected sync must scan all records in-range, including old IDs"
    );
    close_app(&app).await;
}
