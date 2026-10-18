#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use settlement::{
    billing::UsageRow,
    error::Result,
    service::{Prepared, vendor_billing::vendor_allocations},
    store,
};
use support::legacy::{END, Harness, START, admin, payer};

fn usage(id: i64, root_id: i64, user_id: i64, amount: &str) -> UsageRow {
    UsageRow {
        account_id: root_id,
        amount: amount.to_owned(),
        created_at: format!("2025-01-{:02}T00:00:00.000000+00:00", id),
        id,
        root_id,
        user_id,
    }
}

fn member(id: i64, account_id: i64, name: &str) -> Value {
    json!({"id":id,"account_id":account_id,"current_name":name,"name":name})
}

fn amount(accounts: &Value, account_id: i64) -> i64 {
    accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["account_id"] == account_id)
        .and_then(|account| account["amount_fen"].as_i64())
        .unwrap_or_else(|| panic!("account {account_id} has no allocated amount"))
}

fn allocation(accounts: &Value, account_id: i64, user_id: i64) -> i64 {
    accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["account_id"] == account_id)
        .unwrap()["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|share| share["user_id"] == user_id)
        .and_then(|share| share["amount_fen"].as_i64())
        .unwrap_or_else(|| panic!("account {account_id} has no user {user_id} allocation"))
}

fn assert_error<T>(result: Result<T>, status: u16, message: &str) {
    let error = match result {
        Ok(_) => panic!("operation should fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, status, "unexpected error: {error}");
    assert!(
        error.message.contains(message),
        "expected error containing {message:?}, got {:?}",
        error.message
    );
}

async fn mutate(harness: &Harness, key: &str, action: &str, payload: &Value) -> Result<Value> {
    harness
        .app
        .service
        .mutate(&admin(), key, action, payload, Prepared::default(), None)
        .await
}

async fn cycle_row(harness: &Harness, id: i64) -> Value {
    harness
        .rows("cycles")
        .await
        .into_iter()
        .find(|cycle| cycle["id"] == id)
        .unwrap_or_else(|| panic!("cycle {id} not found"))
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

async fn create_cycle(harness: &Harness, account_id: i64, start: &str, key: &str) -> i64 {
    let result = mutate(
        harness,
        key,
        "create",
        &json!({
            "account_id":account_id,
            "name":format!("Fixture vendor {account_id}"),
            "cost":"1.00",
            "creditor_id":2,
            "start":start,
            "start_mode":"exact",
            "cost_kind":"exact"
        }),
    )
    .await
    .expect("create vendor member cycle");
    result["id"].as_i64().unwrap()
}

async fn create_vendor(harness: &Harness, key: &str) -> Value {
    mutate(
        harness,
        key,
        "create-vendor",
        &json!({"name":"Fixture Vendor","cost":"6.00","creditor_id":2}),
    )
    .await
    .expect("create vendor")
}

async fn create_period(
    harness: &Harness,
    vendor: &Value,
    cost: &str,
    cycles: &[i64],
    key: &str,
) -> Result<Value> {
    let members = futures_util::future::join_all(cycles.iter().map(|id| async move {
        let cycle = cycle_row(harness, *id).await;
        json!({"cycle_id":id,"version":cycle["version"]})
    }))
    .await;
    mutate(
        harness,
        key,
        &format!("create-vendor-period:{}", vendor["id"].as_i64().unwrap()),
        &json!({
            "version":vendor["version"],
            "cost":cost,
            "creditor_id":2,
            "start":START,
            "start_mode":"exact",
            "members":members,
            "detached":[]
        }),
    )
    .await
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

async fn confirm_vendor_bill(
    harness: &Harness,
    period: &Value,
    csv: String,
    accept_difference: bool,
) -> Result<Value> {
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
                "csv":csv
            }),
        )
        .await?;
    let prepared = harness
        .app
        .service
        .prepare_vendor_bill(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"preview_id":preview["id"]}),
        )
        .await?;
    mutate_with_prepared(
        harness,
        &format!("vendor-bill-confirm-key-{}", period["id"].as_i64().unwrap()),
        &format!("vendor-bill-confirm:{}", period["id"].as_i64().unwrap()),
        &json!({
            "preview_id":preview["id"],
            "version":period["version"],
            "accept_difference":accept_difference,
            "accept_outside":false
        }),
        Prepared {
            vendor_bill: Some(prepared),
            ..Prepared::default()
        },
    )
    .await
}

async fn mutate_with_prepared(
    harness: &Harness,
    key: &str,
    action: &str,
    payload: &Value,
    prepared: Prepared,
) -> Result<Value> {
    harness
        .app
        .service
        .mutate(&admin(), key, action, payload, prepared, None)
        .await
}

#[test]
fn vendor_allocations_use_account_then_user_largest_remainder() {
    let members = vec![member(20, 200, "B"), member(10, 100, "A")];
    let records = vec![
        usage(1, 100, 2, "1"),
        usage(2, 100, 3, "2"),
        usage(3, 200, 4, "1"),
        usage(4, 200, 5, "1"),
    ];
    let split = vendor_allocations(101, 9, &members, &records, &json!([])).unwrap();
    assert_eq!(split["basis"], "local_consumption");
    assert_eq!(amount(&split["accounts"], 100), 61);
    assert_eq!(amount(&split["accounts"], 200), 40);
    assert_eq!(allocation(&split["accounts"], 100, 2), 20);
    assert_eq!(allocation(&split["accounts"], 100, 3), 41);
    assert_eq!(allocation(&split["accounts"], 200, 4), 20);
    assert_eq!(allocation(&split["accounts"], 200, 5), 20);

    let reversed = vendor_allocations(
        101,
        9,
        &[members[1].clone(), members[0].clone()],
        &[
            usage(4, 200, 5, "1"),
            usage(2, 100, 3, "2"),
            usage(3, 200, 4, "1"),
            usage(1, 100, 2, "1"),
        ],
        &json!([]),
    )
    .unwrap();
    assert_eq!(reversed["basis"], "local_consumption");
    assert_eq!(amount(&reversed["accounts"], 100), 61);
    assert_eq!(amount(&reversed["accounts"], 200), 40);
    assert_eq!(allocation(&reversed["accounts"], 100, 2), 20);
    assert_eq!(allocation(&reversed["accounts"], 100, 3), 41);
}

#[test]
fn vendor_allocations_use_matched_upstream_and_reject_zero_basis() {
    let members = vec![member(10, 100, "A"), member(20, 200, "B")];
    let records = vec![usage(1, 100, 3, "100"), usage(2, 200, 4, "100")];
    let matched = json!([
        {"usage_id":1,"user_id":3,"amount":"7"},
        {"usage_id":2,"user_id":4,"amount":"3"}
    ]);
    let split = vendor_allocations(100, 2, &members, &records, &matched).unwrap();
    assert_eq!(split["basis"], "matched_upstream");
    assert_eq!(amount(&split["accounts"], 100), 70);
    assert_eq!(amount(&split["accounts"], 200), 30);

    let zero = vendor_allocations(
        100,
        2,
        &members,
        &[usage(1, 100, 3, "0"), usage(2, 200, 4, "0")],
        &json!([]),
    )
    .unwrap();
    assert_eq!(zero["basis"], "local_consumption");
    assert_eq!(zero["can_confirm"], false);
    assert!(
        zero["accounts"].as_array().unwrap().iter().all(|a| {
            a["amount_fen"].is_null() && a["allocations"].as_array().unwrap().is_empty()
        })
    );
}

#[tokio::test]
async fn vendor_group_requires_one_start_and_detached_member_cost_and_version() {
    let harness = Harness::new().await;
    let first = create_cycle(&harness, 100, START, "vendor-uniform-first").await;
    let second = create_cycle(&harness, 200, END, "vendor-uniform-second").await;
    let vendor = create_vendor(&harness, "vendor-uniform").await;
    assert_error(
        create_period(
            &harness,
            &vendor,
            "6.00",
            &[first, second],
            "vendor-uniform-fail",
        )
        .await,
        409,
        "成员起点不一致",
    );

    let third = create_cycle(&harness, 300, START, "vendor-edit-third").await;
    let period = create_period(
        &harness,
        &vendor,
        "6.00",
        &[first, third],
        "vendor-edit-create",
    )
    .await
    .expect("create uniform vendor period");
    let period_id = period["id"].as_i64().unwrap();
    let first_row = cycle_row(&harness, first).await;
    let stale = json!({
        "version":period["version"],
        "cost":"6.00",
        "creditor_id":2,
        "start":START,
        "start_mode":"exact",
        "members":[{"cycle_id":first,"version":first_row["version"]}],
        "detached":[]
    });
    assert_error(
        mutate(
            &harness,
            "vendor-edit-missing-detached",
            &format!("edit-vendor-period:{period_id}"),
            &stale,
        )
        .await,
        422,
        "移出成员必须填写独立成本",
    );

    let third_row = cycle_row(&harness, third).await;
    let edited = mutate(
        &harness,
        "vendor-edit-detached",
        &format!("edit-vendor-period:{period_id}"),
        &json!({
            "version":period["version"],
            "cost":"6.00",
            "creditor_id":2,
            "start":START,
            "start_mode":"exact",
            "members":[{"cycle_id":first,"version":first_row["version"]}],
            "detached":[{"cycle_id":third,"version":third_row["version"],"cost":"2.50","creditor_id":3}]
        }),
    )
    .await
    .expect("detach member with independent cost");
    assert_eq!(edited["id"], period_id);
    let detached = cycle_row(&harness, third).await;
    assert!(detached["vendor_period_id"].is_null());
    assert_eq!(detached["cost_fen"], 250);
    assert_eq!(detached["creditor_id"], 3);
    assert_eq!(vendor_period_row(&harness, period_id).await["version"], 2);
}

#[tokio::test]
async fn vendor_group_blocks_old_account_paths_and_redacts_bill_allocation() {
    let harness = Harness::new().await;
    let first = create_cycle(&harness, 100, START, "vendor-bound-first").await;
    let second = create_cycle(&harness, 200, START, "vendor-bound-second").await;
    let old_preview = harness
        .app
        .service
        .preview(&admin(), &json!({"ids":[first],"expense_ids":[],"end":END}))
        .await
        .expect("create old account preview");
    let vendor = create_vendor(&harness, "vendor-bound-vendor").await;
    let period = create_period(
        &harness,
        &vendor,
        "6.00",
        &[first, second],
        "vendor-bound-period",
    )
    .await
    .expect("create vendor period");
    let first_row = cycle_row(&harness, first).await;
    assert_error(
        mutate(
            &harness,
            "vendor-bound-edit",
            &format!("edit:{first}"),
            &json!({
                "account_id":100,
                "name":"changed",
                "cost":"3.00",
                "creditor_id":2,
                "start":START,
                "start_mode":"exact",
                "cost_kind":"exact",
                "version":first_row["version"]
            }),
        )
        .await,
        409,
        "属于供应商周期",
    );
    assert_error(
        mutate(
            &harness,
            "vendor-bound-start",
            &format!("start-confirm:{first}"),
            &json!({"version":first_row["version"],"start":START,"method":"legacy","source_usage_id":null}),
        )
        .await,
        409,
        "属于供应商周期",
    );
    assert_error(
        harness
            .app
            .service
            .bill_preview(
                &admin(),
                first,
                &json!({"version":first_row["version"],"end":END,"allocation_mode":"proportional","file_name":"x.csv","csv":bill_csv()}),
            )
            .await,
        409,
        "属于供应商周期",
    );
    assert_error(
        harness
            .app
            .service
            .preview(&admin(), &json!({"ids":[first],"expense_ids":[],"end":END}))
            .await,
        409,
        "必须包含全部成员",
    );
    assert_error(
        harness
            .app
            .service
            .prepare_settlement(
                &admin(),
                &json!({"preview_id":old_preview["id"],"renewals":[],"delete_cycle_ids":[]}),
            )
            .await,
        404,
        "结算预览不存在",
    );

    confirm_vendor_bill(&harness, &period, bill_csv(), false)
        .await
        .expect("confirm vendor bill");
    let detail = harness
        .app
        .service
        .cycle_detail(&payer(), first)
        .await
        .expect("regular user cycle detail");
    assert!(detail.get("vendor_basis").is_none());
    assert!(detail["bill"].get("matched").is_none());
    assert!(
        detail["allocations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|share| share["user_id"] == payer()["id"])
    );
}

#[tokio::test]
async fn vendor_bill_difference_requires_explicit_confirmation_and_keeps_c_source() {
    let harness = Harness::new().await;
    let first = create_cycle(&harness, 100, START, "vendor-difference-first").await;
    let second = create_cycle(&harness, 200, START, "vendor-difference-second").await;
    let vendor = create_vendor(&harness, "vendor-difference-vendor").await;
    let period = create_period(
        &harness,
        &vendor,
        "5.00",
        &[first, second],
        "vendor-difference-period",
    )
    .await
    .expect("create difference vendor period");
    let preview = harness
        .app
        .service
        .vendor_bill_preview(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"version":period["version"],"end":END,"allocation_mode":"proportional","file_name":"difference.csv","csv":bill_csv()}),
        )
        .await
        .expect("preview difference bill");
    assert_eq!(preview["difference_required"], true);
    let prepared = harness
        .app
        .service
        .prepare_vendor_bill(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"preview_id":preview["id"]}),
        )
        .await
        .expect("prepare difference bill");
    let confirm_payload = json!({
        "preview_id":preview["id"],
        "version":period["version"],
        "accept_difference":false,
        "accept_outside":false
    });
    assert_error(
        mutate_with_prepared(
            &harness,
            "vendor-difference-reject",
            &format!("vendor-bill-confirm:{}", period["id"].as_i64().unwrap()),
            &confirm_payload,
            Prepared {
                vendor_bill: Some(prepared.clone()),
                ..Prepared::default()
            },
        )
        .await,
        422,
        "请确认账单总额",
    );
    let accepted = mutate_with_prepared(
        &harness,
        "vendor-difference-accept",
        &format!("vendor-bill-confirm:{}", period["id"].as_i64().unwrap()),
        &json!({
            "preview_id":preview["id"],
            "version":period["version"],
            "accept_difference":true,
            "accept_outside":false
        }),
        Prepared {
            vendor_bill: Some(prepared),
            ..Prepared::default()
        },
    )
    .await
    .expect("accept difference bill");
    assert_eq!(accepted["id"], period["id"]);
    let saved_period = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    assert_eq!(saved_period["bill_difference_confirmed"], 1);
    assert_eq!(saved_period["bill_end"], "2025-02-01T00:00:00.000000+00:00");
    assert_eq!(saved_period["bill_import_id"], accepted["import_id"]);
    for id in [first, second] {
        let row = cycle_row(&harness, id).await;
        assert_eq!(row["cost_source"], "vendor_bill");
        assert_eq!(row["bill_end"], "2025-02-01T00:00:00.000000+00:00");
        assert!(row["bill_import_id"].is_null());
    }
    let first_cost = cycle_row(&harness, first).await["cost_fen"]
        .as_i64()
        .unwrap();
    let second_cost = cycle_row(&harness, second).await["cost_fen"]
        .as_i64()
        .unwrap();
    assert_eq!(first_cost + second_cost, 500);
    assert!(first_cost > 0 && second_cost > 0);
}

async fn settle_group(
    harness: &Harness,
    period_id: i64,
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
    let _ = period_id;
    mutate_with_prepared(harness, key, "settle", &payload, prepared).await
}

#[tokio::test]
async fn vendor_group_settles_as_one_renews_and_voids_only_as_a_group() {
    let harness = Harness::new().await;
    let first = create_cycle(&harness, 100, START, "vendor-settle-first").await;
    let second = create_cycle(&harness, 200, START, "vendor-settle-second").await;
    let vendor = create_vendor(&harness, "vendor-settle-vendor").await;
    let period = create_period(
        &harness,
        &vendor,
        "6.00",
        &[first, second],
        "vendor-settle-period",
    )
    .await
    .expect("create settlement vendor period");
    confirm_vendor_bill(&harness, &period, bill_csv(), false)
        .await
        .expect("confirm settlement bill");
    let current = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let settled = settle_group(
        &harness,
        period["id"].as_i64().unwrap(),
        &[first, second],
        json!([{
            "period_id":period["id"],
            "version":current["version"],
            "cost":"6.00",
            "creditor_id":2,
            "cycle_ids":[first,second]
        }]),
        "vendor-settle-renew",
    )
    .await
    .expect("settle and renew vendor group");
    assert_eq!(cycle_row(&harness, first).await["status"], "settled");
    assert_eq!(cycle_row(&harness, second).await["status"], "settled");
    assert_eq!(
        vendor_period_row(&harness, period["id"].as_i64().unwrap()).await["status"],
        "settled"
    );
    let next_id = settled["vendor_renewals"][0]["id"].as_i64().unwrap();
    let next = vendor_period_row(&harness, next_id).await;
    assert_eq!(next["renewed_from_period_id"], period["id"]);
    assert_eq!(next["status"], "open");

    let first_settlement = cycle_row(&harness, first).await["settlement_id"].clone();
    assert_error(
        mutate(
            &harness,
            "vendor-settle-individual-void",
            &format!("void:{}", first_settlement.as_i64().unwrap()),
            &json!({}),
        )
        .await,
        409,
        "必须整组作废",
    );
    mutate(
        &harness,
        "vendor-settle-group-void",
        &format!("void-vendor:{}", period["id"].as_i64().unwrap()),
        &json!({"version":vendor_period_row(&harness, period["id"].as_i64().unwrap()).await["version"]}),
    )
    .await
    .expect("void group settlement");
    assert_eq!(cycle_row(&harness, first).await["status"], "open");
    assert_eq!(cycle_row(&harness, second).await["status"], "open");
    assert_eq!(
        vendor_period_row(&harness, period["id"].as_i64().unwrap()).await["status"],
        "open"
    );
}

#[tokio::test]
async fn vendor_group_payment_blocks_group_void() {
    let harness = Harness::new().await;
    let first = create_cycle(&harness, 100, START, "vendor-paid-first").await;
    let second = create_cycle(&harness, 200, START, "vendor-paid-second").await;
    let vendor = create_vendor(&harness, "vendor-paid-vendor").await;
    let period = create_period(
        &harness,
        &vendor,
        "6.00",
        &[first, second],
        "vendor-paid-period",
    )
    .await
    .expect("create paid vendor period");
    confirm_vendor_bill(&harness, &period, bill_csv(), false)
        .await
        .expect("confirm paid vendor bill");
    settle_group(
        &harness,
        period["id"].as_i64().unwrap(),
        &[first, second],
        json!([]),
        "vendor-paid-settle",
    )
    .await
    .expect("settle paid vendor group");
    let order = harness
        .rows("orders")
        .await
        .into_iter()
        .find(|order| order["cycle_id"] == first || order["cycle_id"] == second)
        .expect("group settlement creates an order");
    let paid = mutate(
        &harness,
        "vendor-paid-submit",
        &format!("transition:{}", order["id"].as_i64().unwrap()),
        &json!({"action":"confirm","version":order["version"]}),
    )
    .await;
    assert_error(paid, 409, "付款方式已升级");

    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query("UPDATE orders SET status='submitted' WHERE id=?")
        .bind(order["id"].as_i64().unwrap())
        .execute(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    mutate(
        &harness,
        "vendor-paid-confirm",
        &format!("transition:{}", order["id"].as_i64().unwrap()),
        &json!({"action":"confirm","version":order["version"]}),
    )
    .await
    .expect("mark group order paid");
    let period_row = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    assert_error(
        mutate(
            &harness,
            "vendor-paid-group-void",
            &format!("void-vendor:{}", period["id"].as_i64().unwrap()),
            &json!({"version":period_row["version"]}),
        )
        .await,
        409,
        "已有报已付或结清订单",
    );
}

#[tokio::test]
async fn deleted_vendor_member_cannot_be_removed_from_open_group() {
    let harness = Harness::new().await;
    let first = create_cycle(&harness, 100, START, "vendor-deleted-first").await;
    let second = create_cycle(&harness, 200, START, "vendor-deleted-second").await;
    let vendor = create_vendor(&harness, "vendor-deleted-vendor").await;
    let period = create_period(
        &harness,
        &vendor,
        "6.00",
        &[first, second],
        "vendor-deleted-period",
    )
    .await
    .expect("create deletion vendor period");
    let mut source = harness.source();
    source["accounts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|account| account["id"] == 200)
        .unwrap()["deleted"] = json!(true);
    harness.write_source(&source);
    harness
        .app
        .service
        .sync(true)
        .await
        .expect("sync deleted member");

    let period_id = period["id"].as_i64().unwrap();
    let first_row = cycle_row(&harness, first).await;
    assert_error(
        mutate(
            &harness,
            "vendor-deleted-remove",
            &format!("edit-vendor-period:{period_id}"),
            &json!({
                "version":period["version"],
                "cost":"6.00",
                "creditor_id":2,
                "start":START,
                "start_mode":"exact",
                "members":[{"cycle_id":first,"version":first_row["version"]}],
                "detached":[]
            }),
        )
        .await,
        409,
        "已删除成员仍须保留",
    );
    let members = harness.app.service.vendors(&admin()).await.unwrap()["vendors"][0]["periods"][0]
        ["members"]
        .as_array()
        .unwrap()
        .clone();
    assert!(members.iter().any(|m| m["id"] == second));
}
