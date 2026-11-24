use serde_json::{Value, json};
use settlement::{config::Config, http::App, time};
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
    let values = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
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
    let shifted = time::parse(value)
        .unwrap()
        .checked_sub(time::Duration::minutes(minutes))
        .unwrap();
    time::normalize_datetime(&time::format(shifted)).unwrap()
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
        ids(&app, "SELECT id FROM rust_source_presence ORDER BY id").await,
        vec![1, 2, 3, 4],
        "selected sync must record in-range IDs in presence while preserving out-of-range IDs"
    );
    assert!(
        ids(&app, "SELECT id FROM rust_sync_stage ORDER BY id")
            .await
            .is_empty(),
        "staging table must be cleared once changes are published"
    );
    close_app(&app).await;
}

#[tokio::test]
async fn drift_detection_scopes_recalculation_to_changed_timestamp_windows() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None)];
    let mut rows = vec![
        usage(1, 100, 101, "1.00", "2023-12-31T00:00:00+00:00"),
        usage(2, 100, 101, "10.00", "2024-01-15T00:00:00+00:00"),
        usage(3, 100, 101, "20.00", "2024-02-10T00:00:00+00:00"),
    ];
    write_fixture(&fixture, 1, &accounts, &rows);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();

    let admin = json!({"id": 101, "name": "Fixture user 101", "role": "admin"});
    let create_and_settle = |key_prefix: &str, name: &str, start: &str, end: &str| {
        let app = &app;
        let admin = admin.clone();
        let create_key = format!("{key_prefix}-create");
        let settle_key = format!("{key_prefix}-settle");
        let payload = json!({
            "account_id": 100,
            "name": name,
            "cost": "10.00",
            "creditor_id": 101,
            "start": start,
            "start_mode": "exact",
            "cost_kind": "exact"
        });
        let end = end.to_owned();
        async move {
            let cycle = app
                .service
                .mutate(
                    &admin,
                    &create_key,
                    "create",
                    &payload,
                    settlement::service::Prepared::default(),
                    None,
                )
                .await
                .unwrap();
            let cycle_id = cycle["id"].as_i64().unwrap();
            let preview = app
                .service
                .preview(
                    &admin,
                    &json!({"ids": [cycle_id], "expense_ids": [], "end": end}),
                )
                .await
                .unwrap();
            let settle_payload = json!({
                "preview_id": preview["id"],
                "renewals": [],
                "delete_cycle_ids": []
            });
            let prepared = app
                .service
                .prepare_settlement(&admin, &settle_payload)
                .await
                .unwrap();
            app.service
                .mutate(
                    &admin,
                    &settle_key,
                    "settle",
                    &settle_payload,
                    prepared,
                    None,
                )
                .await
                .unwrap();
            cycle_id
        }
    };

    let jan_cycle = create_and_settle(
        "jan",
        "Jan 2024",
        "2024-01-01T00:00:00+00:00",
        "2024-02-01T00:00:00+00:00",
    )
    .await;
    let feb_cycle = create_and_settle(
        "feb",
        "Feb 2024",
        "2024-02-01T00:00:00+00:00",
        "2024-03-01T00:00:00+00:00",
    )
    .await;

    let drift_flags = || async {
        let mut db = app.service.store.read().await.unwrap();
        let jan: i64 = sqlx::query_scalar("SELECT drift FROM settlements WHERE cycle_id=?")
            .bind(jan_cycle)
            .fetch_one(&mut *db)
            .await
            .unwrap();
        let feb: i64 = sqlx::query_scalar("SELECT drift FROM settlements WHERE cycle_id=?")
            .bind(feb_cycle)
            .fetch_one(&mut *db)
            .await
            .unwrap();
        db.commit().await.unwrap();
        (jan, feb)
    };
    assert_eq!(drift_flags().await, (0, 0));

    // 1. Out-of-window append in March leaves both historical settlements untouched.
    rows.push(usage(4, 100, 102, "30.00", "2024-03-05T00:00:00+00:00"));
    write_fixture(&fixture, 2, &accounts, &rows);
    sync_now(&app, false).await.unwrap();
    assert_eq!(drift_flags().await, (0, 0));

    // 2. Moving an existing February row into March overlaps Feb via its prior timestamp,
    // marking Feb drifted while leaving Jan untouched.
    rows[2]["created_at"] = json!("2024-03-06T00:00:00+00:00");
    write_fixture(&fixture, 3, &accounts, &rows);
    sync_now(&app, true).await.unwrap();
    assert_eq!(drift_flags().await, (0, 1));

    // 3. Mutating an in-window January row marks Jan drifted as well.
    rows[1]["user_id"] = json!(102);
    write_fixture(&fixture, 4, &accounts, &rows);
    sync_now(&app, true).await.unwrap();
    assert_eq!(drift_flags().await, (1, 1));

    close_app(&app).await;
}

#[tokio::test]
async fn drift_detection_marks_both_roots_when_child_shadow_reparents() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let initial_accounts = [
        account(100, None),
        account(200, None),
        account(105, Some(100)),
    ];
    let rows = [
        usage(1, 100, 101, "1.00", "2023-12-31T00:00:00+00:00"),
        usage(2, 105, 102, "10.00", "2024-01-15T00:00:00+00:00"),
        usage(3, 200, 101, "10.00", "2024-01-16T00:00:00+00:00"),
        usage(4, 200, 101, "10.00", "2024-02-10T00:00:00+00:00"),
    ];
    write_fixture(&fixture, 1, &initial_accounts, &rows);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();

    let admin = json!({"id": 101, "name": "Fixture user 101", "role": "admin"});
    let settle_one = |key: &str, aid: i64, start: &str, end: &str| {
        let app = &app;
        let admin = admin.clone();
        let key = key.to_owned();
        let start = start.to_owned();
        let end = end.to_owned();
        async move {
            let c = app
                .service
                .mutate(
                    &admin,
                    &format!("{key}-c"),
                    "create",
                    &json!({
                        "account_id": aid,
                        "name": format!("Root {aid} {key}"),
                        "cost": "10.00",
                        "creditor_id": 101,
                        "start": start,
                        "start_mode": "exact",
                        "cost_kind": "exact"
                    }),
                    settlement::service::Prepared::default(),
                    None,
                )
                .await
                .unwrap();
            let cid = c["id"].as_i64().unwrap();
            let preview = app
                .service
                .preview(
                    &admin,
                    &json!({"ids": [cid], "expense_ids": [], "end": end}),
                )
                .await
                .unwrap();
            let payload =
                json!({"preview_id": preview["id"], "renewals": [], "delete_cycle_ids": []});
            let prepared = app
                .service
                .prepare_settlement(&admin, &payload)
                .await
                .unwrap();
            app.service
                .mutate(
                    &admin,
                    &format!("{key}-s"),
                    "settle",
                    &payload,
                    prepared,
                    None,
                )
                .await
                .unwrap();
            cid
        }
    };

    let c100_jan = settle_one(
        "r100-jan",
        100,
        "2024-01-01T00:00:00+00:00",
        "2024-02-01T00:00:00+00:00",
    )
    .await;
    let c200_jan = settle_one(
        "r200-jan",
        200,
        "2024-01-01T00:00:00+00:00",
        "2024-02-01T00:00:00+00:00",
    )
    .await;
    let c200_feb = settle_one(
        "r200-feb",
        200,
        "2024-02-01T00:00:00+00:00",
        "2024-03-01T00:00:00+00:00",
    )
    .await;

    let reparented = [
        account(100, None),
        account(200, None),
        account(105, Some(200)),
    ];
    write_fixture(&fixture, 2, &reparented, &rows);
    sync_now(&app, false).await.unwrap();

    let mut db = app.service.store.read().await.unwrap();
    let d100: i64 = sqlx::query_scalar("SELECT drift FROM settlements WHERE cycle_id=?")
        .bind(c100_jan)
        .fetch_one(&mut *db)
        .await
        .unwrap();
    let d200_jan: i64 = sqlx::query_scalar("SELECT drift FROM settlements WHERE cycle_id=?")
        .bind(c200_jan)
        .fetch_one(&mut *db)
        .await
        .unwrap();
    let d200_feb: i64 = sqlx::query_scalar("SELECT drift FROM settlements WHERE cycle_id=?")
        .bind(c200_feb)
        .fetch_one(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    assert_eq!(
        (d100, d200_jan, d200_feb),
        (1, 1, 0),
        "reparenting Jan child usage drifts both Jan parents while leaving Feb untouched"
    );

    close_app(&app).await;
}

#[tokio::test]
async fn incremental_sync_with_historical_gap_skips_unchanged_settled_vendor_periods() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    let accounts = [account(100, None)];
    let mut rows = vec![
        usage(1, 100, 101, "1.00", "2023-12-31T00:00:00+00:00"),
        usage(2, 100, 102, "10.00", "2024-01-15T00:00:00+00:00"),
    ];
    write_fixture(&fixture, 1, &accounts, &rows);
    let app = new_app(&temporary, &fixture).await;
    sync_now(&app, true).await.unwrap();

    // Persist a historical coverage gap overlapping January 2024 on root 100.
    {
        let mut db = app.service.store.write().await.unwrap();
        settlement::store::put(
            &mut db,
            "coverage_gaps",
            &json!([{
                "id": 9001,
                "start": "2024-01-10T00:00:00.000000+00:00",
                "end": "2024-01-12T00:00:00.000000+00:00",
                "root_id": 100
            }]),
        )
        .await
        .unwrap();
        db.commit().await.unwrap();
    }

    let admin = json!({"id": 101, "name": "Fixture user 101", "role": "admin"});
    let vendor = app
        .service
        .mutate(
            &admin,
            "gap-vendor-create",
            "create-vendor",
            &json!({
                "name": "Gap vendor",
                "cost": "10.00",
                "creditor_id": 101,
                "start": "2024-01-01T00:00:00+00:00",
                "start_mode": "exact",
                "account_ids": [100]
            }),
            settlement::service::Prepared::default(),
            None,
        )
        .await
        .unwrap();
    let pid = vendor["period"]["id"].as_i64().unwrap();
    let pver = vendor["period"]["version"].as_i64().unwrap();
    let csv = "Time,Billed Cost,Model,Request ID,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens\n\
               2024-01-15T00:00:00Z,10.00,gpt-4o,fixture-request-2,1,2,0,0\n";
    let bill_preview = app
        .service
        .vendor_bill_preview(
            &admin,
            pid,
            &json!({
                "version": pver,
                "end": "2024-02-01T00:00:00+00:00",
                "csv": csv,
                "file_name": "jan.csv"
            }),
        )
        .await
        .unwrap();
    let confirm_data = json!({"version": pver, "preview_id": bill_preview["id"]});
    let prepared_bill = app
        .service
        .prepare_vendor_bill(&admin, pid, &confirm_data)
        .await
        .unwrap();
    app.service
        .mutate(
            &admin,
            "gap-vendor-bill-confirm",
            &format!("vendor-bill-confirm:{pid}"),
            &confirm_data,
            settlement::service::Prepared {
                vendor_bill: Some(prepared_bill),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let cid: i64 = {
        let mut db = app.service.store.read().await.unwrap();
        let id = sqlx::query_scalar("SELECT id FROM cycles WHERE vendor_period_id=?")
            .bind(pid)
            .fetch_one(&mut *db)
            .await
            .unwrap();
        db.commit().await.unwrap();
        id
    };
    let preview = app
        .service
        .preview(
            &admin,
            &json!({"ids": [cid], "expense_ids": [], "end": "2024-02-01T00:00:00+00:00"}),
        )
        .await
        .unwrap();
    let settle_payload =
        json!({"preview_id": preview["id"], "renewals": [], "delete_cycle_ids": []});
    let prepared = app
        .service
        .prepare_settlement(&admin, &settle_payload)
        .await
        .unwrap();
    app.service
        .mutate(
            &admin,
            "gap-vendor-settle",
            "settle",
            &settle_payload,
            prepared,
            None,
        )
        .await
        .unwrap();

    // Poison stored vendor_bill_imports.rows temporarily so any spurious vendor_calculation
    // during no-op or out-of-window incremental sync fails loudly.
    let saved_rows: String = {
        let mut db = app.service.store.write().await.unwrap();
        let orig: String =
            sqlx::query_scalar("SELECT rows FROM vendor_bill_imports WHERE vendor_period_id=?")
                .bind(pid)
                .fetch_one(&mut *db)
                .await
                .unwrap();
        sqlx::query("UPDATE vendor_bill_imports SET rows='not-json' WHERE vendor_period_id=?")
            .bind(pid)
            .execute(&mut *db)
            .await
            .unwrap();
        db.commit().await.unwrap();
        orig
    };

    // 1. No-op incremental sync skips drift evaluation despite persisted coverage_gaps.
    sync_now(&app, false).await.unwrap();

    // 2. Out-of-window March append also skips the settled January vendor period.
    rows.push(usage(3, 100, 102, "5.00", "2024-03-05T00:00:00+00:00"));
    write_fixture(&fixture, 2, &accounts, &rows);
    sync_now(&app, false).await.unwrap();

    // 3. Restore valid rows and mutate January usage -> January vendor settlement drifts.
    {
        let mut db = app.service.store.write().await.unwrap();
        sqlx::query("UPDATE vendor_bill_imports SET rows=? WHERE vendor_period_id=?")
            .bind(&saved_rows)
            .bind(pid)
            .execute(&mut *db)
            .await
            .unwrap();
        db.commit().await.unwrap();
    }
    rows[1]["amount"] = json!("12.00");
    write_fixture(&fixture, 3, &accounts, &rows);
    sync_now(&app, true).await.unwrap();

    let drifted: i64 = {
        let mut db = app.service.store.read().await.unwrap();
        let d = sqlx::query_scalar("SELECT drift FROM settlements WHERE cycle_id=?")
            .bind(cid)
            .fetch_one(&mut *db)
            .await
            .unwrap();
        db.commit().await.unwrap();
        d
    };
    assert_eq!(drifted, 1);

    close_app(&app).await;
}
