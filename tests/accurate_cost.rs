mod support;

use serde_json::{Value, json};
use settlement::{error::Result, maintenance, service::Prepared, store};
use support::legacy::{END, Harness, START, admin};

fn cycle_payload(account_id: i64, cost: &str, start: &str) -> Value {
    json!({
        "account_id": account_id,
        "name": format!("Fixture account {account_id}"),
        "cost": cost,
        "creditor_id": 2,
        "start": start,
    })
}

async fn mutate(harness: &Harness, key: &str, action: &str, payload: &Value) -> Result<Value> {
    harness
        .app
        .service
        .mutate(&admin(), key, action, payload, Prepared::default(), None)
        .await
}

async fn insert_cycle(
    harness: &Harness,
    account_id: i64,
    cost_fen: i64,
    cost_kind: &str,
    status: &str,
    start: &str,
    start_locked: i64,
    vendor_period_id: Option<i64>,
) -> i64 {
    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query(
        "INSERT INTO profiles(account_id,name,kind) VALUES(?,?,'paid') ON CONFLICT(account_id) DO UPDATE SET name=excluded.name,kind='paid'",
    )
    .bind(account_id)
    .bind(format!("Fixture account {account_id}"))
    .execute(&mut *db)
    .await
    .unwrap();
    let id = sqlx::query(
        "INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,start_mode,start_lower,start_confirmed,start_locked,cost_kind,vendor_period_id) VALUES(?,?,?,2,?,'exact',?,1,?,?,?)",
    )
    .bind(account_id)
    .bind(format!("Fixture account {account_id}"))
    .bind(cost_fen)
    .bind(start)
    .bind(start)
    .bind(start_locked)
    .bind(cost_kind)
    .bind(vendor_period_id)
    .execute(&mut *db)
    .await
    .unwrap()
    .last_insert_rowid();
    if status != "open" {
        sqlx::query("UPDATE cycles SET status=? WHERE id=?")
            .bind(status)
            .bind(id)
            .execute(&mut *db)
            .await
            .unwrap();
    }
    db.commit().await.unwrap();
    id
}

async fn insert_audit(harness: &Harness, cycle_id: i64, action: &str) {
    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query(
        "INSERT INTO audit(actor_id,action,entity_id,payload,created_at) VALUES(1,?,?,?,?)",
    )
    .bind(action)
    .bind(cycle_id)
    .bind("{\"fixture\":true}")
    .bind("2025-04-01T00:00:00.000000+00:00")
    .execute(&mut *db)
    .await
    .unwrap();
    db.commit().await.unwrap();
}

async fn insert_bill_import(harness: &Harness, cycle_id: i64) -> i64 {
    let mut db = harness.app.service.store.write().await.unwrap();
    let id = sqlx::query(
        "INSERT INTO bill_imports(cycle_id,file_name,file_hash,rows,created_by,created_at) VALUES(?,?,?,?,?,?)",
    )
    .bind(cycle_id)
    .bind("fixture.csv")
    .bind(format!("fixture-{cycle_id}"))
    .bind("[]")
    .bind(1_i64)
    .bind("2025-04-01T00:00:00.000000+00:00")
    .execute(&mut *db)
    .await
    .unwrap()
    .last_insert_rowid();
    db.commit().await.unwrap();
    id
}

async fn insert_active_settlement(harness: &Harness, cycle_id: i64) {
    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query(
        "INSERT INTO settlements(cycle_id,snapshot,digest,created_at,status) VALUES(?,?,?,?, 'active')",
    )
    .bind(cycle_id)
    .bind("{}")
    .bind(format!("settlement-{cycle_id}"))
    .bind("2025-04-01T00:00:00.000000+00:00")
    .execute(&mut *db)
    .await
    .unwrap();
    db.commit().await.unwrap();
}

async fn insert_deletion_task(harness: &Harness, account_id: i64) {
    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query(
        "INSERT INTO account_deletions(account_id,settlement_id,account_name,account_created_at,actor_id,session_id,created_at,updated_at,status) VALUES(?,?,?,?,?,?,?,?, 'queued')",
    )
    .bind(account_id)
    .bind(900_i64 + account_id)
    .bind(format!("Fixture account {account_id}"))
    .bind("2024-12-01T00:00:00.000000+00:00")
    .bind(1_i64)
    .bind("fixture-session")
    .bind("2025-04-01T00:00:00.000000+00:00")
    .bind("2025-04-01T00:00:00.000000+00:00")
    .execute(&mut *db)
    .await
    .unwrap();
    db.commit().await.unwrap();
}

async fn rows(harness: &Harness, table: &str) -> Vec<Value> {
    let mut db = harness.app.service.store.read().await.unwrap();
    let order = if table == "profiles" { "rowid" } else { "id" };
    let rows = store::all(
        &mut db,
        &format!("SELECT * FROM {table} ORDER BY {order}"),
        &[],
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
    rows
}

async fn cycle_row(harness: &Harness, id: i64) -> Value {
    let mut db = harness.app.service.store.read().await.unwrap();
    let row = store::one(
        &mut db,
        "SELECT * FROM cycles WHERE id=?",
        &[json!(id)],
        "cycle missing",
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
    row
}

fn assert_error<T: std::fmt::Debug>(result: Result<T>, status: u16, message: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.status, status, "unexpected error: {error}");
    assert!(
        error.message.contains(message),
        "expected error containing {message:?}, got {:?}",
        error.message
    );
}

#[tokio::test]
async fn maintenance_preview_and_reset_only_cancel_eligible_estimated_cycles() {
    let harness = Harness::new().await;
    let eligible = insert_cycle(&harness, 400, 100, "estimated", "open", START, 0, None).await;
    let active = insert_cycle(&harness, 200, 200, "estimated", "open", START, 0, None).await;
    insert_active_settlement(&harness, active).await;
    let deleting = insert_cycle(&harness, 300, 300, "estimated", "open", START, 0, None).await;
    insert_deletion_task(&harness, 300).await;
    let imported = insert_bill_import(&harness, eligible).await;
    insert_audit(&harness, eligible, "fixture_history").await;
    let usage_before = rows(&harness, "usage").await;

    let digest_before = maintenance::preview(&harness.app.service.store.path)
        .await
        .unwrap();
    assert_eq!(digest_before["count"], 1);
    assert_eq!(digest_before["cycle_ids"], json!([eligible]));
    assert_eq!(digest_before["digest"].as_str().unwrap().len(), 64);

    let reset = maintenance::reset_estimated(
        &harness.app.service.store,
        digest_before["digest"].as_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        reset,
        json!({
            "reset_count": 1,
            "cycle_ids": [eligible],
            "digest": digest_before["digest"],
        })
    );
    assert_eq!(cycle_row(&harness, eligible).await["status"], "cancelled");
    assert_eq!(cycle_row(&harness, active).await["status"], "open");
    assert_eq!(cycle_row(&harness, deleting).await["status"], "open");
    assert!(
        rows(&harness, "profiles")
            .await
            .iter()
            .all(|row| row["account_id"] != 400),
        "the paid profile is deleted only after its final open cycle is cancelled"
    );
    assert!(
        rows(&harness, "profiles")
            .await
            .iter()
            .any(|row| row["account_id"] == 200)
    );
    assert!(
        rows(&harness, "profiles")
            .await
            .iter()
            .any(|row| row["account_id"] == 300)
    );
    assert!(
        rows(&harness, "bill_imports")
            .await
            .iter()
            .any(|row| row["id"] == imported)
    );
    assert!(
        rows(&harness, "audit")
            .await
            .iter()
            .any(|row| row["action"] == "fixture_history" && row["entity_id"] == eligible)
    );
    assert_eq!(rows(&harness, "usage").await, usage_before);

    let retry = maintenance::reset_estimated(
        &harness.app.service.store,
        digest_before["digest"].as_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(retry, reset, "same digest is an idempotent replay");
    assert_eq!(
        maintenance::preview(&harness.app.service.store.path)
            .await
            .unwrap()["count"],
        0
    );
}

#[tokio::test]
async fn stale_estimated_reset_digest_fails_atomically() {
    let harness = Harness::new().await;
    let first = insert_cycle(&harness, 100, 100, "estimated", "open", START, 0, None).await;
    let second = insert_cycle(&harness, 200, 200, "estimated", "open", START, 0, None).await;
    let before = maintenance::preview(&harness.app.service.store.path)
        .await
        .unwrap();
    assert_eq!(before["count"], 2);

    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query("UPDATE cycles SET cost_fen=cost_fen+1 WHERE id=?")
        .bind(second)
        .execute(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();

    assert_error(
        maintenance::reset_estimated(
            &harness.app.service.store,
            before["digest"].as_str().unwrap(),
        )
        .await,
        409,
        "费用配置已变化",
    );
    assert_eq!(cycle_row(&harness, first).await["status"], "open");
    assert_eq!(cycle_row(&harness, second).await["status"], "open");
}

#[tokio::test]
async fn cancelled_cycles_do_not_overlap_but_locked_upgrade_start_remains_a_boundary() {
    let harness = Harness::new().await;
    let cancelled =
        insert_cycle(&harness, 100, 100, "estimated", "cancelled", START, 0, None).await;
    assert_eq!(cycle_row(&harness, cancelled).await["status"], "cancelled");
    let replacement = mutate(
        &harness,
        "cancelled-overlap-replacement",
        "create",
        &cycle_payload(100, "2.00", START),
    )
    .await
    .unwrap();
    assert_eq!(
        cycle_row(&harness, replacement["id"].as_i64().unwrap()).await["status"],
        "open"
    );

    let locked = insert_cycle(&harness, 200, 200, "exact", "cancelled", END, 1, None).await;
    assert_eq!(cycle_row(&harness, locked).await["start_locked"], 1);
    assert_error(
        mutate(
            &harness,
            "locked-start-before-boundary",
            "create",
            &cycle_payload(200, "3.00", START),
        )
        .await,
        409,
        "不能早于已锁定的升级付费起点",
    );
    let at_boundary = mutate(
        &harness,
        "locked-start-at-boundary",
        "create",
        &cycle_payload(200, "3.00", END),
    )
    .await
    .unwrap();
    assert_eq!(
        cycle_row(&harness, at_boundary["id"].as_i64().unwrap()).await["start"],
        "2025-02-01T00:00:00.000000+00:00"
    );
}

#[tokio::test]
async fn accurate_cost_is_required_while_free_and_start_estimate_contracts_remain() {
    let harness = Harness::new().await;
    let free = json!({"name":"Fixture free account","version":null});
    let free_result = mutate(&harness, "free-300", "free:300", &free)
        .await
        .unwrap();
    assert_eq!(
        mutate(&harness, "free-300", "free:300", &free)
            .await
            .unwrap(),
        free_result
    );
    let mut changed_free = free.clone();
    changed_free["name"] = json!("changed");
    assert_error(
        mutate(&harness, "free-300", "free:300", &changed_free).await,
        409,
        "其他请求",
    );

    let estimated_upgrade = json!({
        "name":"Estimated upgrade",
        "cost":"10.00",
        "creditor_id":2,
        "cost_kind":"estimated",
        "version":1,
    });
    assert_error(
        mutate(
            &harness,
            "upgrade-estimated",
            "upgrade-paid:300",
            &estimated_upgrade,
        )
        .await,
        422,
        "准确成本",
    );
    assert!(rows(&harness, "cycles").await.is_empty());

    let exact_upgrade = json!({
        "name":"Exact upgrade",
        "cost":"10.00",
        "creditor_id":2,
        "cost_kind":"exact",
        "version":1,
    });
    let upgraded = mutate(
        &harness,
        "upgrade-exact",
        "upgrade-paid:300",
        &exact_upgrade,
    )
    .await
    .unwrap();
    assert_eq!(
        mutate(
            &harness,
            "upgrade-exact",
            "upgrade-paid:300",
            &exact_upgrade
        )
        .await
        .unwrap(),
        upgraded
    );
    let mut changed_upgrade = exact_upgrade.clone();
    changed_upgrade["cost"] = json!("11.00");
    assert_error(
        mutate(
            &harness,
            "upgrade-exact",
            "upgrade-paid:300",
            &changed_upgrade,
        )
        .await,
        409,
        "其他请求",
    );
    let upgraded_cycle = cycle_row(&harness, upgraded["cycle_id"].as_i64().unwrap()).await;
    assert_eq!(upgraded_cycle["cost_kind"], "exact");
    assert_eq!(upgraded_cycle["start_locked"], 1);

    let mut estimated_create = cycle_payload(400, "1.00", START);
    estimated_create["cost_kind"] = json!("estimated");
    assert_error(
        mutate(&harness, "create-estimated", "create", &estimated_create).await,
        422,
        "准确成本",
    );
    let mut date_estimate = cycle_payload(400, "1.00", "2025-01-10T14:30:59+08:00");
    date_estimate["start_mode"] = json!("date_estimate");
    date_estimate["cost_kind"] = json!("exact");
    let date_cycle = mutate(&harness, "create-date-estimate", "create", &date_estimate)
        .await
        .unwrap();
    let date_id = date_cycle["id"].as_i64().unwrap();
    let row = cycle_row(&harness, date_id).await;
    assert_eq!(row["cost_kind"], "exact");
    assert_eq!(row["start_confirmed"], 0);
    let mut stale_edit = date_estimate.clone();
    stale_edit["version"] = json!(0);
    assert_error(
        mutate(
            &harness,
            "date-estimate-stale-edit",
            &format!("edit:{date_id}"),
            &stale_edit,
        )
        .await,
        409,
        "周期已更新",
    );
}
