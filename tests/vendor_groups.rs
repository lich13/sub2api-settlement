#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use settlement::{error::Result, service::Prepared, store};
use support::legacy::{END, Harness, START, admin};

mod error {
    pub use settlement::error::{Error, Result};
}
mod json {
    pub use settlement::json::{array, int, text};
}
mod time {
    pub use settlement::time::*;
}
#[path = "../src/input.rs"]
#[allow(dead_code)]
mod input_contract;

async fn mutate(harness: &Harness, key: &str, action: &str, payload: &Value) -> Result<Value> {
    harness
        .app
        .service
        .mutate(&admin(), key, action, payload, Prepared::default(), None)
        .await
}

async fn vendor_period_row(harness: &Harness, id: i64) -> Value {
    let mut db = harness.app.service.store.read().await.unwrap();
    let row = store::one(
        &mut db,
        "SELECT * FROM vendor_periods WHERE id=?",
        &[json!(id)],
        "vendor period missing",
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
    row
}

async fn cycle_for_account(harness: &Harness, account_id: i64) -> i64 {
    harness
        .rows("cycles")
        .await
        .into_iter()
        .rev()
        .find(|cycle| cycle["account_id"] == account_id && cycle["vendor_period_id"].is_i64())
        .unwrap_or_else(|| panic!("vendor cycle for account {account_id} not found"))["id"]
        .as_i64()
        .unwrap()
}

fn assert_error<T>(result: Result<T>, status: u16, clue: &str) {
    let error = match result {
        Ok(_) => panic!("operation should fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, status, "unexpected error: {error}");
    assert!(
        error.message.contains(clue),
        "expected business clue {clue:?}, got {:?}",
        error.message
    );
}

async fn create_group(harness: &Harness, account_ids: &[i64], key: &str) -> Value {
    mutate(
        harness,
        key,
        "create-vendor",
        &json!({
            "name":"Fixture account group",
            "cost":"6.00",
            "creditor_id":2,
            "account_ids":account_ids,
            "start":START,
            "start_mode":"exact"
        }),
    )
    .await
    .expect("create account group")
}

fn period_from_vendor(vendor: &Value) -> Value {
    vendor
        .get("period")
        .cloned()
        .unwrap_or_else(|| panic!("create-vendor must return period: {vendor}"))
}

fn bill_csv() -> String {
    concat!(
        "Time,Billed Cost,Model,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens,Request ID\n",
        "2025-01-01T00:00:00+00:00,1.00,fixture-model,10,20,0,0,fixture-1\n",
        "2025-01-10T00:00:00+00:00,3.00,fixture-model,10,20,0,0,fixture-2\n",
        "2025-01-05T00:00:00+00:00,2.00,fixture-model,10,20,0,0,fixture-6\n"
    )
    .to_owned()
}

async fn confirm_vendor_bill(harness: &Harness, period: &Value) {
    let preview = harness
        .app
        .service
        .vendor_bill_preview(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({
                "version":period["version"],
                "end":END,
                "allocation_mode":"proportional",
                "file_name":"fixture-vendor.csv",
                "csv":bill_csv()
            }),
        )
        .await
        .expect("preview group bill");
    let prepared = harness
        .app
        .service
        .prepare_vendor_bill(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"preview_id":preview["id"]}),
        )
        .await
        .expect("prepare group bill");
    harness
        .app
        .service
        .mutate(
            &admin(),
            &format!("vendor-bill-confirm-key-{}", period["id"].as_i64().unwrap()),
            &format!("vendor-bill-confirm:{}", period["id"].as_i64().unwrap()),
            &json!({
                "preview_id":preview["id"],
                "version":period["version"],
                "accept_difference":false,
                "accept_outside":false
            }),
            Prepared {
                vendor_bill: Some(prepared),
                ..Prepared::default()
            },
            None,
        )
        .await
        .expect("confirm group bill");
}

async fn settle_group(
    harness: &Harness,
    cycle_ids: &[i64],
    renewals: Value,
    key: &str,
) -> Result<Value> {
    let preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":cycle_ids,"expense_ids":[],"end":END}),
        )
        .await?;
    let payload = json!({
        "preview_id":preview["id"],
        "renewals":[],
        "delete_cycle_ids":[],
        "vendor_renewals":renewals
    });
    let prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &payload)
        .await?;
    harness
        .app
        .service
        .mutate(&admin(), key, "settle", &payload, prepared, None)
        .await
}

async fn settle_group_with_deletes(
    harness: &Harness,
    cycle_ids: &[i64],
    delete_cycle_ids: &[i64],
    renewals: Value,
    key: &str,
) -> Result<Value> {
    let preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":cycle_ids,"expense_ids":[],"end":END}),
        )
        .await?;
    let payload = json!({
        "preview_id":preview["id"],
        "renewals":[],
        "delete_cycle_ids":delete_cycle_ids,
        "vendor_renewals":renewals
    });
    let prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &payload)
        .await?;
    harness
        .app
        .service
        .mutate(&admin(), key, "settle", &payload, prepared, None)
        .await
}

async fn mark_deletion_failed(harness: &Harness, account_id: i64) {
    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query("UPDATE account_deletions SET status='failed' WHERE account_id=?")
        .bind(account_id)
        .execute(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
}

async fn insert_cancelled_locked_cycle(harness: &Harness, account_id: i64, start: &str) -> i64 {
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
        "INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,start_mode,start_lower,start_confirmed,start_locked,cost_kind) VALUES(?,?,?,2,?,'exact',?,1,1,'exact')",
    )
    .bind(account_id)
    .bind(format!("Fixture account {account_id}"))
    .bind(100_i64)
    .bind(start)
    .bind(start)
    .execute(&mut *db)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query("UPDATE cycles SET status='cancelled' WHERE id=?")
        .bind(id)
        .execute(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    id
}

#[test]
fn vendor_input_normalization_keeps_group_payload_contract() {
    let created = input_contract::normalize(
        "create-vendor",
        json!({
            "name":"Fixture account group",
            "cost":"6.00",
            "creditor_id":"2",
            "account_ids":[100,200],
            "start":START
        }),
    )
    .expect("normalize create-vendor payload");
    assert_eq!(created["creditor_id"], 2);
    assert_eq!(created["account_ids"], json!([100, 200]));
    assert_eq!(created["start_mode"], "exact");

    let appended = input_contract::normalize(
        "add-vendor-members",
        json!({"version":"3","account_ids":[400]}),
    )
    .expect("normalize append payload");
    assert_eq!(appended["version"], 3);
    assert!(appended["period_version"].is_null());

    let renewed = input_contract::normalize(
        "renew-vendor",
        json!({"version":"4","period_version":"9","cost":"7.00","creditor_id":"2"}),
    )
    .expect("normalize direct renewal payload");
    assert_eq!(renewed["version"], 4);
    assert_eq!(renewed["period_version"], 9);
    assert!(renewed.get("cycle_ids").is_none());

    let settled = input_contract::normalize(
        "settle",
        json!({
            "preview_id":"fixture-preview",
            "renewals":[],
            "delete_cycle_ids":[],
            "vendor_renewals":[{"period_id":"9","version":"4","cost":"7.00","creditor_id":"2"}]
        }),
    )
    .expect("normalize settlement renewal payload");
    assert!(settled["vendor_renewals"][0].get("cycle_ids").is_none());
    assert_eq!(settled["vendor_renewals"][0]["period_id"], 9);
}

#[tokio::test]
async fn independent_create_and_group_create_have_one_winner_for_one_account() {
    let harness = Harness::new().await;
    let peer = harness.peer().await;
    let independent = json!({
        "account_id":400,
        "name":"Independent winner",
        "cost":"1.00",
        "creditor_id":2,
        "start":START,
        "start_mode":"exact",
        "cost_kind":"exact"
    });
    let grouped = json!({
        "name":"Group winner",
        "cost":"6.00",
        "creditor_id":2,
        "account_ids":[400],
        "start":START,
        "start_mode":"exact"
    });
    let actor = admin();
    let (independent_result, grouped_result) = tokio::join!(
        harness.app.service.mutate(
            &actor,
            "race-independent-create",
            "create",
            &independent,
            Prepared::default(),
            None,
        ),
        peer.service.mutate(
            &actor,
            "race-group-create",
            "create-vendor",
            &grouped,
            Prepared::default(),
            None,
        )
    );
    let independent_won = independent_result.is_ok();
    let grouped_won = grouped_result.is_ok();
    assert_eq!(
        independent_won as u8 + grouped_won as u8,
        1,
        "exactly one create path may win: independent={independent_result:?}, grouped={grouped_result:?}"
    );
    let loser = if independent_won {
        grouped_result.expect_err("group create must lose the race")
    } else {
        independent_result.expect_err("independent create must lose the race")
    };
    assert_eq!(
        loser.status, 409,
        "losing create must be a business conflict: {loser}"
    );
    assert_eq!(
        harness
            .rows("profiles")
            .await
            .into_iter()
            .filter(|profile| profile["account_id"] == 400)
            .count(),
        1
    );
    assert_eq!(
        harness
            .rows("cycles")
            .await
            .into_iter()
            .filter(|cycle| cycle["account_id"] == 400)
            .count(),
        1
    );
    let vendors = harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(vendors, grouped_won as usize);
}

#[tokio::test]
async fn create_vendor_group_is_atomic_and_rejects_ineligible_mothers() {
    let harness = Harness::new().await;
    let rejected = mutate(
        &harness,
        "group-atomic-shadow",
        "create-vendor",
        &json!({
            "name":"Invalid account group",
            "cost":"6.00",
            "creditor_id":2,
            "account_ids":[100,101],
            "start":START,
            "start_mode":"exact"
        }),
    )
    .await;
    assert_error(rejected, 409, "待配置母账号");
    assert!(
        harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
            .as_array()
            .unwrap()
            .is_empty(),
        "failed creation must not leave a vendor"
    );
    assert!(harness.rows("cycles").await.is_empty());
    assert!(harness.rows("profiles").await.is_empty());

    let vendor = create_group(&harness, &[100, 200], "group-atomic-valid").await;
    let period = period_from_vendor(&vendor);
    assert_eq!(period["vendor_id"], vendor["id"]);
    assert_eq!(period["status"], "open");
    for account_id in [100, 200] {
        let cycle = harness
            .rows("cycles")
            .await
            .into_iter()
            .find(|c| c["account_id"] == account_id)
            .expect("created group cycle");
        assert_eq!(cycle["vendor_id"], vendor["id"]);
        assert_eq!(cycle["vendor_period_id"], period["id"]);
        let profile = harness
            .rows("profiles")
            .await
            .into_iter()
            .find(|p| p["account_id"] == account_id)
            .expect("created group profile");
        assert_eq!(profile["vendor_id"], vendor["id"]);
    }
}

#[tokio::test]
async fn vendor_group_rejects_paid_free_deleted_and_shadow_accounts() {
    let paid = Harness::new().await;
    mutate(
        &paid,
        "qualification-paid",
        "create",
        &json!({
            "account_id":300,
            "name":"Already paid",
            "cost":"1.00",
            "creditor_id":2,
            "start":START,
            "start_mode":"exact",
            "cost_kind":"exact"
        }),
    )
    .await
    .expect("create paid qualification fixture");
    assert_error(
        mutate(
            &paid,
            "qualification-paid-group",
            "create-vendor",
            &json!({"name":"Paid","cost":"6.00","creditor_id":2,"account_ids":[300],"start":START,"start_mode":"exact"}),
        )
        .await,
        409,
        "待配置",
    );

    let free = Harness::new().await;
    mutate(
        &free,
        "qualification-free",
        "free:400",
        &json!({"name":"Free account","version":null}),
    )
    .await
    .expect("create free qualification fixture");
    assert_error(
        mutate(
            &free,
            "qualification-free-group",
            "create-vendor",
            &json!({"name":"Free","cost":"6.00","creditor_id":2,"account_ids":[400],"start":START,"start_mode":"exact"}),
        )
        .await,
        409,
        "待配置",
    );

    let deleted = Harness::new().await;
    let mut source = deleted.source();
    source["accounts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|account| account["id"] == 500)
        .unwrap()["deleted"] = json!(true);
    deleted.write_source(&source);
    deleted
        .app
        .service
        .sync(true)
        .await
        .expect("sync deleted account");
    assert_error(
        mutate(
            &deleted,
            "qualification-deleted-group",
            "create-vendor",
            &json!({"name":"Deleted","cost":"6.00","creditor_id":2,"account_ids":[500],"start":START,"start_mode":"exact"}),
        )
        .await,
        409,
        "待配置母账号",
    );

    let shadow = Harness::new().await;
    assert_error(
        mutate(
            &shadow,
            "qualification-shadow-group",
            "create-vendor",
            &json!({"name":"Shadow","cost":"6.00","creditor_id":2,"account_ids":[101],"start":START,"start_mode":"exact"}),
        )
        .await,
        409,
        "待配置母账号",
    );
}

#[tokio::test]
async fn add_vendor_members_is_versioned_and_persists_current_membership() {
    let harness = Harness::new().await;
    let vendor = create_group(&harness, &[100, 200], "group-add-create").await;
    let period = period_from_vendor(&vendor);
    let stale_vendor_version = vendor["version"].clone();
    let stale_period_version = period["version"].clone();
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    let added = mutate(
        &harness,
        "group-add-member",
        &format!("add-vendor-members:{}", vendor["id"].as_i64().unwrap()),
        &json!({
            "version":stale_vendor_version,
            "period_version":stale_period_version,
            "account_ids":[400]
        }),
    )
    .await
    .expect("append unconfigured member");
    assert_eq!(added["id"], vendor["id"]);
    let third = cycle_for_account(&harness, 400).await;
    let period_after = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    assert!(period_after["version"].as_i64().unwrap() > period["version"].as_i64().unwrap());
    for cycle_id in [first, second, third] {
        let cycle = harness
            .rows("cycles")
            .await
            .into_iter()
            .find(|c| c["id"] == cycle_id)
            .unwrap();
        assert_eq!(cycle["vendor_period_id"], period["id"]);
        assert_eq!(cycle["vendor_id"], vendor["id"]);
    }
    let member_profile = harness
        .rows("profiles")
        .await
        .into_iter()
        .find(|p| p["account_id"] == 400)
        .unwrap();
    assert_eq!(member_profile["vendor_id"], vendor["id"]);
    let group_view = harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vendor["id"])
        .cloned()
        .unwrap();
    assert_eq!(group_view["current_period"]["id"], period["id"]);
    assert!(!group_view["waiting_renewal"].as_bool().unwrap());
    assert_eq!(group_view["current_members"].as_array().unwrap().len(), 3);

    assert_error(
        mutate(
            &harness,
            "group-add-stale-vendor",
            &format!("add-vendor-members:{}", vendor["id"].as_i64().unwrap()),
            &json!({"version":vendor["version"],"period_version":period_after["version"],"account_ids":[500]}),
        )
        .await,
        409,
        "变化",
    );
    assert!(
        harness
            .rows("profiles")
            .await
            .into_iter()
            .all(|p| p["account_id"] != 500)
    );
    assert_error(
        mutate(
            &harness,
            "group-add-stale-period",
            &format!("add-vendor-members:{}", vendor["id"].as_i64().unwrap()),
            &json!({"version":added["version"],"period_version":stale_period_version,"account_ids":[500]}),
        )
        .await,
        409,
        "变化",
    );
    assert!(
        harness
            .rows("profiles")
            .await
            .into_iter()
            .all(|p| p["account_id"] != 500)
    );
}

#[tokio::test]
async fn settlement_keeps_vendor_membership_and_renews_as_one_group() {
    let harness = Harness::new().await;
    let vendor = create_group(&harness, &[100, 200], "group-renew-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &period).await;
    let current = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":[first,second],"expense_ids":[],"end":END}),
        )
        .await
        .expect("preview group settlement");
    let partial = json!({
        "preview_id":preview["id"],
        "renewals":[],
        "delete_cycle_ids":[],
        "vendor_renewals":[{"period_id":period["id"],"version":current["version"],"cost":"6.00","creditor_id":2,"cycle_ids":[first]}]
    });
    let partial_prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &partial)
        .await
        .expect("prepare partial selection for settle rejection");
    assert_error(
        harness
            .app
            .service
            .mutate(
                &admin(),
                "group-partial-renewal",
                "settle",
                &partial,
                partial_prepared,
                None,
            )
            .await,
        409,
        "整组",
    );
    settle_group(
        &harness,
        &[first, second],
        json!([]),
        "group-settle-no-renewal",
    )
    .await
    .expect("settle group without renewal");
    for account_id in [100, 200] {
        let profile = harness
            .rows("profiles")
            .await
            .into_iter()
            .find(|p| p["account_id"] == account_id)
            .unwrap();
        assert_eq!(profile["vendor_id"], vendor["id"]);
    }
    let settled = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    assert_eq!(settled["status"], "settled");
    let vendor_view = harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vendor["id"])
        .cloned()
        .unwrap();
    assert!(vendor_view["current_period"].is_null());
    assert!(vendor_view["waiting_renewal"].as_bool().unwrap());
    assert_eq!(vendor_view["current_members"].as_array().unwrap().len(), 2);
    let renewed = mutate(
        &harness,
        "group-direct-renew",
        &format!("renew-vendor:{}", vendor["id"].as_i64().unwrap()),
        &json!({
            "version":vendor_view["version"],
            "period_version":settled["version"],
            "cost":"7.00",
            "creditor_id":2
        }),
    )
    .await
    .expect("renew whole vendor group");
    let next_id = renewed["id"].as_i64().unwrap();
    let next = vendor_period_row(&harness, next_id).await;
    assert_eq!(next["renewed_from_period_id"], period["id"]);
    assert_eq!(next["status"], "open");
    assert_eq!(next["start"], END.replace('+', ".000000+"));
    let renewed_view = harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vendor["id"])
        .cloned()
        .unwrap();
    assert_eq!(renewed_view["current_period"]["id"], next["id"]);
    assert!(!renewed_view["waiting_renewal"].as_bool().unwrap());
    let current_cycles = harness
        .rows("cycles")
        .await
        .into_iter()
        .filter(|c| c["vendor_period_id"] == next_id)
        .collect::<Vec<_>>();
    assert_eq!(current_cycles.len(), 2);
    assert!(
        current_cycles
            .iter()
            .all(|c| c["vendor_id"] == vendor["id"])
    );
    assert_error(
        mutate(
            &harness,
            "group-direct-renew-stale",
            &format!("renew-vendor:{}", vendor["id"].as_i64().unwrap()),
            &json!({"version":vendor_view["version"],"period_version":settled["version"],"cost":"7.00","creditor_id":2}),
        )
        .await,
        409,
        "变化",
    );
}

#[tokio::test]
async fn closed_group_append_keeps_old_members_and_renews_with_new_member() {
    let harness = Harness::new().await;
    let vendor = create_group(&harness, &[100, 200], "group-closed-append-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &period).await;
    settle_group(
        &harness,
        &[first, second],
        json!([]),
        "group-closed-append-settle",
    )
    .await
    .expect("settle group before closed append");

    let settled = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let before_append = harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vendor["id"])
        .cloned()
        .unwrap();
    let added = mutate(
        &harness,
        "group-closed-append",
        &format!("add-vendor-members:{}", vendor["id"].as_i64().unwrap()),
        &json!({"version":before_append["version"],"period_version":null,"account_ids":[400]}),
    )
    .await
    .expect("append member to settled group");
    let periods = harness.app.service.vendors(&admin()).await.unwrap()["vendors"][0]["periods"]
        .as_array()
        .unwrap()
        .clone();
    let old_view = periods
        .iter()
        .find(|p| p["id"] == period["id"])
        .expect("historical period remains visible");
    let old_accounts = old_view["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["account_id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(old_accounts, vec![100, 200]);
    assert!(!old_accounts.contains(&400));

    let renewed = mutate(
        &harness,
        "group-closed-append-renew",
        &format!("renew-vendor:{}", vendor["id"].as_i64().unwrap()),
        &json!({
            "version":added["version"],
            "period_version":settled["version"],
            "cost":"7.00",
            "creditor_id":2
        }),
    )
    .await
    .expect("renew settled group with appended member");
    let next_id = renewed["id"].as_i64().unwrap();
    let next_accounts = harness
        .rows("cycles")
        .await
        .into_iter()
        .filter(|cycle| cycle["vendor_period_id"] == next_id)
        .map(|cycle| cycle["account_id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(next_accounts, vec![100, 200, 400]);
}

#[tokio::test]
async fn failed_member_deletion_stays_excluded_from_later_group_renewal() {
    let harness = Harness::new().await;
    let vendor = create_group(&harness, &[100, 200], "group-delete-renew-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &period).await;
    let settled_result = settle_group_with_deletes(
        &harness,
        &[first, second],
        &[second],
        json!([]),
        "group-delete-renew-settle",
    )
    .await
    .expect("settle group with one member deletion");
    assert!(
        settled_result["deletions"]
            .as_array()
            .is_some_and(|jobs| !jobs.is_empty())
    );
    mark_deletion_failed(&harness, 200).await;
    let failed_job = {
        let mut db = harness.app.service.store.read().await.unwrap();
        let row = store::one(
            &mut db,
            "SELECT * FROM account_deletions WHERE account_id=?",
            &[json!(200)],
            "deletion job missing",
        )
        .await
        .unwrap();
        db.commit().await.unwrap();
        row
    };
    assert_eq!(failed_job["status"], "failed");

    let settled = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let current = harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vendor["id"])
        .cloned()
        .unwrap();
    let current_member = current["current_members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["account_id"] == 200)
        .expect("deleted member remains in current group view");
    assert_eq!(current_member["renewal_excluded"], 1);
    let renewed = mutate(
        &harness,
        "group-delete-renew",
        &format!("renew-vendor:{}", vendor["id"].as_i64().unwrap()),
        &json!({
            "version":current["version"],
            "period_version":settled["version"],
            "cost":"7.00",
            "creditor_id":2
        }),
    )
    .await
    .expect("renew remaining group members");
    let next_id = renewed["id"].as_i64().unwrap();
    let next_cycles = harness
        .rows("cycles")
        .await
        .into_iter()
        .filter(|cycle| cycle["vendor_period_id"] == next_id)
        .collect::<Vec<_>>();
    assert_eq!(next_cycles.len(), 1);
    assert_eq!(next_cycles[0]["account_id"], 100);
}

#[tokio::test]
async fn cancelled_locked_cycle_blocks_an_earlier_reconfiguration_start() {
    let harness = Harness::new().await;
    let locked = insert_cancelled_locked_cycle(&harness, 400, END).await;
    assert_eq!(
        harness
            .rows("cycles")
            .await
            .into_iter()
            .find(|cycle| cycle["id"] == locked)
            .unwrap()["status"],
        "cancelled"
    );
    assert_error(
        mutate(
            &harness,
            "locked-start-group-boundary",
            "create",
            &json!({
                "account_id":400,
                "name":"Earlier replacement",
                "cost":"3.00",
                "creditor_id":2,
                "start":START,
                "start_mode":"exact",
                "cost_kind":"exact"
            }),
        )
        .await,
        409,
        "不能早于已锁定的升级付费起点",
    );
}

#[tokio::test]
async fn independent_create_and_free_cannot_bypass_group_membership() {
    let harness = Harness::new().await;
    let vendor = create_group(&harness, &[100, 200], "group-independent-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &period).await;
    settle_group(
        &harness,
        &[first, second],
        json!([]),
        "group-independent-settle",
    )
    .await
    .expect("settle group before independent entry checks");
    let profile = harness
        .rows("profiles")
        .await
        .into_iter()
        .find(|p| p["account_id"] == 100)
        .unwrap();
    assert_error(
        mutate(
            &harness,
            "group-independent-create-reject",
            "create",
            &json!({"account_id":100,"name":"Bypass","cost":"1.00","creditor_id":2,"start":END,"start_mode":"exact","cost_kind":"exact"}),
        )
        .await,
        409,
        "账号组",
    );
    assert_error(
        mutate(
            &harness,
            "group-independent-free-reject",
            "free:100",
            &json!({"name":"Bypass free","version":profile["version"]}),
        )
        .await,
        409,
        "账号组",
    );
}

#[tokio::test]
async fn definition_only_vendor_configures_first_period_and_archive_releases_profiles() {
    let harness = Harness::new().await;
    let vendor = mutate(
        &harness,
        "definition-only-create",
        "create-vendor",
        &json!({"name":"Definition only","cost":"6.00","creditor_id":2}),
    )
    .await
    .expect("create definition-only vendor");
    assert!(vendor.get("period").is_none());
    assert_error(
        mutate(
            &harness,
            "definition-only-empty-append",
            &format!("add-vendor-members:{}", vendor["id"].as_i64().unwrap()),
            &json!({"version":vendor["version"],"period_version":null,"account_ids":[100]}),
        )
        .await,
        409,
        "首期",
    );
    let history_period = mutate(
        &harness,
        "definition-only-first-period",
        &format!("create-vendor-period:{}", vendor["id"].as_i64().unwrap()),
        &json!({
            "version":vendor["version"],
            "cost":"6.00",
            "creditor_id":2,
            "start":START,
            "start_mode":"exact",
            "account_ids":[100,200]
        }),
    )
    .await
    .expect("create first period for definition-only vendor");
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &history_period).await;
    settle_group(
        &harness,
        &[first, second],
        json!([]),
        "definition-only-history-settle",
    )
    .await
    .expect("settle history before null period append");
    let current_vendor = harness.app.service.vendors(&admin()).await.unwrap()["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vendor["id"])
        .cloned()
        .unwrap();
    let added = mutate(
        &harness,
        "definition-only-append",
        &format!("add-vendor-members:{}", vendor["id"].as_i64().unwrap()),
        &json!({"version":current_vendor["version"],"period_version":null,"account_ids":[400]}),
    )
    .await
    .expect("append member while waiting for renewal");
    assert_eq!(added["id"], vendor["id"]);
    assert!(
        harness
            .rows("cycles")
            .await
            .iter()
            .all(|cycle| cycle["account_id"] != 400)
    );
    assert_eq!(harness.rows("profiles").await.len(), 3);
    mutate(
        &harness,
        "definition-only-archive",
        &format!("archive-vendor:{}", vendor["id"].as_i64().unwrap()),
        &json!({"version":added["version"]}),
    )
    .await
    .expect("archive definition-only vendor");
    assert!(harness.rows("profiles").await.is_empty());
    let view = harness.app.service.vendors(&admin()).await.unwrap();
    let archived = view["vendors"].as_array().unwrap().first().unwrap();
    assert!(archived["archived_at"].is_string());
    assert!(archived["current_members"].as_array().unwrap().is_empty());
}
