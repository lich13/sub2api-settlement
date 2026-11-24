use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use settlement::{config::Config, http::App, service::Prepared, store};
use tempfile::TempDir;

const CYCLE_START: &str = "2025-01-01T00:00:00+00:00";
const CYCLE_END: &str = "2025-02-01T00:00:00+00:00";

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

fn admin() -> Value {
    json!({"id": 1, "name": "Fixture Admin", "role": "admin"})
}

fn payer() -> Value {
    json!({"id": 3, "name": "Fixture Payer", "role": "user"})
}

fn base_source() -> Value {
    serde_json::from_str(include_str!("compat/source.json")).unwrap()
}

fn source_without_usage(removed_ids: &[i64], cleanups: Value) -> Value {
    let mut source = base_source();
    let remaining_usage = source["usage"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| !removed_ids.contains(&row["id"].as_i64().unwrap()))
        .cloned()
        .collect::<Vec<_>>();
    source["usage"] = json!(remaining_usage);
    source["cleanups"] = cleanups;
    source
}

fn gap_fixture() -> Value {
    source_without_usage(
        &[2],
        json!([{
            "id": 9001,
            "filters": {
                "start_time": "2025-01-09T00:00:00+00:00",
                "end_time": "2025-01-21T00:00:00+00:00",
                "account_id": 100,
                "user_id": 3
            },
            "deleted_rows": 2,
            "finished_at": "2025-04-01T00:00:00+00:00"
        }]),
    )
}

async fn meta_value(app: &App, key: &str) -> Value {
    let mut db = app.service.store.read().await.unwrap();
    let value = store::meta(&mut db, key).await.unwrap();
    db.commit().await.unwrap();
    value
}

async fn gaps(app: &App) -> Value {
    meta_value(app, "coverage_gaps").await
}

fn cycle_payload(account_id: i64, start: &str) -> Value {
    json!({
        "account_id": account_id,
        "name": format!("Fixture account {account_id}"),
        "cost": "10.00",
        "creditor_id": 2,
        "start": start,
        "start_mode": "exact",
        "cost_kind": "exact"
    })
}

async fn create_cycle(app: &App, key: &str, account_id: i64, start: &str) -> Value {
    app.service
        .mutate(
            &admin(),
            key,
            "create",
            &cycle_payload(account_id, start),
            Prepared::default(),
            None,
        )
        .await
        .expect("create cycle")
}

async fn settle_cycle(app: &App, key: &str, cycle_id: i64, end: &str) -> Value {
    let preview = app
        .service
        .preview(
            &admin(),
            &json!({"ids": [cycle_id], "expense_ids": [], "end": end}),
        )
        .await
        .expect("preview settlement");
    let payload = json!({
        "preview_id": preview["id"],
        "renewals": [],
        "delete_cycle_ids": []
    });
    let prepared = app
        .service
        .prepare_settlement(&admin(), &payload)
        .await
        .expect("prepare settlement");
    app.service
        .mutate(&admin(), key, "settle", &payload, prepared, None)
        .await
        .expect("settle cycle")
}

async fn assert_cycle_rejected_without_writes(app: &App) {
    let admin = json!({"id": 1, "name": "Fixture Admin", "role": "admin"});
    let payload = json!({
        "account_id": 100,
        "name": "Fixture restart-gap cycle",
        "cost": "10.00",
        "creditor_id": 2,
        "start": CYCLE_START,
        "start_mode": "exact",
        "cost_kind": "exact"
    });
    let mut db = app.service.store.write().await.unwrap();
    let error = app
        .service
        .create_cycle(&mut db, &admin, &payload)
        .await
        .unwrap_err();
    assert_eq!(error.status, 422);
    assert!(
        error.message.contains("该周期存在未采集的历史记录"),
        "unexpected rejection: {}",
        error.message
    );

    let cycles = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cycles")
        .fetch_one(&mut *db)
        .await
        .unwrap();
    let audit = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit")
        .fetch_one(&mut *db)
        .await
        .unwrap();
    assert_eq!(cycles, 0);
    assert_eq!(audit, 0);
    db.rollback().await.unwrap();
}

#[tokio::test]
async fn persisted_coverage_gap_blocks_cycle_before_and_after_restart_sync() {
    let temporary = TempDir::new().unwrap();
    let fixture = temporary.path().join("source.json");
    std::fs::write(
        &fixture,
        serde_json::to_vec(
            &serde_json::from_str::<Value>(include_str!("compat/source.json")).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let app = App::new(config(temporary.path(), fixture.clone()))
        .await
        .unwrap();
    app.service.sync(true).await.unwrap();

    std::fs::write(&fixture, serde_json::to_vec(&gap_fixture()).unwrap()).unwrap();
    app.service.sync(true).await.unwrap();
    let persisted = gaps(&app).await;
    assert_eq!(persisted.as_array().unwrap().len(), 1);
    assert_eq!(persisted[0]["root_id"], 100);

    app.service.source.close().await;
    app.service.store.close().await;

    let restarted = App::new(config(temporary.path(), fixture)).await.unwrap();
    assert_eq!(gaps(&restarted).await, persisted);
    // App::new reopens the persisted database but does not sync the source.
    // This is the first cycle validation after restart.
    assert_cycle_rejected_without_writes(&restarted).await;

    restarted.service.sync(true).await.unwrap();
    assert_eq!(gaps(&restarted).await, persisted);
    assert_cycle_rejected_without_writes(&restarted).await;

    restarted.service.source.close().await;
    restarted.service.store.close().await;
}

#[tokio::test]
async fn cleanup_before_coverage_and_inclusive_end_boundary_enforce_exact_window() {
    // 1. cleanup-before-coverage: cleanup ending at 2024-12-31T23:59:59.999999+00:00
    // converts to half-open end 2025-01-01T00:00:00.000000+00:00 and does not block >= 2025-01-01.
    let before_dir = TempDir::new().unwrap();
    let before_fixture = before_dir.path().join("source.json");
    std::fs::write(&before_fixture, serde_json::to_vec(&base_source()).unwrap()).unwrap();
    let app = App::new(config(before_dir.path(), before_fixture.clone()))
        .await
        .unwrap();
    app.service.sync(true).await.unwrap();
    std::fs::write(
        &before_fixture,
        serde_json::to_vec(&source_without_usage(
            &[],
            json!([{
                "id": 9001,
                "filters": {
                    "start_time": "2024-12-01T00:00:00+00:00",
                    "end_time": "2024-12-31T23:59:59.999999+00:00",
                    "account_id": 100
                },
                "deleted_rows": 500,
                "finished_at": "2025-04-01T00:00:00.000000+00:00"
            }]),
        ))
        .unwrap(),
    )
    .unwrap();
    app.service.sync(true).await.unwrap();
    assert_eq!(
        meta_value(&app, "cleanup_counts").await,
        json!({"9001": 500})
    );
    assert_eq!(
        gaps(&app).await,
        json!([{
            "end": "2025-01-01T00:00:00.000000+00:00",
            "id": 9001,
            "root_id": 100,
            "start": "2024-12-01T00:00:00.000000+00:00"
        }])
    );
    let cycle = create_cycle(&app, "before-coverage-create", 100, CYCLE_START).await;
    settle_cycle(
        &app,
        "before-coverage-settle",
        cycle["id"].as_i64().unwrap(),
        CYCLE_END,
    )
    .await;
    app.service.source.close().await;
    app.service.store.close().await;

    // 2. cleanup-end-boundary: end_time 2025-01-10T08:00:00+08:00 (= 2025-01-10T00:00:00Z)
    // is inclusive, so start == 2025-01-10T00:00:00Z is blocked (422) while +1us succeeds.
    let end_dir = TempDir::new().unwrap();
    let end_fixture = end_dir.path().join("source.json");
    std::fs::write(&end_fixture, serde_json::to_vec(&base_source()).unwrap()).unwrap();
    let app = App::new(config(end_dir.path(), end_fixture.clone()))
        .await
        .unwrap();
    app.service.sync(true).await.unwrap();
    let early = create_cycle(&app, "end-boundary-early", 100, CYCLE_START).await;
    std::fs::write(
        &end_fixture,
        serde_json::to_vec(&source_without_usage(
            &[2],
            json!([{
                "id": 9001,
                "filters": {
                    "start_time": "2025-01-09T00:00:00+00:00",
                    "end_time": "2025-01-10T08:00:00+08:00",
                    "account_id": 100
                },
                "deleted_rows": 2,
                "finished_at": "2025-04-01T00:00:00.000000+00:00"
            }]),
        ))
        .unwrap(),
    )
    .unwrap();
    app.service.sync(true).await.unwrap();
    // Settling before the gap start (2025-01-09) succeeds.
    settle_cycle(
        &app,
        "end-boundary-before-gap",
        early["id"].as_i64().unwrap(),
        "2025-01-09T00:00:00+00:00",
    )
    .await;
    let inclusive_err = app
        .service
        .mutate(
            &admin(),
            "end-inclusive-blocked",
            "create",
            &cycle_payload(100, "2025-01-10T00:00:00+00:00"),
            Prepared::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(inclusive_err.status, 422);
    assert!(inclusive_err.message.contains("未采集的历史记录"));

    let next_instant = create_cycle(
        &app,
        "end-exclusive-allowed",
        100,
        "2025-01-10T00:00:00.000001+00:00",
    )
    .await;
    settle_cycle(
        &app,
        "end-exclusive-settle",
        next_instant["id"].as_i64().unwrap(),
        CYCLE_END,
    )
    .await;
    app.service.source.close().await;
    app.service.store.close().await;
}

#[tokio::test]
async fn cleanup_evidence_accounting_prevents_reuse_and_cross_scope_borrowing() {
    // 1. cleanup-count-evidence + cleanup-deferred-restart:
    // Observed missing rows across restarts satisfy deleted_rows without recording a gap.
    let ok_dir = TempDir::new().unwrap();
    let ok_fixture = ok_dir.path().join("source.json");
    std::fs::write(&ok_fixture, serde_json::to_vec(&base_source()).unwrap()).unwrap();
    let app = App::new(config(ok_dir.path(), ok_fixture.clone()))
        .await
        .unwrap();
    app.service.sync(true).await.unwrap();
    let cycle = create_cycle(&app, "evidence-ok-cycle", 100, CYCLE_START).await;
    // Usage 2 disappears before cleanup row appears, then restart + cleanup deleted_rows=1 arrives.
    std::fs::write(
        &ok_fixture,
        serde_json::to_vec(&source_without_usage(&[2], json!([]))).unwrap(),
    )
    .unwrap();
    app.service.sync(true).await.unwrap();
    app.service.source.close().await;
    app.service.store.close().await;

    let app = App::new(config(ok_dir.path(), ok_fixture.clone()))
        .await
        .unwrap();
    std::fs::write(
        &ok_fixture,
        serde_json::to_vec(&source_without_usage(
            &[2],
            json!([{
                "id": 9001,
                "filters": {
                    "start_time": "2025-01-09T00:00:00+00:00",
                    "end_time": "2025-01-21T00:00:00+00:00",
                    "account_id": 100,
                    "user_id": 3
                },
                "deleted_rows": 1,
                "finished_at": "2025-04-01T00:00:00+00:00"
            }]),
        ))
        .unwrap(),
    )
    .unwrap();
    app.service.sync(true).await.unwrap();
    assert_eq!(gaps(&app).await, json!([]));
    assert_eq!(meta_value(&app, "cleanup_counts").await, json!({"9001": 1}));

    // 2. cleanup-evidence-not-reused: a second cleanup 9002 cannot reuse usage 2's deletion evidence.
    std::fs::write(
        &ok_fixture,
        serde_json::to_vec(&source_without_usage(
            &[2],
            json!([
                {
                    "id": 9001,
                    "filters": {
                        "start_time": "2025-01-09T00:00:00+00:00",
                        "end_time": "2025-01-21T00:00:00+00:00",
                        "account_id": 100,
                        "user_id": 3
                    },
                    "deleted_rows": 1,
                    "finished_at": "2025-04-01T00:00:00+00:00"
                },
                {
                    "id": 9002,
                    "filters": {
                        "start_time": "2025-01-09T00:00:00+00:00",
                        "end_time": "2025-01-21T00:00:00+00:00",
                        "account_id": 100,
                        "user_id": 3
                    },
                    "deleted_rows": 1,
                    "finished_at": "2025-04-01T00:00:00+00:00"
                }
            ]),
        ))
        .unwrap(),
    )
    .unwrap();
    app.service.sync(true).await.unwrap();
    assert_eq!(gaps(&app).await.as_array().unwrap().len(), 1);
    assert_eq!(gaps(&app).await[0]["id"], 9002);

    // 3. cleanup-count-exceeds-evidence: open cycle preview & cycle_detail fail 409, while cost edit still works.
    let cid = cycle["id"].as_i64().unwrap();
    let preview_err = app
        .service
        .preview(
            &admin(),
            &json!({"ids": [cid], "expense_ids": [], "end": CYCLE_END}),
        )
        .await
        .unwrap_err();
    assert_eq!(preview_err.status, 409);
    for actor in [admin(), payer()] {
        let detail_err = app.service.cycle_detail(&actor, cid).await.unwrap_err();
        assert_eq!(detail_err.status, 409);
    }
    let edited = app
        .service
        .mutate(
            &admin(),
            "gap-open-cost-edit",
            &format!("edit:{cid}"),
            &json!({
                "account_id": 100,
                "name": "Fixture account 100",
                "cost": "12.00",
                "creditor_id": 2,
                "start": CYCLE_START,
                "start_mode": "exact",
                "cost_kind": "exact",
                "version": 1
            }),
            Prepared::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(edited["version"], 2);
    app.service.source.close().await;
    app.service.store.close().await;
}

#[tokio::test]
async fn cleanup_scope_isolation_settled_locality_and_stale_sync_masking() {
    // 1. No cross-account borrow (missing usage 2 + usage 8 on account 400) &
    //    no cross-user borrow (missing usage 2 + usage 9 on user 4) when filter is (account 100, user 3).
    for (label, removed) in [
        ("no-account-borrow", &[2_i64, 8][..]),
        ("no-user-borrow", &[2_i64, 9][..]),
    ] {
        let dir = TempDir::new().unwrap();
        let fixture = dir.path().join("source.json");
        std::fs::write(&fixture, serde_json::to_vec(&base_source()).unwrap()).unwrap();
        let app = App::new(config(dir.path(), fixture.clone())).await.unwrap();
        app.service.sync(true).await.unwrap();
        let cycle = create_cycle(&app, &format!("{label}-cycle"), 100, CYCLE_START).await;
        std::fs::write(
            &fixture,
            serde_json::to_vec(&source_without_usage(
                removed,
                json!([{
                    "id": 9001,
                    "filters": {
                        "start_time": "2025-01-09T00:00:00+00:00",
                        "end_time": "2025-01-21T00:00:00+00:00",
                        "account_id": 100,
                        "user_id": 3
                    },
                    "deleted_rows": 2,
                    "finished_at": "2025-04-01T00:00:00+00:00"
                }]),
            ))
            .unwrap(),
        )
        .unwrap();
        app.service.sync(true).await.unwrap();
        assert_eq!(gaps(&app).await.as_array().unwrap().len(), 1, "{label}");
        let err = app
            .service
            .preview(
                &admin(),
                &json!({"ids": [cycle["id"]], "expense_ids": [], "end": CYCLE_END}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{label}");
        app.service.source.close().await;
        app.service.store.close().await;
    }

    // 2. Partial parent (100), shadow (101), and nested shadow (102) scopes all map to root 100
    //    while leaving account 200 unaffected.
    for (filter_account, removed_usage) in [(100, 2_i64), (101, 3_i64), (102, 4_i64)] {
        let dir = TempDir::new().unwrap();
        let fixture = dir.path().join("source.json");
        std::fs::write(&fixture, serde_json::to_vec(&base_source()).unwrap()).unwrap();
        let app = App::new(config(dir.path(), fixture.clone())).await.unwrap();
        app.service.sync(true).await.unwrap();
        std::fs::write(
            &fixture,
            serde_json::to_vec(&source_without_usage(
                &[removed_usage],
                json!([{
                    "id": 9001,
                    "filters": {
                        "start_time": "2025-01-09T00:00:00+00:00",
                        "end_time": "2025-01-21T00:00:00+00:00",
                        "account_id": filter_account
                    },
                    "deleted_rows": 2,
                    "finished_at": "2025-04-01T00:00:00+00:00"
                }]),
            ))
            .unwrap(),
        )
        .unwrap();
        app.service.sync(true).await.unwrap();
        assert_eq!(gaps(&app).await[0]["root_id"], 100);
        let blocked = app
            .service
            .mutate(
                &admin(),
                "root-blocked",
                "create",
                &cycle_payload(100, CYCLE_START),
                Prepared::default(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(blocked.status, 422);
        let unaffected = create_cycle(&app, "other-root-ok", 200, CYCLE_START).await;
        settle_cycle(
            &app,
            "other-root-settle",
            unaffected["id"].as_i64().unwrap(),
            CYCLE_END,
        )
        .await;
        app.service.source.close().await;
        app.service.store.close().await;
    }

    // 3. cleanup-gap-drift-locality + cleanup-stale-sync-masks-open:
    // Settled snapshots stay readable (200) across later gaps and stale syncs, while open cycle_detail fails 503 on stale sync.
    let dir = TempDir::new().unwrap();
    let fixture = dir.path().join("source.json");
    std::fs::write(&fixture, serde_json::to_vec(&base_source()).unwrap()).unwrap();
    let app = App::new(config(dir.path(), fixture.clone())).await.unwrap();
    app.service.sync(true).await.unwrap();
    let settled = create_cycle(&app, "locality-settled", 100, CYCLE_START).await;
    let settled_id = settled["id"].as_i64().unwrap();
    settle_cycle(&app, "locality-settle", settled_id, CYCLE_END).await;
    let open = create_cycle(&app, "locality-open", 200, CYCLE_START).await;
    let open_id = open["id"].as_i64().unwrap();

    // Introduce gap on root 100 after settlement: settled cycle_detail still returns 200, edit returns 409.
    std::fs::write(&fixture, serde_json::to_vec(&gap_fixture()).unwrap()).unwrap();
    app.service.sync(true).await.unwrap();
    assert!(app.service.cycle_detail(&admin(), settled_id).await.is_ok());
    let locked_edit = app
        .service
        .mutate(
            &admin(),
            "locked-edit",
            &format!("edit:{settled_id}"),
            &json!({
                "account_id": 100,
                "name": "Fixture account 100",
                "cost": "12.00",
                "creditor_id": 2,
                "start": CYCLE_START,
                "start_mode": "exact",
                "cost_kind": "exact",
                "version": 2
            }),
            Prepared::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(locked_edit.status, 409);

    // Force stale last_sync: overview stays 200 with sync_error, settled detail stays 200, open detail fails 503.
    let mut db = app.service.store.write().await.unwrap();
    store::exec(
        &mut db,
        "UPDATE meta SET value=? WHERE key='last_sync'",
        &[json!("\"2025-01-01T00:00:00+00:00\"")],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
    let overview = app.service.overview(&payer()).await.unwrap();
    assert!(!overview["sync_error"].is_null());
    assert!(app.service.cycle_detail(&payer(), settled_id).await.is_ok());
    let stale_open = app
        .service
        .cycle_detail(&payer(), open_id)
        .await
        .unwrap_err();
    assert_eq!(stale_open.status, 503);

    app.service.source.close().await;
    app.service.store.close().await;
}
