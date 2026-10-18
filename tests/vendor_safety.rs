#[allow(dead_code)]
mod support;

use axum::http::StatusCode;
use reqwest::Method;
use serde_json::{Value, json};
use settlement::{error::Result, maintenance, service::Prepared, store};
use std::{net::SocketAddr, path::PathBuf};
use support::legacy::{END, Harness, START, admin};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

struct RouteHarness {
    _temporary: TempDir,
    _app: settlement::http::App,
    address: SocketAddr,
    task: JoinHandle<()>,
    client: reqwest::Client,
}

impl RouteHarness {
    async fn new() -> Self {
        let temporary = TempDir::new().expect("isolated route test directory");
        let fixture_path = temporary.path().join("source.json");
        let fixture = support::legacy::fixture_data();
        std::fs::write(&fixture_path, serde_json::to_vec(&fixture).unwrap())
            .expect("write route test source fixture");
        let app = settlement::http::App::new(support::legacy::config(
            temporary.path(),
            PathBuf::from(&fixture_path),
        ))
        .await
        .expect("isolated route test app");
        app.service
            .sync(true)
            .await
            .expect("initial route test sync");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = app
            .router()
            .into_make_service_with_connect_info::<SocketAddr>();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            _temporary: temporary,
            _app: app,
            address,
            task,
            client,
        }
    }

    async fn request(&self, method: Method, path: &str) -> StatusCode {
        self.client
            .request(
                method,
                format!("http://{}/settlement/api/v1/{path}", self.address),
            )
            .send()
            .await
            .expect("route request")
            .status()
    }
}

impl Drop for RouteHarness {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mutate(harness: &Harness, key: &str, action: &str, payload: &Value) -> Result<Value> {
    harness
        .app
        .service
        .mutate(&admin(), key, action, payload, Prepared::default(), None)
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

async fn create_vendor(harness: &Harness, account_ids: &[i64], cost: &str, key: &str) -> Value {
    mutate(
        harness,
        key,
        "create-vendor",
        &json!({
            "name":"Fixture Vendor",
            "cost":cost,
            "creditor_id":2,
            "account_ids":account_ids,
            "start":START,
            "start_mode":"exact"
        }),
    )
    .await
    .expect("create vendor group")
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

async fn settle_group(
    harness: &Harness,
    cycle_ids: &[i64],
    vendor_renewals: Value,
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
        "vendor_renewals":vendor_renewals
    });
    let prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &payload)
        .await?;
    mutate_with_prepared(harness, key, "settle", &payload, prepared).await
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
async fn vendor_routes_are_connected_before_authentication() {
    let server = RouteHarness::new().await;
    let routes = [
        (Method::GET, Method::PATCH, "vendors"),
        (Method::POST, Method::DELETE, "vendors"),
        (Method::PATCH, Method::GET, "vendors/1"),
        (Method::POST, Method::GET, "vendors/1/archive"),
        (Method::POST, Method::GET, "vendors/1/members"),
        (Method::POST, Method::GET, "vendors/1/renew"),
        (Method::POST, Method::GET, "vendors/1/periods"),
        (Method::PATCH, Method::GET, "vendor-periods/1"),
        (Method::POST, Method::GET, "vendor-periods/1/start-confirm"),
        (Method::POST, Method::GET, "vendor-periods/1/bill-previews"),
        (Method::POST, Method::GET, "vendor-periods/1/bill-confirm"),
        (Method::POST, Method::GET, "vendor-periods/1/void"),
        (
            Method::GET,
            Method::POST,
            "vendor-periods/1/bill-previews/fixture-preview/unmatched.csv",
        ),
    ];
    for (method, wrong_method, path) in routes {
        assert_eq!(
            server.request(method.clone(), path).await,
            StatusCode::UNAUTHORIZED,
            "expected {method} {path} to reach authentication instead of 404"
        );
        assert_eq!(
            server.request(wrong_method.clone(), path).await,
            StatusCode::METHOD_NOT_ALLOWED,
            "expected {wrong_method} {path} to be rejected as an unsupported method"
        );
    }
}

#[tokio::test]
async fn vendor_bill_keeps_sub_fen_difference_visible() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "vendor-micro-create").await;
    let period = period_from_vendor(&vendor);
    let csv = concat!(
        "Time,Billed Cost,Model,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens,Request ID\n",
        "2025-01-01T00:00:00+00:00,1.000000,fixture-model,10,20,0,0,fixture-1\n",
        "2025-01-10T00:00:00+00:00,3.000000,fixture-model,10,20,0,0,fixture-2\n",
        "2025-01-05T00:00:00+00:00,2.000001,fixture-model,10,20,0,0,fixture-6\n"
    );
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
                "file_name":"micro.csv",
                "csv":csv
            }),
        )
        .await
        .expect("preview sub-fen difference");
    assert_eq!(preview["unmatched_count"], 0);
    assert_eq!(preview["outside_count"], 0);
    assert_eq!(preview["difference_required"], true);
    assert_eq!(preview["difference_amount"], "-0.000001");
}

#[tokio::test]
async fn vendor_bill_preview_rejects_cost_and_member_drift_without_import() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "vendor-drift-cost-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;

    let preview = harness
        .app
        .service
        .vendor_bill_preview(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"version":period["version"],"end":END,"allocation_mode":"proportional","file_name":"drift.csv","csv":bill_csv()}),
        )
        .await
        .expect("preview vendor bill");
    let prepared = harness
        .app
        .service
        .prepare_vendor_bill(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"preview_id":preview["id"]}),
        )
        .await
        .expect("prepare vendor bill");

    let current = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let first_row = cycle_row(&harness, first).await;
    let second_row = cycle_row(&harness, second).await;
    mutate(
        &harness,
        "vendor-drift-cost-edit",
        &format!("edit-vendor-period:{}", period["id"].as_i64().unwrap()),
        &json!({
            "version":current["version"],
            "cost":"7.00",
            "creditor_id":2,
            "start":START,
            "start_mode":"exact",
            "members":[
                {"cycle_id":first,"version":first_row["version"]},
                {"cycle_id":second,"version":second_row["version"]}
            ],
            "detached":[]
        }),
    )
    .await
    .expect("change vendor cost");

    assert_error(
        mutate_with_prepared(
            &harness,
            "vendor-drift-cost-confirm",
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
        )
        .await,
        404,
        "记录不存在",
    );
    assert!(
        harness.rows("bill_imports").await.is_empty(),
        "a stale bill preview must not create an import"
    );

    let current = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let preview = harness
        .app
        .service
        .vendor_bill_preview(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"version":current["version"],"end":END,"allocation_mode":"proportional","file_name":"member-drift.csv","csv":bill_csv()}),
        )
        .await
        .expect("preview before member drift");
    let prepared = harness
        .app
        .service
        .prepare_vendor_bill(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"preview_id":preview["id"]}),
        )
        .await
        .expect("prepare before member drift");
    let vendor_current = harness
        .app
        .service
        .vendors(&admin())
        .await
        .expect("read current vendor")["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vendor["id"])
        .cloned()
        .expect("current vendor row");
    mutate(
        &harness,
        "vendor-drift-member-edit",
        &format!("add-vendor-members:{}", vendor["id"].as_i64().unwrap()),
        &json!({
            "version":vendor_current["version"],
            "period_version":current["version"],
            "account_ids":[400]
        }),
    )
    .await
    .expect("append unconfigured vendor member");
    assert_error(
        mutate_with_prepared(
            &harness,
            "vendor-drift-member-confirm",
            &format!("vendor-bill-confirm:{}", period["id"].as_i64().unwrap()),
            &json!({
                "preview_id":preview["id"],
                "version":current["version"],
                "accept_difference":false,
                "accept_outside":false
            }),
            Prepared {
                vendor_bill: Some(prepared),
                ..Prepared::default()
            },
        )
        .await,
        404,
        "记录不存在",
    );
    assert!(harness.rows("bill_imports").await.is_empty());
}

#[tokio::test]
async fn vendor_settlement_rejects_usage_revision_without_partial_orders() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "vendor-revision-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &period, bill_csv(), false)
        .await
        .expect("confirm vendor bill");

    let preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":[first,second],"expense_ids":[],"end":END}),
        )
        .await
        .expect("preview group settlement");
    let payload = json!({"preview_id":preview["id"],"renewals":[],"delete_cycle_ids":[],"vendor_renewals":[]});
    let prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &payload)
        .await
        .expect("prepare group settlement");

    let mut source = harness.source();
    source["usage"].as_array_mut().unwrap().push(json!({
        "id":10,
        "account_id":100,
        "user_id":3,
        "amount":"1",
        "created_at":"2025-01-20T00:00:00+00:00",
        "request_id":"fixture-10",
        "model":"fixture-model",
        "input_tokens":10,
        "output_tokens":20,
        "cache_read_tokens":0,
        "cache_creation_tokens":0
    }));
    harness.write_source(&source);
    harness
        .app
        .service
        .sync(true)
        .await
        .expect("sync changed usage revision");

    assert_error(
        mutate_with_prepared(
            &harness,
            "vendor-revision-settle",
            "settle",
            &payload,
            prepared,
        )
        .await,
        409,
        "消费记录已更新",
    );
    assert!(
        harness.rows("settlements").await.is_empty(),
        "the failed group settlement must not leave one member settled"
    );
    assert!(
        harness.rows("orders").await.is_empty(),
        "the failed group settlement must not leave half a group's orders"
    );
    assert_eq!(cycle_row(&harness, first).await["status"], "open");
    assert_eq!(cycle_row(&harness, second).await["status"], "open");
}

#[tokio::test]
async fn vendor_renewal_preserves_old_end_and_rejects_old_group_retry() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "vendor-renew-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &period, bill_csv(), false)
        .await
        .expect("confirm vendor bill");
    let current = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let settled = settle_group(
        &harness,
        &[first, second],
        json!([{
            "period_id":period["id"],
            "version":current["version"],
            "cost":"6.00",
            "creditor_id":2
        }]),
        "vendor-renew-settle",
    )
    .await
    .expect("settle and renew vendor group");
    let old_id = period["id"].as_i64().unwrap();
    let next_id = settled["vendor_renewals"][0]["id"].as_i64().unwrap();
    let old = vendor_period_row(&harness, old_id).await;
    let next = vendor_period_row(&harness, next_id).await;
    assert_eq!(old["status"], "settled");
    assert_eq!(old["end"], END.replace('+', ".000000+"));
    assert_eq!(next["status"], "open");
    assert_eq!(next["end"], Value::Null);
    assert_eq!(next["renewed_from_period_id"], old_id);

    let before_periods =
        harness.app.service.vendors(&admin()).await.unwrap()["vendors"][0]["periods"]
            .as_array()
            .unwrap()
            .len();
    assert_error(
        settle_group(
            &harness,
            &[first, second],
            json!([{
                "period_id":old_id,
                "version":old["version"],
                "cost":"6.00",
                "creditor_id":2
            }]),
            "vendor-renew-old-retry",
        )
        .await,
        409,
        "周期",
    );
    let after_periods =
        harness.app.service.vendors(&admin()).await.unwrap()["vendors"][0]["periods"]
            .as_array()
            .unwrap()
            .len();
    assert_eq!(after_periods, before_periods);

    mutate(
        &harness,
        "vendor-renew-old-void",
        &format!("void-vendor:{old_id}"),
        &json!({"version":old["version"]}),
    )
    .await
    .expect("void old vendor group");
    let old_after_void = vendor_period_row(&harness, old_id).await;
    let next_after_void = vendor_period_row(&harness, next_id).await;
    assert_eq!(old_after_void["status"], "open");
    assert_eq!(old_after_void["end"], END.replace('+', ".000000+"));
    assert_eq!(next_after_void["status"], "open");
    assert_eq!(next_after_void["end"], Value::Null);
}

#[tokio::test]
async fn vendor_unmatched_export_is_formula_safe_and_expires() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "vendor-export-create").await;
    let period = period_from_vendor(&vendor);
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
                "file_name":"formula.csv",
                "csv":format!("{}2025-01-15T00:00:00+00:00,0.01,fixture-model,0,0,0,0,\"=SUM(1,2)\"\n2025-02-02T00:00:00+00:00,0.02,fixture-model,0,0,0,0,@SUM(A1:A2)\n", bill_csv())
            }),
        )
        .await
        .expect("preview unmatched vendor bill");
    assert!(preview["unmatched_count"].as_i64().unwrap() >= 1);
    assert!(preview["outside_count"].as_i64().unwrap() >= 1);
    let preview_id = preview["id"].as_str().unwrap();
    let export = harness
        .app
        .service
        .vendor_unmatched_csv(&admin(), period["id"].as_i64().unwrap(), preview_id)
        .await
        .expect("export unmatched vendor bill");
    let csv = String::from_utf8(export).unwrap();
    assert!(
        csv.contains("'=SUM(1,2)"),
        "unmatched formula must be neutralized: {csv}"
    );
    assert!(
        csv.contains("'@SUM(A1:A2)"),
        "outside formula must be neutralized: {csv}"
    );
    assert!(!csv.contains("\n=SUM(1,2)"));
    assert!(!csv.contains("\n@SUM(A1:A2)"));

    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query("UPDATE vendor_bill_previews SET expires_at=? WHERE id=?")
        .bind("2020-01-01T00:00:00.000000+00:00")
        .bind(preview_id)
        .execute(&mut *db)
        .await
        .unwrap();
    db.commit().await.unwrap();
    assert_error(
        harness
            .app
            .service
            .vendor_unmatched_csv(&admin(), period["id"].as_i64().unwrap(), preview_id)
            .await,
        409,
        "过期",
    );
}

#[tokio::test]
async fn deleted_vendor_member_keeps_settlement_financial_history() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "vendor-history-create").await;
    let period = period_from_vendor(&vendor);
    let first = cycle_for_account(&harness, 100).await;
    let second = cycle_for_account(&harness, 200).await;
    confirm_vendor_bill(&harness, &period, bill_csv(), false)
        .await
        .expect("confirm vendor bill");

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
        .expect("sync deleted vendor member");

    let current = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let members = harness.app.service.vendors(&admin()).await.unwrap()["vendors"][0]["periods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == period["id"])
        .unwrap()["members"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        members.iter().any(|member| {
            member["id"] == second && member["account_deleted"].as_i64() == Some(1)
        }),
        "a deleted account remains a member of the historical group: {members:?}"
    );

    settle_group(
        &harness,
        &[first, second],
        json!([]),
        "vendor-history-settle",
    )
    .await
    .expect("settle group with deleted historical member");
    let second_row = cycle_row(&harness, second).await;
    assert_eq!(second_row["status"], "settled");
    assert!(second_row["settlement_id"].as_i64().is_some());
    let orders = harness.rows("orders").await;
    assert!(
        orders.iter().any(|order| order["cycle_id"] == second),
        "the deleted member's financial order remains linked to its cycle"
    );
    assert_eq!(
        vendor_period_row(&harness, period["id"].as_i64().unwrap()).await["end"],
        END.replace('+', ".000000+")
    );
    assert_eq!(current["bill_end"], END.replace('+', ".000000+"));
}

async fn insert_estimated_cycle(harness: &Harness, account_id: i64) -> i64 {
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
        "INSERT INTO cycles(account_id,name,cost_fen,creditor_id,start,start_mode,start_lower,start_confirmed,start_locked,cost_kind) VALUES(?,?,?,2,?,'exact',?,1,0,'estimated')",
    )
    .bind(account_id)
    .bind(format!("Fixture account {account_id}"))
    .bind(100_i64)
    .bind(START)
    .bind(START)
    .execute(&mut *db)
    .await
    .unwrap()
    .last_insert_rowid();
    db.commit().await.unwrap();
    id
}

#[tokio::test]
async fn maintenance_excludes_open_estimated_cycles_with_void_history() {
    let harness = Harness::new().await;
    let cycle = insert_estimated_cycle(&harness, 400).await;
    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query(
        "INSERT INTO settlements(cycle_id,snapshot,digest,status,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(cycle)
    .bind("{}")
    .bind("void-history")
    .bind("void")
    .bind("2025-03-01T00:00:00.000000+00:00")
    .execute(&mut *db)
    .await
    .unwrap();
    db.commit().await.unwrap();

    let preview = maintenance::preview(&harness.app.service.store.path)
        .await
        .unwrap();
    assert_eq!(preview["count"], 0);
    assert_eq!(preview["cycle_ids"], json!([]));
}
