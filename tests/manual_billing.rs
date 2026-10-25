#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use settlement::{
    billing::{BillRow, RequestDetail, UsageRow},
    error::Result,
    manual_billing,
    service::Prepared,
};
use std::collections::BTreeMap;
use support::legacy::{END, Harness, START, admin, payer};

fn csv(rows: &[(&str, &str, &str, &str)]) -> String {
    let mut value = String::from(
        "Time,Billed Cost,Model,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens,Request ID\n",
    );
    for (at, amount, model, request_id) in rows {
        value.push_str(&format!("{at},{amount},{model},10,20,0,0,{request_id}\n"));
    }
    value
}

fn basis(source: &str, rates: &[(&str, &str)], ambiguous_usage_ids: &[i64]) -> Value {
    json!({
        "source": source,
        "rates": rates
            .iter()
            .map(|(account_id, rate)| json!({"account_id":account_id.parse::<i64>().unwrap(),"rate":rate}))
            .collect::<Vec<_>>(),
        "ambiguous_usage_ids": ambiguous_usage_ids,
    })
}

fn cycle_payload(account_id: i64, cost: &str) -> Value {
    json!({
        "account_id": account_id,
        "name": "Fixture manual billing cycle",
        "cost": cost,
        "creditor_id": 2,
        "start": START,
        "start_mode": "exact",
    })
}

async fn create_cycle(harness: &Harness, account_id: i64, cost: &str, key: &str) -> i64 {
    harness
        .app
        .service
        .mutate(
            &admin(),
            key,
            "create",
            &cycle_payload(account_id, cost),
            Prepared::default(),
            None,
        )
        .await
        .expect("create fixture cycle")["id"]
        .as_i64()
        .unwrap()
}

async fn create_vendor(harness: &Harness, account_ids: &[i64], cost: &str, key: &str) -> Value {
    harness
        .app
        .service
        .mutate(
            &admin(),
            key,
            "create-vendor",
            &json!({
                "name": "Fixture manual billing group",
                "cost": cost,
                "creditor_id": 2,
                "account_ids": account_ids,
                "start": START,
                "start_mode": "exact"
            }),
            Prepared::default(),
            None,
        )
        .await
        .expect("create fixture vendor group")
}

async fn vendor_period_row(harness: &Harness, period_id: i64) -> Value {
    let mut db = harness.app.service.store.read().await.unwrap();
    let row = settlement::store::one(
        &mut db,
        "SELECT * FROM vendor_periods WHERE id=?",
        &[json!(period_id)],
        "vendor period missing",
    )
    .await
    .unwrap();
    db.commit().await.unwrap();
    row
}

async fn cycle_for_account(harness: &Harness, account_id: i64) -> Value {
    harness
        .rows("cycles")
        .await
        .into_iter()
        .rev()
        .find(|row| row["account_id"] == account_id && row["vendor_period_id"].is_i64())
        .unwrap_or_else(|| panic!("group cycle for account {account_id} not found"))
}

async fn preview_vendor(
    harness: &Harness,
    period: &Value,
    config: Value,
    csv_text: Option<String>,
) -> Result<Value> {
    let mut payload = json!({
        "version": period["version"],
        "end": END,
        "allocation_mode": "proportional",
        "file_name": "fixture-manual-vendor.csv",
        "manual_basis": config,
    });
    if let Some(csv_text) = csv_text {
        payload["csv"] = json!(csv_text);
    }
    harness
        .app
        .service
        .vendor_bill_preview(&admin(), period["id"].as_i64().unwrap(), &payload)
        .await
}

async fn confirm_vendor(harness: &Harness, period: &Value, preview: &Value) -> Result<Value> {
    let pid = period["id"].as_i64().unwrap();
    let payload = json!({
        "preview_id": preview["id"],
        "version": period["version"],
        "accept_difference": false,
        "accept_outside": false,
    });
    let prepared = harness
        .app
        .service
        .prepare_vendor_bill(&admin(), pid, &json!({"preview_id": preview["id"]}))
        .await?;
    harness
        .app
        .service
        .mutate(
            &admin(),
            &format!("manual-vendor-confirm-{pid}"),
            &format!("vendor-bill-confirm:{pid}"),
            &payload,
            Prepared {
                vendor_bill: Some(prepared),
                ..Prepared::default()
            },
            None,
        )
        .await
}

async fn cycle_row(harness: &Harness, cycle_id: i64) -> Value {
    harness
        .rows("cycles")
        .await
        .into_iter()
        .find(|row| row["id"] == cycle_id)
        .unwrap_or_else(|| panic!("cycle {cycle_id} not found"))
}

async fn manual_basis_rows(harness: &Harness) -> Vec<Value> {
    let mut db = harness.app.service.store.read().await.unwrap();
    let rows = settlement::store::all(&mut db, "SELECT * FROM manual_bill_bases ORDER BY id", &[])
        .await
        .unwrap();
    db.commit().await.unwrap();
    rows
}

async fn preview_cycle(
    harness: &Harness,
    cycle_id: i64,
    config: Option<Value>,
    csv_text: Option<String>,
    mode: &str,
) -> Result<Value> {
    let cycle = cycle_row(harness, cycle_id).await;
    let mut payload = json!({
        "version": cycle["version"],
        "end": END,
        "allocation_mode": mode,
        "file_name": "fixture-manual.csv",
    });
    if let Some(csv_text) = csv_text {
        payload["csv"] = json!(csv_text);
    }
    if let Some(config) = config {
        payload["manual_basis"] = config;
    }
    harness
        .app
        .service
        .bill_preview(&admin(), cycle_id, &payload)
        .await
}

async fn confirm_cycle(
    harness: &Harness,
    cycle_id: i64,
    preview: &Value,
    accept_difference: bool,
) -> Result<Value> {
    let cycle = cycle_row(harness, cycle_id).await;
    let payload = json!({
        "version": cycle["version"],
        "preview_id": preview["id"],
        "accept_difference": accept_difference,
        "accept_outside": false,
    });
    let (bill, revision) = harness
        .app
        .service
        .prepare_bill(&admin(), cycle_id, &payload)
        .await?;
    harness
        .app
        .service
        .mutate(
            &admin(),
            &format!(
                "manual-bill-confirm-{cycle_id}-{accept_difference}-{}",
                preview["id"].as_str().unwrap_or("preview")
            ),
            &format!("bill-confirm:{cycle_id}"),
            &payload,
            Prepared {
                bill: Some((bill, revision)),
                ..Prepared::default()
            },
            None,
        )
        .await
}

#[tokio::test]
async fn independent_cycle_can_return_to_local_proportions_with_the_same_cost() {
    for source in ["local_only", "upstream"] {
        let harness = Harness::new().await;
        let cycle_id = create_cycle(&harness, 100, "10.00", "local-proportion-create").await;
        let (config, csv_text) = if source == "local_only" {
            (Some(basis("local_only", &[("100", "1")], &[])), None)
        } else {
            (
                None,
                Some(csv(&[(START, "10.00", "fixture-model", "fixture-1")])),
            )
        };
        let bill = preview_cycle(&harness, cycle_id, config, csv_text, "matched_only")
            .await
            .expect("old billing basis preview");
        confirm_cycle(&harness, cycle_id, &bill, false)
            .await
            .expect("confirm old billing basis");
        let before = cycle_row(&harness, cycle_id).await;
        assert_eq!(before["cost_fen"], 1000);
        let saved_bases = manual_basis_rows(&harness).await;
        let saved_imports = harness.rows("bill_imports").await;
        let old_preview = harness
            .app
            .service
            .preview(&admin(), &json!({"ids":[cycle_id],"end":END}))
            .await
            .expect("old settlement preview");
        let old_payload = json!({"preview_id":old_preview["id"],"renewals":[]});
        let old_prepared = harness
            .app
            .service
            .prepare_settlement(&admin(), &old_payload)
            .await
            .expect("prepare old settlement");
        preview_cycle(
            &harness,
            cycle_id,
            Some(basis("local_only", &[("100", "2")], &[])),
            None,
            "matched_only",
        )
        .await
        .expect("leave an unconfirmed manual billing draft");
        let blocked = harness
            .app
            .service
            .preview(&admin(), &json!({"ids":[cycle_id],"end":END}))
            .await
            .expect("blocked settlement preview");
        assert_eq!(blocked["cycles"][0]["can_settle"], false);

        let mut reset = cycle_payload(100, "10.00");
        reset["version"] = before["version"].clone();
        reset["cost_source"] = json!("manual");
        harness
            .app
            .service
            .mutate(
                &admin(),
                "return-to-local-proportions",
                &format!("edit:{cycle_id}"),
                &reset,
                Prepared::default(),
                None,
            )
            .await
            .expect("return to local proportions without changing the cost");
        let current = cycle_row(&harness, cycle_id).await;
        assert_eq!(current["cost_fen"], before["cost_fen"]);
        assert_eq!(current["cost_source"], "manual");
        assert_eq!(current["bill_allocation_mode"], "proportional");
        for key in [
            "bill_import_id",
            "manual_basis_id",
            "bill_end",
            "bill_digest",
            "billable_cost_fen",
        ] {
            assert!(current[key].is_null(), "stale billing field: {key}");
        }
        assert_error(
            harness
                .app
                .service
                .mutate(
                    &admin(),
                    "reject-old-billing-settlement",
                    "settle",
                    &old_payload,
                    old_prepared,
                    None,
                )
                .await,
            409,
            "成本或周期已修改",
        );

        let preview = harness
            .app
            .service
            .preview(&admin(), &json!({"ids":[cycle_id],"end":END}))
            .await
            .expect("local consumption settlement preview");
        let calculation = &preview["cycles"][0];
        assert_eq!(calculation["can_settle"], true);
        assert!(calculation.get("bill").is_none());
        let allocations = calculation["allocations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["user_id"].as_i64().unwrap(),
                    row["amount_fen"].as_i64().unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(allocations, BTreeMap::from([(2, 100), (3, 500), (4, 400)]));
        let payload = json!({"preview_id":preview["id"],"renewals":[]});
        let prepared = harness
            .app
            .service
            .prepare_settlement(&admin(), &payload)
            .await
            .expect("prepare local consumption settlement");
        harness
            .app
            .service
            .mutate(
                &admin(),
                "settle-local-proportions",
                "settle",
                &payload,
                prepared,
                None,
            )
            .await
            .expect("settle without a CSV or manual multiplier");
        assert_eq!(harness.rows("settlements").await.len(), 1);
        assert_eq!(manual_basis_rows(&harness).await, saved_bases);
        assert_eq!(harness.rows("bill_imports").await, saved_imports);
    }
}

#[tokio::test]
async fn normal_csv_preview_replaces_pending_manual_preview_for_independent_and_group() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "manual-draft-normal-create").await;
    let manual_preview = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("local_only", &[("100", "1")], &[])),
        None,
        "matched_only",
    )
    .await
    .expect("local-only preview");
    assert_eq!(manual_preview["manual_summary"]["source"], "local_only");

    let normal_preview = preview_cycle(
        &harness,
        cycle_id,
        None,
        Some(csv(&[(
            "2025-01-01T00:00:00+00:00",
            "1.00",
            "fixture-model",
            "fixture-1",
        )])),
        "matched_only",
    )
    .await
    .expect("normal CSV preview");
    assert!(normal_preview.get("manual_basis").is_none());
    assert_error(
        confirm_cycle(&harness, cycle_id, &manual_preview, false).await,
        404,
        "账单预览不存在",
    );
    confirm_cycle(&harness, cycle_id, &normal_preview, false)
        .await
        .expect("confirm normal CSV preview");
    let cycle = cycle_row(&harness, cycle_id).await;
    assert_eq!(cycle["cost_source"], "upstream_bill");
    assert!(cycle["manual_basis_id"].is_null());

    let harness = Harness::new().await;
    let vendor = create_vendor(
        &harness,
        &[100, 200],
        "6.00",
        "manual-group-draft-normal-create",
    )
    .await;
    let period = vendor["period"].clone();
    let manual_preview = preview_vendor(
        &harness,
        &period,
        basis("local_only", &[("100", "1"), ("200", "1")], &[]),
        None,
    )
    .await
    .expect("group local-only preview");
    assert_eq!(manual_preview["manual_summary"]["source"], "local_only");

    let normal_payload = json!({
        "version": period["version"],
        "end": END,
        "allocation_mode": "proportional",
        "file_name": "fixture-normal-vendor.csv",
        "csv": csv(&[
            (
                "2025-01-01T00:00:00+00:00",
                "1.00",
                "fixture-model",
                "fixture-1",
            ),
            (
                "2025-01-10T00:00:00+00:00",
                "3.00",
                "fixture-model",
                "fixture-2",
            ),
            (
                "2025-01-05T00:00:00+00:00",
                "2.00",
                "fixture-model",
                "fixture-6",
            ),
        ]),
    });
    let normal_preview = harness
        .app
        .service
        .vendor_bill_preview(&admin(), period["id"].as_i64().unwrap(), &normal_payload)
        .await
        .expect("normal group CSV preview");
    assert!(normal_preview.get("manual_basis").is_none());
    assert_error(
        confirm_vendor(&harness, &period, &manual_preview).await,
        404,
        "记录不存在",
    );
    confirm_vendor(&harness, &period, &normal_preview)
        .await
        .expect("confirm normal group CSV preview");
    let period = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    assert!(period["manual_basis_id"].is_null());
    assert!(period["bill_import_id"].is_i64());
}

fn usage(id: i64, root_id: i64, user_id: i64, amount: &str, at: &str) -> UsageRow {
    UsageRow {
        account_id: root_id,
        amount: amount.to_owned(),
        created_at: at.to_owned(),
        id,
        root_id,
        user_id,
    }
}

fn bill(row: i64, amount: &str, request_id: &str, at: &str) -> BillRow {
    BillRow {
        amount: amount.to_owned(),
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        created_at: at.to_owned(),
        input_tokens: 10,
        model: "fixture-model".to_owned(),
        output_tokens: 20,
        request_id: request_id.to_owned(),
        row,
    }
}

fn detail(request_id: &str) -> RequestDetail {
    RequestDetail {
        request_id: Some(request_id.to_owned()),
        model: Some("fixture-model".to_owned()),
        input_tokens: Some(10),
        output_tokens: Some(20),
        cache_read_tokens: Some(0),
        cache_creation_tokens: Some(0),
        ..RequestDetail::default()
    }
}

fn assert_error<T>(result: Result<T>, status: u16, clue: &str) {
    let error = match result {
        Ok(_) => panic!("operation should fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, status, "unexpected error: {error}");
    assert!(
        error.message.contains(clue),
        "expected {clue:?}, got {:?}",
        error.message
    );
}

#[test]
fn manual_basis_normalization_enforces_rates_and_empty_rates_are_disabled() {
    let normalized = manual_billing::normalize(&basis(
        "upstream",
        &[("100", ""), ("200", "0"), ("300", "0.150000")],
        &[],
    ))
    .expect("valid manual basis");
    assert_eq!(
        normalized["rates"],
        json!([
            {"account_id":200,"rate":"0"},
            {"account_id":300,"rate":"0.150000"}
        ])
    );
    assert_error(
        manual_billing::normalize(&basis("upstream", &[("100", "0.1234567")], &[])),
        422,
        "最多六位小数",
    );
    assert_error(
        manual_billing::normalize(&basis("unknown", &[("100", "0.5")], &[])),
        422,
        "补计来源无效",
    );
}

#[test]
fn manual_reconcile_uses_proportional_and_matched_only_formulas() {
    let usage = vec![
        usage(1, 100, 3, "2", "2025-01-02T00:00:00+00:00"),
        usage(2, 100, 3, "3", "2025-01-03T00:00:00+00:00"),
    ];
    let mut details = BTreeMap::new();
    details.insert(1, detail("fixture-1"));
    details.insert(2, detail("fixture-2"));
    let rows = vec![
        bill(2, "4", "fixture-1", "2025-01-02T00:00:00+00:00"),
        bill(3, "6", "missing-upstream", "2025-01-04T00:00:00+00:00"),
    ];
    let config = basis("upstream", &[("100", "0.5")], &[]);
    let proportional = manual_billing::reconcile(
        &rows,
        &usage,
        &details,
        START,
        END,
        "proportional",
        &config,
        &[100],
    )
    .expect("manual proportional reconciliation");
    assert_eq!(proportional["matched_amount"], "4");
    assert_eq!(proportional["manual_summary"]["amount"], "1.5");
    assert_eq!(proportional["cost_fen"], 1150);
    assert_eq!(proportional["upstream_total_fen"], 1000);
    assert_eq!(proportional["can_confirm"], true);
    let allocations = manual_billing::allocations(&proportional, 2).unwrap();
    assert_eq!(
        allocations
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (
                row["user_id"].as_i64().unwrap(),
                row["amount_fen"].as_i64().unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![(3, 1150)]
    );

    let matched_only = manual_billing::reconcile(
        &rows,
        &usage,
        &details,
        START,
        END,
        "matched_only",
        &config,
        &[100],
    )
    .expect("manual matched-only reconciliation");
    assert_eq!(matched_only["cost_fen"], 550);
    assert_eq!(matched_only["billable_cost_fen"], 550);

    let no_positive = manual_billing::reconcile(
        &[bill(
            1,
            "10",
            "missing-upstream",
            "2025-01-04T00:00:00+00:00",
        )],
        &usage,
        &details,
        START,
        END,
        "proportional",
        &config,
        &[100],
    )
    .expect("manual missing-only reconciliation");
    assert_eq!(no_positive["matched_amount"], "0");
    assert_eq!(no_positive["total"], "10");
    assert_eq!(no_positive["can_confirm"], false);
    assert_error(
        manual_billing::reconcile(
            &rows,
            &usage,
            &details,
            START,
            END,
            "proportional",
            &basis("upstream", &[("100", "0.5")], &[999]),
            &[100],
        ),
        422,
        "所选请求不属于本窗口",
    );
}

#[test]
fn manual_reconcile_reports_ambiguous_candidates_and_selects_only_requested_rows() {
    let usage = vec![
        usage(1, 100, 3, "2", "2025-01-02T00:00:00+00:00"),
        usage(2, 100, 4, "3", "2025-01-02T00:00:01+00:00"),
    ];
    let mut details = BTreeMap::new();
    details.insert(1, detail(""));
    details.insert(2, detail(""));
    let row = bill(2, "4", "", "2025-01-02T00:00:00+00:00");
    let config = basis("upstream", &[("100", "0.5")], &[]);
    let defaulted = manual_billing::reconcile(
        &[row],
        &usage,
        &details,
        START,
        END,
        "proportional",
        &config,
        &[100],
    )
    .expect("ambiguous manual reconciliation");
    assert_eq!(defaulted["matched_count"], 0);
    assert_eq!(defaulted["local_unmatched"][0]["reason"], "ambiguous");
    assert_eq!(defaulted["local_unmatched"][0]["included"], false);

    let selected = manual_billing::reconcile(
        &[bill(2, "4", "", "2025-01-02T00:00:00+00:00")],
        &usage,
        &details,
        START,
        END,
        "proportional",
        &basis("upstream", &[("100", "0.5")], &[2]),
        &[100],
    )
    .expect("selected ambiguous manual reconciliation");
    let selected_row = selected["local_unmatched"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["usage_id"] == 2)
        .unwrap();
    assert_eq!(selected_row["reason"], "ambiguous");
    assert_eq!(selected_row["included"], true);
    assert_eq!(selected["manual_summary"]["amount"], "1.5");
}

#[test]
fn manual_reconcile_can_use_supplement_when_upstream_basis_is_zero_or_excluded() {
    let usage = vec![usage(1, 100, 3, "2", "2025-01-02T00:00:00+00:00")];
    let mut details = BTreeMap::new();
    details.insert(1, detail("fixture-1"));
    let config = basis("upstream", &[("100", "0.5")], &[]);

    let no_upstream_rows = manual_billing::reconcile(
        &[],
        &usage,
        &details,
        START,
        END,
        "proportional",
        &config,
        &[100],
    )
    .expect("zero upstream total with local supplement");
    assert_eq!(no_upstream_rows["can_confirm"], true);
    assert_eq!(no_upstream_rows["cost_fen"], 100);
    assert_eq!(
        manual_billing::allocations(&no_upstream_rows, 2).unwrap()[0]["amount_fen"],
        100
    );

    let excluded_upstream = manual_billing::reconcile(
        &[bill(
            2,
            "9",
            "no-local-request",
            "2025-01-03T00:00:00+00:00",
        )],
        &usage,
        &details,
        START,
        END,
        "matched_only",
        &config,
        &[100],
    )
    .expect("matched-only excludes unmatched upstream rows");
    assert_eq!(excluded_upstream["matched_amount"], "0");
    assert_eq!(excluded_upstream["can_confirm"], true);
    assert_eq!(excluded_upstream["cost_fen"], 100);
}

#[tokio::test]
async fn independent_local_only_has_no_csv_or_fabricated_import_and_persists_summary() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "manual-local-only-create").await;
    let preview = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("local_only", &[("100", "1")], &[])),
        None,
        "matched_only",
    )
    .await
    .expect("local-only preview");
    assert_eq!(preview["manual_summary"]["source"], "local_only");
    assert_eq!(preview["upstream_total_fen"], Value::Null);
    assert_eq!(preview["file_hash"], "");
    assert_eq!(preview["can_confirm"], true);
    assert!(preview["local_unmatched"].as_array().unwrap().len() >= 2);

    confirm_cycle(&harness, cycle_id, &preview, false)
        .await
        .expect("confirm local-only preview");
    assert!(
        harness.rows("bill_imports").await.is_empty(),
        "local-only confirmation must not fabricate a bill import"
    );
    let row = cycle_row(&harness, cycle_id).await;
    assert_eq!(row["cost_source"], "manual_bill");
    assert!(row["bill_import_id"].is_null());
    assert!(row["manual_basis_id"].is_i64());
}

#[tokio::test]
async fn independent_manual_preview_excludes_matched_rows_and_selects_ambiguous_ids() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "manual-upstream-create").await;
    let preview = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("upstream", &[("100", "0.5")], &[])),
        Some(csv(&[
            (
                "2025-01-01T00:00:00+00:00",
                "1.00",
                "fixture-model",
                "fixture-1",
            ),
            (
                "2025-01-25T00:00:00+00:00",
                "4.00",
                "fixture-model",
                "missing-row",
            ),
        ])),
        "proportional",
    )
    .await
    .expect("manual upstream preview");
    assert_eq!(preview["matched_count"], 1);
    assert_eq!(preview["local_unmatched_count"], 3);
    assert!(
        preview["local_unmatched"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["usage_id"] != 1),
        "a local usage already matched by CSV must not be supplemented"
    );
    assert!(preview["manual_summary"]["amount"].as_str().unwrap() > "0");

    let selected = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("upstream", &[("100", "0.5")], &[1])),
        Some(csv(&[
            (
                "2025-01-01T00:00:00+00:00",
                "1.00",
                "fixture-model",
                "fixture-1",
            ),
            (
                "2025-01-01T00:00:00+00:00",
                "1.00",
                "fixture-model",
                "fixture-1",
            ),
        ])),
        "proportional",
    )
    .await
    .expect("select local missing usage");
    let selected_row = selected["local_unmatched"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["usage_id"] == 1)
        .expect("selected local usage row");
    assert_eq!(selected_row["included"], true);
    assert_eq!(selected_row["rate"], "0.5");

    let old_confirm = confirm_cycle(&harness, cycle_id, &preview, true).await;
    assert_error(old_confirm, 404, "账单预览不存在");
}

#[tokio::test]
async fn independent_manual_preview_rejects_non_positive_matched_basis_and_window_drift() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "manual-window-create").await;
    let config = basis("upstream", &[("100", "0.5")], &[]);
    let no_positive = preview_cycle(
        &harness,
        cycle_id,
        Some(config.clone()),
        Some(csv(&[(
            "2025-01-25T00:00:00+00:00",
            "4.00",
            "fixture-model",
            "not-local",
        )])),
        "proportional",
    )
    .await
    .expect("preview with only missing upstream row");
    assert_eq!(no_positive["matched_amount"], "0");
    assert_eq!(no_positive["can_confirm"], false);

    let row = cycle_row(&harness, cycle_id).await;
    let stale = harness
        .app
        .service
        .bill_preview(
            &admin(),
            cycle_id,
            &json!({
                "version": row["version"].as_i64().unwrap() - 1,
                "end": END,
                "allocation_mode": "proportional",
                "manual_basis": config,
                "csv": csv(&[("2025-01-01T00:00:00+00:00", "1.00", "fixture-model", "fixture-1")]),
                "file_name": "fixture-manual.csv"
            }),
        )
        .await;
    assert_error(stale, 409, "周期已修改");

    let valid_preview = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("local_only", &[("100", "1")], &[])),
        None,
        "matched_only",
    )
    .await
    .expect("valid preview before version drift");
    let current = cycle_row(&harness, cycle_id).await;
    harness
        .app
        .service
        .mutate(
            &admin(),
            "manual-window-drift-edit",
            &format!("edit:{cycle_id}"),
            &json!({
                "account_id": 100,
                "name": "Fixture manual billing cycle",
                "cost": "11.00",
                "creditor_id": 2,
                "start": START,
                "start_mode": "exact",
                "cost_kind": "exact",
                "version": current["version"]
            }),
            Prepared::default(),
            None,
        )
        .await
        .expect("edit cycle to invalidate the preview");
    assert_error(
        confirm_cycle(&harness, cycle_id, &valid_preview, false).await,
        409,
        "成本或周期",
    );
    assert!(harness.rows("bill_imports").await.is_empty());
}

#[tokio::test]
async fn independent_local_only_settlement_freezes_manual_snapshot_and_renews_idempotently() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "manual-settle-local-create").await;
    let bill_preview = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("local_only", &[("100", "1")], &[])),
        None,
        "matched_only",
    )
    .await
    .expect("local-only bill preview");
    confirm_cycle(&harness, cycle_id, &bill_preview, false)
        .await
        .expect("confirm local-only basis");
    let confirmed = cycle_row(&harness, cycle_id).await;
    let frozen_cost = confirmed["cost_fen"].as_i64().unwrap();
    assert_eq!(confirmed["cost_source"], "manual_bill");
    assert!(confirmed["manual_basis_id"].is_i64());

    let settlement_preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":[cycle_id],"expense_ids":[],"end":END}),
        )
        .await
        .expect("ordinary settlement preview");
    assert_eq!(settlement_preview["cycles"][0]["can_settle"], true);
    let payload = json!({
        "preview_id":settlement_preview["id"],
        "renewals":[{"cycle_id":cycle_id,"cost":"12.00","creditor_id":2}],
        "delete_cycle_ids":[],
        "vendor_renewals":[]
    });
    let prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &payload)
        .await
        .expect("prepare ordinary settlement");
    let first = harness
        .app
        .service
        .mutate(
            &admin(),
            "manual-settle-local-operation",
            "settle",
            &payload,
            prepared,
            None,
        )
        .await
        .expect("settle local-only cycle and renew");
    let retry = harness
        .app
        .service
        .mutate(
            &admin(),
            "manual-settle-local-operation",
            "settle",
            &payload,
            Prepared::default(),
            None,
        )
        .await
        .expect("replay settlement operation");
    assert_eq!(retry, first, "same operation number must replay its result");
    assert_eq!(harness.rows("settlements").await.len(), 1);

    let settled_cycle = cycle_row(&harness, cycle_id).await;
    assert_eq!(settled_cycle["status"], "settled");
    assert_eq!(settled_cycle["cost_fen"], frozen_cost);
    assert!(settled_cycle["manual_basis_id"].is_i64());
    let settlement = harness.rows("settlements").await.remove(0);
    let snapshot: Value = serde_json::from_str(settlement["snapshot"].as_str().unwrap())
        .expect("settlement snapshot JSON");
    assert_eq!(snapshot["cycle"]["cost_source"], "manual_bill");
    assert_eq!(snapshot["cycle"]["cost_fen"], frozen_cost);
    assert_eq!(snapshot["bill"]["manual_basis"]["source"], "local_only");
    assert_eq!(
        snapshot["bill"]["manual_basis"]["rates"][0]["account_id"],
        100
    );
    assert!(
        snapshot["bill"]["manual_summary"]["amount"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            > 0.0
    );

    let next_id = first["settlements"][0]["next_cycle"]
        .as_i64()
        .expect("renewed cycle id");
    let next = cycle_row(&harness, next_id).await;
    assert_eq!(next["renewed_from_cycle_id"], cycle_id);
    assert_eq!(next["cost_fen"], 1200);
    assert!(next["manual_basis_id"].is_null());
}

#[tokio::test]
async fn manual_basis_change_blocks_old_settlement_prepare_until_reconfirmed() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "manual-draft-create").await;
    let old_preview = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("local_only", &[("100", "1")], &[])),
        None,
        "matched_only",
    )
    .await
    .expect("old basis preview");
    confirm_cycle(&harness, cycle_id, &old_preview, false)
        .await
        .expect("confirm old basis");

    let settlement_preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":[cycle_id],"expense_ids":[],"end":END}),
        )
        .await
        .expect("old ordinary settlement preview");
    let old_payload = json!({
        "preview_id":settlement_preview["id"],
        "renewals":[],
        "delete_cycle_ids":[],
        "vendor_renewals":[]
    });
    let old_prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &old_payload)
        .await
        .expect("prepare old ordinary settlement");

    let changed_preview = preview_cycle(
        &harness,
        cycle_id,
        Some(basis("local_only", &[("100", "0.5")], &[])),
        None,
        "matched_only",
    )
    .await
    .expect("changed manual basis preview");
    assert_error(
        harness
            .app
            .service
            .mutate(
                &admin(),
                "manual-draft-old-settlement",
                "settle",
                &old_payload,
                old_prepared,
                None,
            )
            .await,
        409,
        "人工计费依据已修改",
    );

    confirm_cycle(&harness, cycle_id, &changed_preview, false)
        .await
        .expect("confirm changed basis");
    let current = cycle_row(&harness, cycle_id).await;
    assert_eq!(current["cost_fen"], 500);
    let final_preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":[cycle_id],"expense_ids":[],"end":END}),
        )
        .await
        .expect("new ordinary settlement preview");
    assert_eq!(final_preview["cycles"][0]["can_settle"], true);
    let final_payload = json!({
        "preview_id":final_preview["id"],
        "renewals":[],
        "delete_cycle_ids":[],
        "vendor_renewals":[]
    });
    let final_prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &final_payload)
        .await
        .expect("prepare new ordinary settlement");
    harness
        .app
        .service
        .mutate(
            &admin(),
            "manual-draft-new-settlement",
            "settle",
            &final_payload,
            final_prepared,
            None,
        )
        .await
        .expect("settle after reconfirming changed basis");
    assert_eq!(harness.rows("settlements").await.len(), 1);
}

#[tokio::test]
async fn vendor_local_only_settlement_preserves_group_cost_and_single_basis_on_renewal() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "manual-group-settle-create").await;
    let period = vendor["period"].clone();
    let first = cycle_for_account(&harness, 100).await["id"]
        .as_i64()
        .unwrap();
    let second = cycle_for_account(&harness, 200).await["id"]
        .as_i64()
        .unwrap();
    let bill_preview = preview_vendor(
        &harness,
        &period,
        basis("local_only", &[("100", "1"), ("200", "1")], &[]),
        None,
    )
    .await
    .expect("group local-only preview");
    confirm_vendor(&harness, &period, &bill_preview)
        .await
        .expect("confirm group local-only basis");
    let current_period = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    let settlement_preview = harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":[first,second],"expense_ids":[],"end":END}),
        )
        .await
        .expect("group settlement preview");
    assert!(
        settlement_preview["cycles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cycle| cycle["can_settle"] == true)
    );
    let payload = json!({
        "preview_id":settlement_preview["id"],
        "renewals":[],
        "delete_cycle_ids":[],
        "vendor_renewals":[{"period_id":period["id"],"version":current_period["version"],"cost":"7.00","creditor_id":2,"cycle_ids":[first,second]}]
    });
    let prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &payload)
        .await
        .expect("prepare group settlement");
    let result = harness
        .app
        .service
        .mutate(
            &admin(),
            "manual-group-settle-renew",
            "settle",
            &payload,
            prepared,
            None,
        )
        .await
        .expect("settle and renew whole group");
    assert_eq!(result["settlements"].as_array().unwrap().len(), 2);

    let old_period = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    assert_eq!(old_period["status"], "settled");
    assert_eq!(old_period["cost_fen"], 600);
    assert!(old_period["manual_basis_id"].is_i64());
    let bases = manual_basis_rows(&harness).await;
    assert_eq!(bases.len(), 1, "group confirmation stores one manual basis");
    assert_eq!(bases[0]["vendor_period_id"], period["id"]);
    assert!(bases[0]["cycle_id"].is_null());

    let settled_cycles: Vec<_> = harness
        .rows("cycles")
        .await
        .into_iter()
        .filter(|cycle| cycle["vendor_period_id"] == period["id"])
        .collect();
    assert_eq!(settled_cycles.len(), 2);
    assert_eq!(
        settled_cycles
            .iter()
            .map(|c| c["cost_fen"].as_i64().unwrap())
            .sum::<i64>(),
        600
    );
    for cycle in &settled_cycles {
        assert_eq!(cycle["status"], "settled");
        assert!(cycle["manual_basis_id"].is_null());
    }

    let next_id = result["vendor_renewals"][0]["id"].as_i64().unwrap();
    let next_period = vendor_period_row(&harness, next_id).await;
    assert_eq!(next_period["status"], "open");
    assert_eq!(next_period["cost_fen"], 700);
    assert!(next_period["manual_basis_id"].is_null());
    let renewed_cycles: Vec<_> = harness
        .rows("cycles")
        .await
        .into_iter()
        .filter(|cycle| cycle["vendor_period_id"] == next_id)
        .collect();
    assert_eq!(renewed_cycles.len(), 2);
    assert!(
        renewed_cycles
            .iter()
            .all(|cycle| cycle["manual_basis_id"].is_null())
    );
}

#[tokio::test]
async fn replacing_manual_csv_moves_matched_usage_out_of_supplement_without_accumulating_rows() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "manual-csv-replace-create").await;
    let config = basis("upstream", &[("100", "0.5")], &[]);
    let first = preview_cycle(
        &harness,
        cycle_id,
        Some(config.clone()),
        Some(csv(&[(
            "2025-01-01T00:00:00+00:00",
            "1.00",
            "fixture-model",
            "fixture-1",
        )])),
        "matched_only",
    )
    .await
    .expect("first manual CSV preview");
    assert_eq!(first["matched_count"], 1);
    assert!(
        first["local_unmatched"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["usage_id"] != 1)
    );
    assert!(
        first["local_unmatched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["usage_id"] == 2 && row["included"] == true)
    );

    let second = preview_cycle(
        &harness,
        cycle_id,
        Some(config),
        Some(csv(&[(
            "2025-01-10T00:00:00+00:00",
            "3.00",
            "fixture-model",
            "fixture-2",
        )])),
        "matched_only",
    )
    .await
    .expect("replacement manual CSV preview");
    assert_ne!(first["file_hash"], second["file_hash"]);
    assert_eq!(second["matched_count"], 1);
    assert!(
        second["local_unmatched"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["usage_id"] != 2)
    );
    assert!(
        second["local_unmatched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["usage_id"] == 1 && row["included"] == true)
    );
    confirm_cycle(&harness, cycle_id, &second, false)
        .await
        .expect("confirm replacement CSV");
    let imports = harness.rows("bill_imports").await;
    assert_eq!(imports.len(), 1, "replacement CSV must create one import");
    let imported_rows: Value =
        serde_json::from_str(imports[0]["rows"].as_str().unwrap()).expect("imported CSV rows JSON");
    assert_eq!(imported_rows.as_array().unwrap().len(), 1);
    assert_eq!(imported_rows[0]["request_id"], "fixture-2");
}

#[test]
fn manual_micro_amounts_round_once_and_ties_are_order_independent_by_user_id() {
    let mut details = BTreeMap::new();
    details.insert(1, detail(""));
    details.insert(2, detail(""));
    let config = basis("local_only", &[("100", "1")], &[]);
    let forward = vec![
        usage(1, 100, 4, "0.005", "2025-01-02T00:00:00+00:00"),
        usage(2, 100, 3, "0.005", "2025-01-02T00:00:01+00:00"),
    ];
    let reverse = vec![
        usage(2, 100, 3, "0.005", "2025-01-02T00:00:01+00:00"),
        usage(1, 100, 4, "0.005", "2025-01-02T00:00:00+00:00"),
    ];
    let first = manual_billing::reconcile(
        &[],
        &forward,
        &details,
        START,
        END,
        "matched_only",
        &config,
        &[100],
    )
    .expect("forward micro amount reconciliation");
    let second = manual_billing::reconcile(
        &[],
        &reverse,
        &details,
        START,
        END,
        "matched_only",
        &config,
        &[100],
    )
    .expect("reverse micro amount reconciliation");
    assert_eq!(first["manual_summary"]["amount"], "0.010");
    assert_eq!(first["cost_fen"], 1);
    assert_eq!(second["cost_fen"], first["cost_fen"]);
    let amounts = |value: &Value| {
        manual_billing::allocations(value, 2)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["user_id"].as_i64().unwrap(),
                    row["amount_fen"].as_i64().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(amounts(&first), vec![(3, 1), (4, 0)]);
    assert_eq!(amounts(&second), amounts(&first));
}

#[tokio::test]
async fn manual_preview_requires_admin_and_legacy_bill_summary_is_unchanged_without_basis() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(&harness, 100, "10.00", "legacy-summary-create").await;
    let cycle = cycle_row(&harness, cycle_id).await;
    let legacy = harness
        .app
        .service
        .bill_preview(
            &admin(),
            cycle_id,
            &json!({
                "version": cycle["version"],
                "end": END,
                "allocation_mode": "matched_only",
                "file_name": "legacy.csv",
                "csv": csv(&[("2025-01-01T00:00:00+00:00", "1.00", "fixture-model", "fixture-1")])
            }),
        )
        .await
        .expect("legacy bill preview");
    assert!(legacy["manual_summary"].is_null());
    assert!(legacy["local_unmatched"].is_null());
    assert!(legacy["upstream_total_fen"].is_null());
    assert_eq!(legacy["billable_cost_fen"], 100);

    let denied = harness
        .app
        .service
        .bill_preview(
            &payer(),
            cycle_id,
            &json!({
                "version": cycle["version"],
                "end": END,
                "allocation_mode": "matched_only",
                "manual_basis": basis("local_only", &[("100", "1")], &[])
            }),
        )
        .await;
    assert_error(denied, 403, "管理员");
}

#[tokio::test]
async fn vendor_manual_local_only_splits_fixed_group_cost_and_writes_no_import() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "manual-vendor-local-create").await;
    let period = vendor["period"].clone();
    let preview = preview_vendor(
        &harness,
        &period,
        basis("local_only", &[("100", "1"), ("200", "1")], &[]),
        None,
    )
    .await
    .expect("local-only vendor preview");
    assert_eq!(preview["vendor_period"]["cost_fen"], 600);
    assert_eq!(preview["manual_summary"]["source"], "local_only");
    assert_eq!(preview["upstream_total_fen"], Value::Null);
    assert_eq!(preview["can_confirm"], true);
    let accounts = preview["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(
        accounts
            .iter()
            .map(|a| a["amount_fen"].as_i64().unwrap())
            .sum::<i64>(),
        600
    );
    assert_eq!(
        accounts.iter().find(|a| a["account_id"] == 100).unwrap()["amount_fen"],
        500
    );
    assert_eq!(
        accounts.iter().find(|a| a["account_id"] == 200).unwrap()["amount_fen"],
        100
    );

    let refreshed = preview_vendor(
        &harness,
        &period,
        basis("local_only", &[("100", "1"), ("200", "1")], &[]),
        None,
    )
    .await
    .expect("refreshed local-only vendor preview");
    let stale = harness
        .app
        .service
        .prepare_vendor_bill(
            &admin(),
            period["id"].as_i64().unwrap(),
            &json!({"preview_id": preview["id"]}),
        )
        .await;
    assert_error(stale, 404, "记录不存在");

    confirm_vendor(&harness, &period, &refreshed)
        .await
        .expect("confirm local-only vendor preview");
    assert!(
        harness
            .app
            .service
            .vendor_bill_preview(
                &admin(),
                period["id"].as_i64().unwrap(),
                &json!({
                    "version": period["version"],
                    "end": END,
                    "allocation_mode": "proportional",
                    "manual_basis": basis("local_only", &[("100", "1"), ("200", "1")], &[])
                }),
            )
            .await
            .is_err(),
        "confirmed group period must advance its version"
    );
    let current = vendor_period_row(&harness, period["id"].as_i64().unwrap()).await;
    assert!(current["manual_basis_id"].is_i64());
    assert!(current["bill_import_id"].is_null());
    for account_id in [100, 200] {
        let cycle = cycle_for_account(&harness, account_id).await;
        assert_eq!(cycle["cost_source"], "vendor_bill");
        assert!(cycle["cost_fen"].as_i64().unwrap() > 0);
    }
    let first_cost = cycle_for_account(&harness, 100).await["cost_fen"]
        .as_i64()
        .unwrap();
    let second_cost = cycle_for_account(&harness, 200).await["cost_fen"]
        .as_i64()
        .unwrap();
    assert_eq!(first_cost + second_cost, 600);
}

#[tokio::test]
async fn vendor_manual_zero_rates_do_not_fallback_to_local_consumption() {
    let harness = Harness::new().await;
    let vendor = create_vendor(&harness, &[100, 200], "6.00", "manual-vendor-zero-create").await;
    let period = vendor["period"].clone();
    let preview = preview_vendor(
        &harness,
        &period,
        basis("local_only", &[("100", "0"), ("200", "")], &[]),
        None,
    )
    .await
    .expect("zero-rate vendor preview");
    assert_eq!(preview["manual_summary"]["amount"], "0");
    assert_eq!(preview["can_confirm"], false);
    assert!(
        preview["accounts"].as_array().unwrap().iter().all(|a| {
            a["amount_fen"].is_null() && a["allocations"].as_array().unwrap().is_empty()
        })
    );
    assert_error(
        confirm_vendor(&harness, &period, &preview).await,
        422,
        "没有正金额",
    );
}

#[tokio::test]
async fn vendor_manual_upstream_weights_include_supplement_without_changing_group_cost() {
    let harness = Harness::new().await;
    let vendor = create_vendor(
        &harness,
        &[100, 200],
        "6.00",
        "manual-vendor-upstream-create",
    )
    .await;
    let period = vendor["period"].clone();
    let preview = preview_vendor(
        &harness,
        &period,
        basis("upstream", &[("100", "0.5"), ("200", "1")], &[]),
        Some(csv(&[
            (
                "2025-01-01T00:00:00+00:00",
                "1.00",
                "fixture-model",
                "fixture-1",
            ),
            (
                "2025-01-22T00:00:00+00:00",
                "7.00",
                "fixture-model",
                "missing-upstream",
            ),
        ])),
    )
    .await
    .expect("group upstream and manual supplement preview");

    assert_eq!(preview["vendor_period"]["cost_fen"], 600);
    assert_eq!(preview["cost_fen"], 600);
    assert_eq!(preview["upstream_total_fen"], 800);
    assert_eq!(preview["manual_summary"]["amount"], "6.5");
    let accounts = preview["accounts"].as_array().unwrap();
    assert_eq!(
        accounts
            .iter()
            .map(|a| a["amount_fen"].as_i64().unwrap())
            .sum::<i64>(),
        600
    );
    assert_eq!(
        accounts.iter().find(|a| a["account_id"] == 100).unwrap()["manual_amount"],
        "4.5"
    );
    assert_eq!(
        accounts.iter().find(|a| a["account_id"] == 200).unwrap()["manual_amount"],
        "2"
    );
    assert_eq!(
        accounts.iter().find(|a| a["account_id"] == 100).unwrap()["amount_fen"],
        440
    );
    assert_eq!(
        accounts.iter().find(|a| a["account_id"] == 200).unwrap()["amount_fen"],
        160
    );
}

#[test]
fn manual_calculation_redaction_hides_other_users_and_request_details() {
    let result = settlement::service::Service::redact(
        json!({
            "allocations": [
                {"user_id": 2, "amount_fen": 12},
                {"user_id": 3, "amount_fen": 34}
            ],
            "bill": {
                "weights": {"3": "9"},
                "matched": [{"usage_id": 1001}],
                "unmatched": [{"row": 7}],
                "outside": [],
                "manual_basis": {"rates": [{"account_id": 100, "rate": "1"}]},
                "manual_weights": {"3": "4"},
                "local_unmatched": [{"usage_id": 1001, "user_id": 3}],
                "manual_summary": {
                    "amount": "4",
                    "accounts": [{"account_id": 100, "local_amount": "4"}]
                }
            }
        }),
        &payer(),
    );

    assert_eq!(
        result["allocations"],
        json!([{"user_id": 3, "amount_fen": 34}])
    );
    assert!(result["bill"].get("weights").is_none());
    assert!(result["bill"].get("matched").is_none());
    assert!(result["bill"].get("unmatched").is_none());
    assert!(result["bill"].get("manual_basis").is_none());
    assert!(result["bill"].get("manual_weights").is_none());
    assert!(result["bill"].get("local_unmatched").is_none());
    assert_eq!(result["bill"]["manual_summary"]["amount"], "4");
    assert!(result["bill"]["manual_summary"].get("accounts").is_none());
}

#[tokio::test]
async fn vendor_manual_preview_enforces_admin_and_selected_window_ids() {
    let harness = Harness::new().await;
    let vendor = create_vendor(
        &harness,
        &[100, 200],
        "6.00",
        "manual-vendor-permission-create",
    )
    .await;
    let period = vendor["period"].clone();
    let denied = harness
        .app
        .service
        .vendor_bill_preview(
            &payer(),
            period["id"].as_i64().unwrap(),
            &json!({
                "version": period["version"],
                "end": END,
                "allocation_mode": "proportional",
                "manual_basis": basis("local_only", &[("100", "1"), ("200", "1")], &[999])
            }),
        )
        .await;
    assert_error(denied, 403, "管理员");

    let invalid = preview_vendor(
        &harness,
        &period,
        basis("local_only", &[("100", "1"), ("200", "1")], &[999]),
        None,
    )
    .await;
    assert_error(invalid, 422, "所选请求不属于本窗口");
}
