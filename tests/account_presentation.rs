#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use settlement::service::Prepared;
use support::legacy::{END, Harness, START, admin, fixture_data};

const RATIO_START: &str = "2025-01-10T00:00:00+00:00";

fn cycle_payload(account_id: i64, name: &str, cost: &str, start: &str) -> Value {
    json!({
        "account_id": account_id,
        "name": name,
        "cost": cost,
        "creditor_id": 2,
        "start": start,
    })
}

async fn create_cycle(
    harness: &Harness,
    account_id: i64,
    name: &str,
    cost: &str,
    start: &str,
    key: &str,
) -> i64 {
    let result = harness
        .app
        .service
        .mutate(
            &admin(),
            key,
            "create",
            &cycle_payload(account_id, name, cost, start),
            Prepared::default(),
            None,
        )
        .await
        .expect("create fixture cycle");
    result["id"].as_i64().unwrap()
}

async fn cycle_row(harness: &Harness, id: i64) -> Value {
    harness
        .rows("cycles")
        .await
        .into_iter()
        .find(|cycle| cycle["id"] == json!(id))
        .unwrap_or_else(|| panic!("cycle {id} not found"))
}

async fn settlement_row(harness: &Harness, cycle_id: i64) -> Value {
    harness
        .rows("settlements")
        .await
        .into_iter()
        .find(|settlement| settlement["cycle_id"] == json!(cycle_id))
        .unwrap_or_else(|| panic!("settlement for cycle {cycle_id} not found"))
}

async fn preview(harness: &Harness, cycle_ids: &[i64]) -> Value {
    harness
        .app
        .service
        .preview(
            &admin(),
            &json!({"ids":cycle_ids,"expense_ids":[],"end":END}),
        )
        .await
        .expect("fixture-backed preview")
}

async fn settle_preview(harness: &Harness, preview_id: &str, key: &str) -> Value {
    let payload = json!({
        "preview_id": preview_id,
        "renewals": [],
        "delete_cycle_ids": [],
    });
    let prepared = harness
        .app
        .service
        .prepare_settlement(&admin(), &payload)
        .await
        .expect("prepare fixture settlement");
    harness
        .app
        .service
        .mutate(&admin(), key, "settle", &payload, prepared, None)
        .await
        .expect("settle fixture preview")
}

fn rename_source_account(source: &mut Value, account_id: i64, name: &str) {
    let account = source["accounts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|account| account["id"] == json!(account_id))
        .unwrap_or_else(|| panic!("source account {account_id} not found"));
    account["name"] = json!(name);
}

#[tokio::test]
async fn source_name_overlays_open_views_and_is_frozen_with_new_orders() {
    let harness = Harness::new().await;
    let cycle_id = create_cycle(
        &harness,
        100,
        "Old profile and cycle alias",
        "10.00",
        START,
        "source-name-cycle",
    )
    .await;

    let current_name = "Upstream name before settlement";
    let mut source = harness.source();
    rename_source_account(&mut source, 100, current_name);
    harness.write_source(&source);
    harness
        .app
        .service
        .sync(true)
        .await
        .expect("sync renamed fixture account");

    let overview = harness.app.service.overview(&admin()).await.unwrap();
    let row = overview["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(100))
        .unwrap();
    assert_eq!(row["name"], current_name);
    assert_eq!(row["cycle"]["id"], cycle_id);
    assert_eq!(row["cycle"]["name"], current_name);

    let preview = preview(&harness, &[cycle_id]).await;
    let calculation = &preview["cycles"][0];
    assert_eq!(calculation["cycle"]["name"], current_name);
    assert_eq!(calculation["display"]["cost_multiplier"], "1.0000");
    assert_eq!(calculation["display"]["settled"], false);

    let history = harness.app.service.history(&admin()).await.unwrap();
    let history_cycle = history["cycles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cycle| cycle["id"] == json!(cycle_id))
        .unwrap();
    assert_eq!(history_cycle["name"], current_name);
    let open_detail = harness
        .app
        .service
        .cycle_detail(&admin(), cycle_id)
        .await
        .unwrap();
    assert_eq!(open_detail["cycle"]["name"], current_name);
    assert_eq!(open_detail["display"]["settled"], false);

    settle_preview(
        &harness,
        preview["id"].as_str().unwrap(),
        "source-name-settle",
    )
    .await;

    let stored_cycle = cycle_row(&harness, cycle_id).await;
    assert_eq!(stored_cycle["name"], current_name);
    let stored_settlement = settlement_row(&harness, cycle_id).await;
    let snapshot: Value =
        serde_json::from_str(stored_settlement["snapshot"].as_str().unwrap()).unwrap();
    assert_eq!(snapshot["cycle"]["name"], current_name);
    assert!(snapshot.get("display").is_none());
    assert_eq!(stored_settlement["digest"], snapshot["digest"]);

    let detail = harness
        .app
        .service
        .cycle_detail(&admin(), cycle_id)
        .await
        .unwrap();
    assert_eq!(detail["cycle"]["name"], current_name);
    assert_eq!(detail["display"]["settled"], true);
    assert_eq!(detail["display"]["cost_multiplier"], "1.0000");

    let orders = harness
        .app
        .service
        .orders(&admin(), "all", "full", None)
        .await
        .unwrap();
    let order_rows = orders["orders"].as_array().unwrap();
    assert!(!order_rows.is_empty());
    assert!(
        order_rows
            .iter()
            .filter(|order| order["cycle_id"] == json!(cycle_id))
            .all(|order| order["account_name"] == current_name)
    );

    let later_name = "Upstream account renamed after settlement";
    let mut source = harness.source();
    rename_source_account(&mut source, 100, later_name);
    harness.write_source(&source);
    harness
        .app
        .service
        .sync(true)
        .await
        .expect("sync later fixture account rename");

    let current_overview = harness.app.service.overview(&admin()).await.unwrap();
    let current_row = current_overview["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(100))
        .unwrap();
    assert_eq!(current_row["name"], later_name);
    assert_eq!(current_row["cycle"]["name"], current_name);
    assert_eq!(cycle_row(&harness, cycle_id).await["name"], current_name);
    assert_eq!(
        settlement_row(&harness, cycle_id).await["snapshot"],
        stored_settlement["snapshot"]
    );

    let unchanged_orders = harness
        .app
        .service
        .orders(&admin(), "all", "full", None)
        .await
        .unwrap();
    assert!(
        unchanged_orders["orders"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|order| order["cycle_id"] == json!(cycle_id))
            .all(|order| order["account_name"] == current_name)
    );
}

fn usage_row(id: i64, account_id: i64, amount: &str, request_id: &str) -> Value {
    json!({
        "id": id,
        "account_id": account_id,
        "user_id": 3,
        "amount": amount,
        "created_at": RATIO_START,
        "request_id": request_id,
        "model": "fixture-model",
        "input_tokens": 10,
        "output_tokens": 20,
        "cache_read_tokens": 0,
        "cache_creation_tokens": 0
    })
}

fn multiplier_fixture() -> Value {
    let mut source = fixture_data();
    source["accounts"].as_array_mut().unwrap().push(json!({
        "id": 600,
        "name": "Fixture ratio half-up",
        "platform": "openai",
        "status": "active",
        "deleted": false,
        "parent_account_id": null,
        "created_at": "2024-12-01T00:00:00+00:00"
    }));
    source["usage"] = json!([
        usage_row(1, 100, "7.315", "ratio-100"),
        usage_row(2, 200, "7.315", "ratio-200"),
        usage_row(3, 300, "200", "ratio-300"),
        usage_row(4, 500, "7.315", "ratio-500"),
        usage_row(5, 600, "8", "ratio-600"),
    ]);
    source
}

fn csv_text(rows: &[(&str, &str, &str, i64, i64, &str)]) -> String {
    let mut text = String::from(
        "Time,Billed Cost,Model,Input Tokens,Output Tokens,Cache Read Tokens,Cache Creation Tokens,Request ID\n",
    );
    for (at, amount, model, input, output, request_id) in rows {
        text.push_str(&format!(
            "{at},{amount},{model},{input},{output},0,0,{request_id}\n"
        ));
    }
    text
}

async fn confirm_matched_only_bill(harness: &Harness, cycle_id: i64) {
    let cycle = cycle_row(harness, cycle_id).await;
    let csv = csv_text(&[
        (RATIO_START, "0.50", "fixture-model", 10, 20, "ratio-500"),
        (
            "2025-01-11T00:00:00+00:00",
            "1.50",
            "unmatched-model",
            99,
            99,
            "unmatched-row",
        ),
    ]);
    let bill = harness
        .app
        .service
        .bill_preview(
            &admin(),
            cycle_id,
            &json!({
                "version":cycle["version"],
                "end":END,
                "csv":csv,
                "file_name":"matched-only.csv",
                "allocation_mode":"matched_only"
            }),
        )
        .await
        .expect("preview matched-only fixture bill");
    assert_eq!(bill["cost_fen"], 200);
    assert_eq!(bill["billable_cost_fen"], 50);
    assert_eq!(bill["can_confirm"], true);

    let payload = json!({
        "version":cycle["version"],
        "preview_id":bill["id"],
        "accept_difference":false,
        "accept_outside":false
    });
    let (prepared_bill, revision) = harness
        .app
        .service
        .prepare_bill(&admin(), cycle_id, &payload)
        .await
        .expect("prepare matched-only fixture bill");
    harness
        .app
        .service
        .mutate(
            &admin(),
            "matched-only-display-confirm",
            &format!("bill-confirm:{cycle_id}"),
            &payload,
            Prepared {
                bill: Some((prepared_bill, revision)),
                ..Prepared::default()
            },
            None,
        )
        .await
        .expect("confirm matched-only fixture bill");
}

#[tokio::test]
async fn calculation_multiplier_uses_billable_cost_rounds_half_up_and_marks_small_or_missing_values()
 {
    let harness = Harness::with_fixture(multiplier_fixture()).await;
    let ids = [
        create_cycle(
            &harness,
            100,
            "Ratio one",
            "1.00",
            RATIO_START,
            "ratio-cycle-100",
        )
        .await,
        create_cycle(
            &harness,
            200,
            "Ratio zero cost",
            "0.00",
            RATIO_START,
            "ratio-cycle-200",
        )
        .await,
        create_cycle(
            &harness,
            300,
            "Ratio below display threshold",
            "0.01",
            RATIO_START,
            "ratio-cycle-300",
        )
        .await,
        create_cycle(
            &harness,
            400,
            "Ratio without consumption",
            "1.00",
            RATIO_START,
            "ratio-cycle-400",
        )
        .await,
        create_cycle(
            &harness,
            500,
            "Matched-only ratio",
            "2.00",
            RATIO_START,
            "ratio-cycle-500",
        )
        .await,
        create_cycle(
            &harness,
            600,
            "Ratio half-up tie",
            "0.01",
            RATIO_START,
            "ratio-cycle-600",
        )
        .await,
    ];
    confirm_matched_only_bill(&harness, ids[4]).await;

    let all_preview = preview(&harness, &ids).await;
    let calculations = all_preview["cycles"].as_array().unwrap();
    let by_cycle = calculations
        .iter()
        .map(|calculation| (calculation["cycle"]["id"].as_i64().unwrap(), calculation))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(by_cycle[&ids[0]]["display"]["cost_multiplier"], "0.1367");
    assert_eq!(by_cycle[&ids[1]]["display"]["cost_multiplier"], "0.0000");
    assert_eq!(by_cycle[&ids[2]]["display"]["cost_multiplier"], "<0.0001");
    assert!(by_cycle[&ids[3]]["display"]["cost_multiplier"].is_null());
    assert_eq!(by_cycle[&ids[4]]["bill"]["billable_cost_fen"], 50);
    assert_eq!(by_cycle[&ids[4]]["cycle"]["cost_fen"], 200);
    assert_eq!(by_cycle[&ids[4]]["display"]["cost_multiplier"], "0.0684");
    assert_eq!(by_cycle[&ids[5]]["display"]["cost_multiplier"], "0.0013");
    assert!(
        calculations
            .iter()
            .all(|calculation| calculation["display"]["settled"] == false)
    );

    let detail = harness
        .app
        .service
        .cycle_detail(&admin(), ids[0])
        .await
        .unwrap();
    assert_eq!(detail["display"]["cost_multiplier"], "0.1367");
    assert_eq!(detail["display"]["settled"], false);

    let single_preview = preview(&harness, &[ids[0]]).await;
    settle_preview(
        &harness,
        single_preview["id"].as_str().unwrap(),
        "ratio-cycle-settle",
    )
    .await;
    let settlement = settlement_row(&harness, ids[0]).await;
    let snapshot: Value = serde_json::from_str(settlement["snapshot"].as_str().unwrap()).unwrap();
    assert!(snapshot.get("display").is_none());
    assert_eq!(settlement["digest"], snapshot["digest"]);
    let settled_detail = harness
        .app
        .service
        .cycle_detail(&admin(), ids[0])
        .await
        .unwrap();
    assert_eq!(settled_detail["display"]["cost_multiplier"], "0.1367");
    assert_eq!(settled_detail["display"]["settled"], true);
    assert_eq!(
        settlement_row(&harness, ids[0]).await["snapshot"],
        settlement["snapshot"]
    );
}
