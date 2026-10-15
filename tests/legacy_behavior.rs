//! Business-level migration of the fixed Python test contracts.
//!
//! Coverage map (Rust test -> Python references):
//! - fixed_expense_allocation_and_custom_weights_match_legacy_behavior ->
//!   test_equal_allocation_assigns_integer_remainder_deterministically,
//!   test_custom_allocation_requires_one_hundred_percent_and_unique_users
//! - expense_creditor_share_is_self_covered_and_never_becomes_an_order ->
//!   test_creditor_share_is_recorded_as_self_covered_without_an_order
//! - periodic_and_fixed_expense_orders_merge_and_net_without_losing_source_rows ->
//!   test_periodic_and_fixed_expense_orders_can_merge_and_be_paid_by_netting,
//!   test_pending_batch_requires_netting_and_preserves_original_items
//! - fixed_expense_void_splits_batch_but_preserves_cycle_order_and_reopens_expense ->
//!   test_fixed_expense_split_then_void_restores_expense_and_keeps_cycle_orders,
//!   test_void_splits_cross_settlement_batch_and_preserves_other_pending_order
//! - regular_user_views_redact_expense_members_and_scope_orders ->
//!   test_expense_visibility_redacts_members_and_hides_unrelated_users,
//!   test_regular_users_only_receive_their_own_orders_allocations_and_notifications
//! - mutation_idempotency_replays_only_the_original_payload ->
//!   test_idempotency_replays_without_duplicates_and_rejects_changed_payload
//! - estimated_cost_cannot_settle_and_editing_kind_invalidates_the_old_preview ->
//!   test_estimated_cost_is_visible_but_blocks_preview_settlement,
//!   test_editing_cost_kind_increments_version_and_invalidates_old_preview,
//!   test_legacy_cycle_payload_defaults_to_exact_and_can_settle
//! - historical_creditors_survive_open_cycle_edits_but_cannot_be_reassigned ->
//!   test_open_cycle_keeps_historical_creditor_for_audited_cost_edit_then_locks_after_settlement,
//!   test_historical_creditor_exception_does_not_allow_switch_to_another_unavailable_user,
//!   test_unavailable_creditor_cannot_be_assigned_to_a_new_cycle
//! - estimate_confirmation_and_earliest_usage_renewal_preserve_cycle_boundaries ->
//!   test_date_estimate_preview_requires_confirmation_and_invalidates_old_preview,
//!   test_provided_time_confirmation_persists_anchor_and_invalidates_old_version,
//!   test_earliest_usage_starts_at_captured_record_and_renewal_is_continuous,
//!   test_cycle_boundary_is_half_open_and_renewal_starts_at_exact_end
//! - paid_upgrade_locks_start_and_excludes_prior_usage ->
//!   test_upgrade_paid_starts_a_locked_cycle_and_excludes_prior_usage,
//!   test_upgrade_is_idempotent_and_free_accounts_cannot_use_generic_cycle_create,
//!   test_locked_upgrade_start_cannot_move_but_cost_can_change,
//!   test_upgrade_requires_current_free_profile_version_and_active_creditor
//! - netting_keeps_original_cents_orders_and_pair_scoped_permissions ->
//!   test_two_way_cents_netting_records_original_orders_and_survives_restart,
//!   test_three_person_balances_stay_pair_scoped_and_user_scopes_are_isolated
//! - zero_net_can_be_confirmed_without_a_transfer_timestamp ->
//!   test_zero_net_can_be_confirmed_by_either_participant_without_submitted_timestamp
//! - concurrent_netting_payment_replays_one_committed_response ->
//!   test_concurrent_payment_is_idempotent_and_history_survives_restart,
//!   test_concurrent_idempotent_retries_share_one_committed_response
//! - failed_netting_payment_rolls_back_snapshot_orders_and_request_key ->
//!   test_mid_transaction_failure_rolls_back_payment_snapshot_and_order_updates
//! - bill_import_matches_request_id_and_five_second_token_fallback_before_rounding ->
//!   test_request_id_and_unique_model_token_match_with_five_second_window,
//!   test_csv_sum_keeps_eight_decimal_places_until_final_fen_rounding
//! - bill_import_outside_confirmation_duplicate_history_and_stale_preview ->
//!   test_outside_rows_need_explicit_confirmation,
//!   test_duplicate_confirmation_is_idempotent_and_replacement_keeps_history,
//!   test_old_preview_is_rejected_after_cutoff_or_cycle_data_changes
//! - bill_import_matched_only_allocates_only_matched_cost ->
//!   test_matched_only_rounds_billable_cost_and_assigns_only_matched_amount,
//!   test_regular_user_cannot_read_raw_bill_matching_data
//! - matched_only_bill_import_still_requires_outside_confirmation_and_positive_match ->
//!   test_matched_only_still_requires_outside_confirmation,
//!   test_matched_only_without_positive_match_is_still_rejected
//!
//! HTTP workflow/session contracts and the remaining legacy areas are tracked
//! separately; this test binary exercises Service through isolated fixture-backed
//! stores and never contacts the production source or ledger.

mod support;

use std::sync::Arc;

use serde_json::{Value, json};
use settlement::{
    error::Result,
    http::App,
    service::{Prepared, Service},
    time,
};
use support::legacy::{
    END, Harness, NEXT_END, START, admin, creditor, fixture_data, other, payer, stranger,
};
use tokio::sync::Barrier;

fn cycle_payload(account_id: i64, cost: &str, creditor_id: i64, start: &str) -> Value {
    json!({
        "account_id": account_id,
        "name": format!("Fixture account {account_id}"),
        "cost": cost,
        "creditor_id": creditor_id,
        "start": start,
    })
}

async fn create_cycle(harness: &Harness, payload: &Value, key: &str) -> Value {
    harness
        .app
        .service
        .mutate(&admin(), key, "create", payload, Prepared::default(), None)
        .await
        .expect("create fixture cycle")
}

async fn cycle_row(harness: &Harness, id: i64) -> Value {
    harness
        .rows("cycles")
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("cycle {id} not found"))
}

fn cost_string(fen: i64) -> String {
    format!("{}.{:02}", fen / 100, fen % 100)
}

async fn edit_cycle(
    harness: &Harness,
    id: i64,
    actor: &Value,
    key: &str,
    changes: Value,
) -> Result<Value> {
    let current = cycle_row(harness, id).await;
    let mut payload = json!({
        "account_id": current["account_id"],
        "name": current["name"],
        "cost": cost_string(current["cost_fen"].as_i64().unwrap()),
        "creditor_id": current["creditor_id"],
        "start": current["start"],
        "start_mode": current["start_mode"],
        "cost_kind": current["cost_kind"],
        "version": current["version"],
    });
    for (key, value) in changes.as_object().unwrap() {
        payload[key] = value.clone();
    }
    harness
        .app
        .service
        .mutate(
            actor,
            key,
            &format!("edit:{id}"),
            &payload,
            Prepared::default(),
            None,
        )
        .await
}

async fn create_expense(harness: &Harness, payload: &Value, key: &str) -> Result<Value> {
    harness
        .app
        .service
        .mutate(
            &admin(),
            key,
            "create-expense",
            payload,
            Prepared::default(),
            None,
        )
        .await
}

fn equal_expense_payload(name: &str, cost: &str, creditor_id: i64, members: &[i64]) -> Value {
    json!({
        "name": name,
        "cost": cost,
        "incurred_on": "2025-01-20",
        "creditor_id": creditor_id,
        "allocation_mode": "equal",
        "members": members.iter().map(|id| json!({"user_id":id,"percent":null})).collect::<Vec<_>>(),
    })
}

async fn preview(harness: &Harness, cycle_ids: &[i64], expense_ids: &[i64], end: &str) -> Value {
    let data = json!({"ids":cycle_ids,"expense_ids":expense_ids,"end":end});
    harness
        .app
        .service
        .preview(&admin(), &data)
        .await
        .expect("fixture-backed preview")
}

async fn settle_on(
    app: &App,
    actor: &Value,
    preview_id: &str,
    key: &str,
    renewals: Value,
) -> Result<Value> {
    let data = json!({
        "preview_id": preview_id,
        "renewals": renewals,
        "delete_cycle_ids": [],
    });
    let prepared = app.service.prepare_settlement(actor, &data).await?;
    app.service
        .mutate(actor, key, "settle", &data, prepared, None)
        .await
}

async fn settle_preview(
    harness: &Harness,
    preview_id: &str,
    key: &str,
    renewals: Value,
) -> Result<Value> {
    settle_on(&harness.app, &admin(), preview_id, key, renewals).await
}

fn assert_error<T: std::fmt::Debug>(result: Result<T>, status: u16, message: &str) {
    let error = result.expect_err("operation should fail with the legacy boundary");
    assert_eq!(error.status, status, "unexpected service error: {error}");
    assert!(
        error.message.contains(message),
        "expected error containing {message:?}, got {:?}",
        error.message
    );
}

async fn merge_orders(harness: &Harness, actor: &Value, ids: Vec<i64>, key: &str) -> Value {
    let payload = json!({"ids":ids});
    harness
        .app
        .service
        .mutate(actor, key, "merge", &payload, Prepared::default(), None)
        .await
        .expect("merge compatible pending orders")
}

async fn transition(
    harness: &Harness,
    actor: &Value,
    order: &Value,
    action: &str,
    key: &str,
) -> Result<Value> {
    let payload = json!({"action":action,"version":order["version"]});
    harness
        .app
        .service
        .mutate(
            actor,
            key,
            &format!("transition:{}", order["id"].as_i64().unwrap()),
            &payload,
            Prepared::default(),
            None,
        )
        .await
}

async fn netting_payload(harness: &Harness, actor: &Value, user_a: i64, user_b: i64) -> Value {
    let balance = harness
        .app
        .service
        .netting_balance(actor, user_a, user_b)
        .await
        .expect("pair balance");
    json!({"user_a":user_a,"user_b":user_b,"digest":balance["digest"]})
}

async fn pay_netting_on(app: &App, actor: &Value, payload: &Value, key: &str) -> Result<Value> {
    app.service
        .mutate(
            actor,
            key,
            "netting-payment",
            payload,
            Prepared::default(),
            None,
        )
        .await
}

async fn netting_balances(harness: &Harness, actor: &Value, scope: &str) -> Result<Vec<Value>> {
    let mut db = harness.app.service.store.read().await?;
    let balances = Service::balances(&mut db, actor, scope).await?;
    db.commit().await?;
    Ok(balances)
}

fn allocations(result: &Value) -> std::collections::BTreeMap<i64, &Value> {
    result["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["user_id"].as_i64().unwrap(), row))
        .collect()
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

async fn bill_preview(harness: &Harness, cycle_id: i64, csv: &str, file_name: &str) -> Value {
    bill_preview_with_mode(harness, cycle_id, csv, file_name, None).await
}

async fn bill_preview_with_mode(
    harness: &Harness,
    cycle_id: i64,
    csv: &str,
    file_name: &str,
    allocation_mode: Option<&str>,
) -> Value {
    let cycle = cycle_row(harness, cycle_id).await;
    let mut payload = json!({
        "version":cycle["version"],
        "end":END,
        "csv":csv,
        "file_name":file_name,
    });
    if let Some(mode) = allocation_mode {
        payload["allocation_mode"] = json!(mode);
    }
    harness
        .app
        .service
        .bill_preview(&admin(), cycle_id, &payload)
        .await
        .expect("synthetic bill preview")
}

fn bill_payload(cycle_version: Value, preview_id: &Value, accept_outside: bool) -> Value {
    json!({
        "version":cycle_version,
        "preview_id":preview_id,
        "accept_difference":false,
        "accept_outside":accept_outside,
    })
}

async fn prepare_bill(app: &App, cycle_id: i64, payload: &Value) -> Result<Prepared> {
    let prepared = app
        .service
        .prepare_bill(&admin(), cycle_id, payload)
        .await?;
    Ok(Prepared {
        bill: Some(prepared),
        ..Prepared::default()
    })
}

async fn confirm_bill_on(
    app: &App,
    cycle_id: i64,
    payload: &Value,
    prepared: Prepared,
    key: &str,
) -> Result<Value> {
    app.service
        .mutate(
            &admin(),
            key,
            &format!("bill-confirm:{cycle_id}"),
            payload,
            prepared,
            None,
        )
        .await
}

async fn confirm_bill(
    harness: &Harness,
    cycle_id: i64,
    preview: &Value,
    accept_outside: bool,
    key: &str,
) -> Result<Value> {
    let cycle = cycle_row(harness, cycle_id).await;
    let payload = bill_payload(cycle["version"].clone(), &preview["id"], accept_outside);
    let prepared = prepare_bill(&harness.app, cycle_id, &payload).await?;
    confirm_bill_on(&harness.app, cycle_id, &payload, prepared, key).await
}

#[tokio::test]
async fn mutation_idempotency_replays_only_the_original_payload() {
    let harness = Harness::new().await;
    let payload = cycle_payload(100, "10.00", 2, START);
    let first = create_cycle(&harness, &payload, "cycle-idempotency-key").await;
    let replay = create_cycle(&harness, &payload, "cycle-idempotency-key").await;
    assert_eq!(first, replay);

    let changed = cycle_payload(100, "11.00", 2, START);
    let result = harness
        .app
        .service
        .mutate(
            &admin(),
            "cycle-idempotency-key",
            "create",
            &changed,
            Prepared::default(),
            None,
        )
        .await;
    assert_error(result, 409, "其他请求");
    assert_eq!(harness.rows("cycles").await.len(), 1);
    assert_eq!(harness.rows("requests").await.len(), 1);
    assert_eq!(harness.rows("audit").await.len(), 1);
}

#[tokio::test]
async fn fixed_expense_allocation_and_custom_weights_match_legacy_behavior() {
    let harness = Harness::new().await;
    let equal = create_expense(
        &harness,
        &equal_expense_payload("Three person split", "10.00", 2, &[3, 4, 5]),
        "expense-equal",
    )
    .await
    .unwrap();
    let equal_result = harness
        .app
        .service
        .expenses(&admin(), Some(equal["id"].as_i64().unwrap()))
        .await
        .unwrap();
    let equal_shares = allocations(&equal_result);
    assert_eq!(equal_shares[&3]["amount_fen"], 334);
    assert_eq!(equal_shares[&4]["amount_fen"], 333);
    assert_eq!(equal_shares[&5]["amount_fen"], 333);
    assert_eq!(
        equal_shares
            .values()
            .map(|share| share["amount_fen"].as_i64().unwrap())
            .sum::<i64>(),
        1000
    );

    let custom_payload = json!({
        "name":"Custom split",
        "cost":"10.00",
        "incurred_on":"2025-01-20",
        "creditor_id":2,
        "allocation_mode":"custom",
        "members":[
            {"user_id":3,"percent":"33.33"},
            {"user_id":4,"percent":"66.67"}
        ]
    });
    let custom = create_expense(&harness, &custom_payload, "expense-custom")
        .await
        .unwrap();
    let custom_result = harness
        .app
        .service
        .expenses(&admin(), Some(custom["id"].as_i64().unwrap()))
        .await
        .unwrap();
    let custom_shares = allocations(&custom_result);
    assert_eq!(custom_shares[&3]["amount_fen"], 333);
    assert_eq!(custom_shares[&4]["amount_fen"], 667);

    for (key, members, expected) in [
        (
            "expense-custom-total-invalid",
            json!([
                {"user_id":3,"percent":"20.00"},
                {"user_id":4,"percent":"20.00"}
            ]),
            "100.00%",
        ),
        (
            "expense-custom-duplicate-user",
            json!([
                {"user_id":3,"percent":"50.00"},
                {"user_id":3,"percent":"50.00"}
            ]),
            "不同",
        ),
    ] {
        let invalid = json!({
            "name":"Invalid custom split",
            "cost":"10.00",
            "incurred_on":"2025-01-20",
            "creditor_id":2,
            "allocation_mode":"custom",
            "members":members,
        });
        let result = create_expense(&harness, &invalid, key).await;
        assert_error(result, 422, expected);
    }
    assert_eq!(harness.rows("expenses").await.len(), 2);
}

#[tokio::test]
async fn expense_creditor_share_is_self_covered_and_never_becomes_an_order() {
    let harness = Harness::new().await;
    let expense = create_expense(
        &harness,
        &equal_expense_payload("Creditor share", "10.00", 3, &[3, 4]),
        "expense-self-covered",
    )
    .await
    .unwrap();
    let id = expense["id"].as_i64().unwrap();
    let preview = preview(&harness, &[], &[id], END).await;
    let shares = allocations(&preview["expenses"][0]);
    assert_eq!(shares[&3]["amount_fen"], 500);
    assert_eq!(shares[&3]["self_covered"], true);

    settle_preview(
        &harness,
        preview["id"].as_str().unwrap(),
        "expense-self-settle",
        json!([]),
    )
    .await
    .unwrap();
    let orders = harness.rows("orders").await;
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0]["expense_id"], id);
    assert_eq!(orders[0]["payer_id"], 4);
    assert_eq!(orders[0]["creditor_id"], 3);
    assert_eq!(orders[0]["amount_fen"], 500);
}

#[tokio::test]
async fn periodic_and_fixed_expense_orders_merge_and_net_without_losing_source_rows() {
    let harness = Harness::new().await;
    let cycle = create_cycle(
        &harness,
        &cycle_payload(100, "100.00", 2, START),
        "periodic-cycle",
    )
    .await;
    let expense = create_expense(
        &harness,
        &equal_expense_payload("Fixed lunch", "10.00", 2, &[3, 4]),
        "periodic-fixed-expense",
    )
    .await
    .unwrap();
    let cycle_id = cycle["id"].as_i64().unwrap();
    let expense_id = expense["id"].as_i64().unwrap();
    let preview = preview(&harness, &[cycle_id], &[expense_id], END).await;
    settle_preview(
        &harness,
        preview["id"].as_str().unwrap(),
        "cycle-expense-settle",
        json!([]),
    )
    .await
    .unwrap();

    let mut payer_orders = harness
        .rows("orders")
        .await
        .into_iter()
        .filter(|row| row["payer_id"] == 3)
        .collect::<Vec<_>>();
    payer_orders.sort_by_key(|row| row["id"].as_i64().unwrap());
    assert_eq!(payer_orders.len(), 2);
    assert!(payer_orders.iter().any(|row| row["cycle_id"] == cycle_id));
    assert!(
        payer_orders
            .iter()
            .any(|row| row["expense_id"] == expense_id)
    );
    assert!(payer_orders.iter().all(|row| row["creditor_id"] == 2));
    assert_eq!(
        payer_orders
            .iter()
            .map(|row| row["amount_fen"].as_i64().unwrap())
            .sum::<i64>(),
        5500
    );

    let ids = payer_orders
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    let batch = merge_orders(&harness, &payer(), ids, "cycle-expense-merge").await;
    let merged = harness.rows("orders").await;
    assert!(
        merged
            .iter()
            .filter(|row| row["payer_id"] == 3)
            .all(|row| row["batch_id"] == batch["id"])
    );
    let merged_payer_order = merged
        .iter()
        .find(|row| row["payer_id"] == 3)
        .expect("payer order remains visible after merge");
    assert_error(
        transition(
            &harness,
            &payer(),
            merged_payer_order,
            "submit",
            "cycle-expense-submit",
        )
        .await,
        409,
        "净额",
    );

    let payment = netting_payload(&harness, &payer(), 2, 3).await;
    pay_netting_on(&harness.app, &payer(), &payment, "cycle-expense-netting")
        .await
        .unwrap();
    let after = harness.rows("orders").await;
    let payer_orders = after
        .iter()
        .filter(|row| row["payer_id"] == 3)
        .collect::<Vec<_>>();
    assert_eq!(payer_orders.len(), 2);
    assert!(payer_orders.iter().all(|row| row["status"] == "paid"));
    assert!(
        payer_orders
            .iter()
            .all(|row| row["batch_id"] == batch["id"])
    );
    assert_eq!(harness.rows("batches").await[0]["status"], "paid");
}

#[tokio::test]
async fn fixed_expense_void_splits_batch_but_preserves_cycle_order_and_reopens_expense() {
    let harness = Harness::new().await;
    let cycle = create_cycle(
        &harness,
        &cycle_payload(100, "100.00", 2, START),
        "void-cycle",
    )
    .await;
    let expense = create_expense(
        &harness,
        &equal_expense_payload("Void me", "10.00", 2, &[3, 4]),
        "void-expense",
    )
    .await
    .unwrap();
    let cycle_id = cycle["id"].as_i64().unwrap();
    let expense_id = expense["id"].as_i64().unwrap();
    let preview = preview(&harness, &[cycle_id], &[expense_id], END).await;
    settle_preview(
        &harness,
        preview["id"].as_str().unwrap(),
        "void-first-settle",
        json!([]),
    )
    .await
    .unwrap();

    let rows = harness.rows("orders").await;
    let cycle_order = rows
        .iter()
        .find(|row| row["payer_id"] == 3 && row["cycle_id"] == cycle_id)
        .unwrap()
        .clone();
    let expense_order = rows
        .iter()
        .find(|row| row["payer_id"] == 3 && row["expense_id"] == expense_id)
        .unwrap()
        .clone();
    merge_orders(
        &harness,
        &payer(),
        vec![
            cycle_order["id"].as_i64().unwrap(),
            expense_order["id"].as_i64().unwrap(),
        ],
        "void-merge",
    )
    .await;
    let merged_cycle_order = harness
        .rows("orders")
        .await
        .into_iter()
        .find(|row| row["id"] == cycle_order["id"])
        .unwrap();
    transition(
        &harness,
        &payer(),
        &merged_cycle_order,
        "split",
        "void-split",
    )
    .await
    .unwrap();
    assert_eq!(harness.rows("batches").await[0]["status"], "split");
    assert!(
        harness
            .rows("orders")
            .await
            .iter()
            .all(|row| row["batch_id"].is_null())
    );

    let expense_settlement = harness
        .rows("settlements")
        .await
        .into_iter()
        .find(|row| row["expense_id"] == expense_id)
        .unwrap();
    let void_payload = json!({});
    harness
        .app
        .service
        .mutate(
            &admin(),
            "void-expense-settlement",
            &format!("void:{}", expense_settlement["id"].as_i64().unwrap()),
            &void_payload,
            Prepared::default(),
            None,
        )
        .await
        .unwrap();

    let expense_after = harness
        .app
        .service
        .expenses(&admin(), Some(expense_id))
        .await
        .unwrap();
    assert_eq!(expense_after["expense"]["status"], "open");
    assert!(expense_after["expense"]["settlement_id"].is_null());
    let orders = harness.rows("orders").await;
    assert_eq!(
        orders
            .iter()
            .find(|row| row["id"] == expense_order["id"])
            .unwrap()["status"],
        "void"
    );
    assert_eq!(
        orders
            .iter()
            .find(|row| row["id"] == cycle_order["id"])
            .unwrap()["status"],
        "pending"
    );
    assert_eq!(cycle_row(&harness, cycle_id).await["status"], "settled");
}

#[tokio::test]
async fn regular_user_views_redact_expense_members_and_scope_orders() {
    let harness = Harness::new().await;
    let expense = create_expense(
        &harness,
        &equal_expense_payload("Private member list", "10.00", 2, &[3, 4]),
        "permission-expense",
    )
    .await
    .unwrap();
    let expense_id = expense["id"].as_i64().unwrap();
    let payer_view = harness
        .app
        .service
        .expenses(&payer(), Some(expense_id))
        .await
        .unwrap();
    assert!(payer_view["expense"].get("members").is_none());
    assert!(payer_view.get("records").is_none());
    assert_eq!(
        allocations(&payer_view).keys().copied().collect::<Vec<_>>(),
        vec![3]
    );
    assert!(
        harness
            .app
            .service
            .expenses(&stranger(), None)
            .await
            .unwrap()["expenses"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_error(
        harness
            .app
            .service
            .expenses(&stranger(), Some(expense_id))
            .await,
        404,
        "费用",
    );

    let preview = preview(&harness, &[], &[expense_id], END).await;
    settle_preview(
        &harness,
        preview["id"].as_str().unwrap(),
        "permission-settle",
        json!([]),
    )
    .await
    .unwrap();
    let mine = harness
        .app
        .service
        .orders(&payer(), "mine", "full", None)
        .await
        .unwrap()["orders"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(mine.len(), 1);
    assert!(mine.iter().all(|row| row["payer_id"] == 3));
    let other_orders = harness
        .app
        .service
        .orders(&other(), "mine", "full", None)
        .await
        .unwrap()["orders"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(other_orders.len(), 1);
    assert_eq!(other_orders[0]["payer_id"], 4);
    assert!(
        harness
            .app
            .service
            .orders(&payer(), "receivable", "full", None)
            .await
            .unwrap()["orders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let receivable = harness
        .app
        .service
        .orders(&creditor(), "receivable", "full", None)
        .await
        .unwrap()["orders"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(receivable.len(), 2);
    let compact = harness
        .app
        .service
        .orders(&creditor(), "receivable", "compact", Some("pending"))
        .await
        .unwrap()["orders"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(compact.len(), 2);
    let compact_fields = compact[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        compact_fields,
        std::collections::BTreeSet::from([
            "id",
            "batch_id",
            "source_name",
            "payer_name",
            "amount_fen",
            "status",
        ])
    );
    assert_eq!(
        harness
            .app
            .service
            .orders(&admin(), "all", "full", None)
            .await
            .unwrap()["orders"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        harness
            .app
            .service
            .orders(&stranger(), "mine", "full", None)
            .await
            .unwrap()["orders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_error(
        harness
            .app
            .service
            .orders(&payer(), "all", "full", None)
            .await,
        403,
        "管理员",
    );
    assert_error(
        harness
            .app
            .service
            .detail(&stranger(), mine[0]["id"].as_i64().unwrap())
            .await,
        404,
        "订单",
    );
    let detail = harness
        .app
        .service
        .detail(&payer(), mine[0]["id"].as_i64().unwrap())
        .await
        .unwrap();
    assert_eq!(
        allocations(&detail["snapshot"])
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(
        harness.app.service.inbox(&payer()).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["user_id"] == 3)
    );
    assert!(
        harness.app.service.inbox(&stranger()).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn estimated_cost_cannot_settle_and_editing_kind_invalidates_the_old_preview() {
    let harness = Harness::new().await;
    let mut payload = cycle_payload(100, "100.00", 2, START);
    payload["cost_kind"] = json!("estimated");
    let created = create_cycle(&harness, &payload, "estimated-cost-cycle").await;
    let id = created["id"].as_i64().unwrap();
    assert_eq!(cycle_row(&harness, id).await["cost_kind"], "estimated");

    let old = preview(&harness, &[id], &[], END).await;
    let estimate = &old["cycles"][0];
    assert_eq!(estimate["cycle"]["cost_kind"], "estimated");
    assert_eq!(estimate["can_settle"], false);
    assert!(
        estimate["cost_warning"]
            .as_str()
            .unwrap()
            .contains("准确成本")
    );
    assert_error(
        settle_preview(
            &harness,
            old["id"].as_str().unwrap(),
            "estimated-cost-settle",
            json!([]),
        )
        .await,
        409,
        "成本",
    );
    assert!(harness.rows("settlements").await.is_empty());
    assert_eq!(cycle_row(&harness, id).await["status"], "open");

    let edited = edit_cycle(
        &harness,
        id,
        &admin(),
        "estimated-cost-to-exact",
        json!({"cost_kind":"exact"}),
    )
    .await
    .unwrap();
    assert_eq!(edited["version"], 2);
    assert_error(
        settle_preview(
            &harness,
            old["id"].as_str().unwrap(),
            "estimated-cost-stale-preview",
            json!([]),
        )
        .await,
        409,
        "修改",
    );
    let fresh = preview(&harness, &[id], &[], END).await;
    assert_eq!(fresh["cycles"][0]["cycle"]["version"], 2);
    assert_eq!(fresh["cycles"][0]["cycle"]["cost_kind"], "exact");
    assert_eq!(fresh["cycles"][0]["can_settle"], true);
    settle_preview(
        &harness,
        fresh["id"].as_str().unwrap(),
        "exact-cost-settle",
        json!([]),
    )
    .await
    .unwrap();

    let legacy = create_cycle(
        &harness,
        &cycle_payload(200, "20.00", 2, END),
        "legacy-cost-default",
    )
    .await;
    let legacy_row = cycle_row(&harness, legacy["id"].as_i64().unwrap()).await;
    assert_eq!(legacy_row["cost_kind"], "exact");
    assert_eq!(legacy_row["cost_source"], "manual");
    let legacy_id = legacy["id"].as_i64().unwrap();
    let legacy_preview = preview(&harness, &[legacy_id], &[], NEXT_END).await;
    assert_eq!(legacy_preview["cycles"][0]["can_settle"], true);
    settle_preview(
        &harness,
        legacy_preview["id"].as_str().unwrap(),
        "legacy-exact-settle",
        json!([]),
    )
    .await
    .unwrap();
    assert_eq!(cycle_row(&harness, legacy_id).await["status"], "settled");
}

#[tokio::test]
async fn historical_creditors_survive_open_cycle_edits_but_cannot_be_reassigned() {
    for unavailable in ["inactive", "deleted"] {
        let harness = Harness::new().await;
        let created = create_cycle(
            &harness,
            &cycle_payload(100, "10.00", 2, START),
            &format!("historical-creditor-cycle-{unavailable}"),
        )
        .await;
        let id = created["id"].as_i64().unwrap();
        let old_preview = preview(&harness, &[id], &[], END).await;

        let mut source = harness.source();
        for user in source["users"].as_array_mut().unwrap() {
            if user["id"] == 2 || user["id"] == 4 {
                if unavailable == "inactive" {
                    user["status"] = json!("disabled");
                } else {
                    user["deleted"] = json!(true);
                }
            }
        }
        harness.write_source(&source);
        harness.app.service.sync(true).await.unwrap();
        let users = harness.app.service.catalog(&admin()).await.unwrap()["users"]
            .as_array()
            .unwrap()
            .iter()
            .map(|user| user["id"].as_i64().unwrap())
            .collect::<Vec<_>>();
        assert!(!users.contains(&2) && !users.contains(&4));

        let edit = edit_cycle(
            &harness,
            id,
            &admin(),
            &format!("historical-creditor-edit-{unavailable}"),
            json!({"cost":"35.01"}),
        )
        .await
        .unwrap();
        assert_eq!(edit["version"], 2);
        let updated = cycle_row(&harness, id).await;
        assert_eq!(updated["creditor_id"], 2);
        assert_eq!(updated["cost_fen"], 3501);

        let audit = harness.rows("audit").await;
        let edit_audit = audit
            .iter()
            .find(|row| row["action"] == "edit_cycle")
            .unwrap();
        let detail: Value = serde_json::from_str(edit_audit["payload"].as_str().unwrap()).unwrap();
        assert_eq!(detail["before"]["cost_fen"], 1000);
        assert_eq!(detail["after"]["cost_fen"], 3501);
        assert_eq!(detail["before"]["creditor_id"], 2);
        assert_eq!(detail["after"]["creditor_id"], 2);
        assert_error(
            settle_preview(
                &harness,
                old_preview["id"].as_str().unwrap(),
                &format!("historical-creditor-stale-{unavailable}"),
                json!([]),
            )
            .await,
            409,
            "修改",
        );

        let before_failed_changes = [
            harness.rows("cycles").await,
            harness.rows("profiles").await,
            harness.rows("audit").await,
            harness.rows("requests").await,
        ];
        assert_error(
            edit_cycle(
                &harness,
                id,
                &admin(),
                &format!("historical-creditor-switch-{unavailable}"),
                json!({"creditor_id":4}),
            )
            .await,
            404,
            "垫付人不可用",
        );
        assert_eq!(
            before_failed_changes,
            [
                harness.rows("cycles").await,
                harness.rows("profiles").await,
                harness.rows("audit").await,
                harness.rows("requests").await,
            ]
        );
        let new_cycle = cycle_payload(200, "1.00", 2, END);
        let result = harness
            .app
            .service
            .mutate(
                &admin(),
                &format!("historical-creditor-new-cycle-{unavailable}"),
                "create",
                &new_cycle,
                Prepared::default(),
                None,
            )
            .await;
        assert_error(result, 404, "垫付人不可用");

        let current_preview = preview(&harness, &[id], &[], END).await;
        settle_preview(
            &harness,
            current_preview["id"].as_str().unwrap(),
            &format!("historical-creditor-settle-{unavailable}"),
            json!([]),
        )
        .await
        .unwrap();
        let locked_rows = [
            harness.rows("cycles").await,
            harness.rows("orders").await,
            harness.rows("settlements").await,
            harness.rows("audit").await,
        ];
        assert_error(
            edit_cycle(
                &harness,
                id,
                &admin(),
                &format!("historical-creditor-edit-settled-{unavailable}"),
                json!({"cost":"40.00"}),
            )
            .await,
            409,
            "已出单成本已锁定",
        );
        assert_eq!(
            locked_rows,
            [
                harness.rows("cycles").await,
                harness.rows("orders").await,
                harness.rows("settlements").await,
                harness.rows("audit").await,
            ]
        );
    }
}

#[tokio::test]
async fn estimate_confirmation_and_earliest_usage_renewal_preserve_cycle_boundaries() {
    let harness = Harness::new().await;
    let estimated = cycle_payload(100, "100.00", 2, "2025-01-10T14:00:00+08:00");
    let mut estimated = estimated;
    estimated["start_mode"] = json!("date_estimate");
    let created = create_cycle(&harness, &estimated, "date-estimated-cycle").await;
    let id = created["id"].as_i64().unwrap();
    let initial = cycle_row(&harness, id).await;
    assert_eq!(initial["start"], "2025-01-09T16:00:00.000000+00:00");
    assert_eq!(initial["start_confirmed"], 0);

    let old_preview = preview(&harness, &[id], &[], END).await;
    assert_eq!(old_preview["cycles"][0]["can_settle"], false);
    assert_eq!(old_preview["cycles"][0]["data_quality"], "estimated");
    let confirmation = json!({
        "version":initial["version"],
        "start":"2025-01-10T08:00:00+00:00",
        "method":"provided_time",
    });
    harness
        .app
        .service
        .mutate(
            &admin(),
            "confirm-estimated-start",
            &format!("start-confirm:{id}"),
            &confirmation,
            Prepared::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(cycle_row(&harness, id).await["start_mode"], "exact");
    assert_error(
        settle_preview(
            &harness,
            old_preview["id"].as_str().unwrap(),
            "estimated-start-old-preview",
            json!([]),
        )
        .await,
        409,
        "修改",
    );
    let fresh = preview(&harness, &[id], &[], END).await;
    assert_eq!(fresh["cycles"][0]["can_settle"], true);
    settle_preview(
        &harness,
        fresh["id"].as_str().unwrap(),
        "estimated-start-settle",
        json!([]),
    )
    .await
    .unwrap();

    let earliest = json!({
        "account_id":200,
        "name":"Start at first captured use",
        "cost":"20.00",
        "creditor_id":2,
        "start":null,
        "start_mode":"earliest_usage",
    });
    let created = create_cycle(&harness, &earliest, "earliest-usage-cycle").await;
    let earliest_id = created["id"].as_i64().unwrap();
    let current = cycle_row(&harness, earliest_id).await;
    assert_eq!(current["start"], "2025-01-05T00:00:00.000000+00:00");
    assert_eq!(current["start_source_usage_id"], 6);
    let renewal_preview = preview(&harness, &[earliest_id], &[], END).await;
    let settled = settle_preview(
        &harness,
        renewal_preview["id"].as_str().unwrap(),
        "earliest-usage-renewal",
        json!([{"cycle_id":earliest_id,"cost":"30.00","creditor_id":2}]),
    )
    .await
    .unwrap();
    let next_id = settled["settlements"][0]["next_cycle"].as_i64().unwrap();
    let next = cycle_row(&harness, next_id).await;
    assert_eq!(next["start"], cycle_row(&harness, earliest_id).await["end"]);
    assert_eq!(next["renewed_from_cycle_id"], earliest_id);
    assert_eq!(
        preview(&harness, &[next_id], &[], NEXT_END).await["cycles"][0]["record_count"],
        0
    );
}

#[tokio::test]
async fn paid_upgrade_locks_start_and_excludes_prior_usage() {
    let harness = Harness::new().await;
    let free_payload = json!({"name":"Fixture free account","version":null});
    harness
        .app
        .service
        .mutate(
            &admin(),
            "mark-account-300-free",
            "free:300",
            &free_payload,
            Prepared::default(),
            None,
        )
        .await
        .unwrap();
    let upgrade_payload = json!({
        "name":"Paid after upgrade",
        "cost":"100.00",
        "creditor_id":2,
        "cost_kind":"exact",
        "version":1,
    });
    let mut stale_version = upgrade_payload.clone();
    stale_version["version"] = json!(2);
    assert_error(
        harness
            .app
            .service
            .mutate(
                &admin(),
                "upgrade-account-300-stale-version",
                "upgrade-paid:300",
                &stale_version,
                Prepared::default(),
                None,
            )
            .await,
        409,
        "配置已更新",
    );
    assert!(harness.rows("cycles").await.is_empty());

    let mut unavailable_creditor = upgrade_payload.clone();
    unavailable_creditor["creditor_id"] = json!(99);
    assert_error(
        harness
            .app
            .service
            .mutate(
                &admin(),
                "upgrade-account-300-unavailable-creditor",
                "upgrade-paid:300",
                &unavailable_creditor,
                Prepared::default(),
                None,
            )
            .await,
        404,
        "垫付人",
    );
    assert!(harness.rows("cycles").await.is_empty());

    let upgraded = harness
        .app
        .service
        .mutate(
            &admin(),
            "upgrade-account-300",
            "upgrade-paid:300",
            &upgrade_payload,
            Prepared::default(),
            None,
        )
        .await
        .unwrap();
    let replay = harness
        .app
        .service
        .mutate(
            &admin(),
            "upgrade-account-300",
            "upgrade-paid:300",
            &upgrade_payload,
            Prepared::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(replay, upgraded);
    assert_eq!(harness.rows("cycles").await.len(), 1);
    let cycle_id = upgraded["cycle_id"].as_i64().unwrap();
    let original = cycle_row(&harness, cycle_id).await;
    assert_eq!(original["start_locked"], 1);
    assert_eq!(original["start"], upgraded["paid_since"]);
    assert_eq!(original["start_confirmed"], 1);
    assert_eq!(harness.rows("profiles").await[0]["kind"], "paid");

    harness
        .app
        .service
        .mutate(
            &admin(),
            "mark-account-200-free",
            "free:200",
            &json!({"name":"Another free account","version":null}),
            Prepared::default(),
            None,
        )
        .await
        .unwrap();
    assert_error(
        harness
            .app
            .service
            .mutate(
                &admin(),
                "generic-create-for-free-account-200",
                "create",
                &cycle_payload(200, "10.00", 2, START),
                Prepared::default(),
                None,
            )
            .await,
        409,
        "升级付费",
    );
    assert_eq!(harness.rows("cycles").await.len(), 1);

    assert_error(
        edit_cycle(
            &harness,
            cycle_id,
            &admin(),
            "upgrade-cannot-move-start",
            json!({"start":END}),
        )
        .await,
        409,
        "起点已锁定",
    );
    edit_cycle(
        &harness,
        cycle_id,
        &admin(),
        "upgrade-edit-cost",
        json!({"cost":"120.00"}),
    )
    .await
    .unwrap();
    let updated = cycle_row(&harness, cycle_id).await;
    assert_eq!(updated["cost_fen"], 12000);
    assert_eq!(updated["start"], original["start"]);
    assert_eq!(updated["start_locked"], 1);

    let mut source = harness.source();
    source["usage"].as_array_mut().unwrap().extend([
        json!({
            "id":30,
            "account_id":300,
            "user_id":4,
            "amount":"2",
            "created_at":updated["start"],
        }),
        json!({
            "id":31,
            "account_id":300,
            "user_id":4,
            "amount":"3",
            "created_at":updated["start"],
        }),
    ]);
    harness.write_source(&source);
    harness.app.service.sync(true).await.unwrap();
    let end = time::now();
    let calculation = preview(&harness, &[cycle_id], &[], &end).await;
    let cycle_calculation = &calculation["cycles"][0];
    assert_eq!(cycle_calculation["record_count"], 2);
    assert_eq!(cycle_calculation["total_consumption"], "5");
    assert_eq!(cycle_calculation["allocations"][0]["consumption"], "5");
    let usage_ids = harness
        .rows("usage")
        .await
        .iter()
        .filter_map(|row| row["id"].as_i64())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(usage_ids.is_superset(&[7, 30, 31].into_iter().collect()));
}

async fn create_netting_pair(harness: &Harness, prefix: &str) {
    let forward = equal_expense_payload("Forward cents", "0.03", 2, &[3, 4]);
    let forward_expense = create_expense(harness, &forward, &format!("{prefix}-forward"))
        .await
        .unwrap();
    let forward_preview = preview(
        harness,
        &[],
        &[forward_expense["id"].as_i64().unwrap()],
        END,
    )
    .await;
    settle_preview(
        harness,
        forward_preview["id"].as_str().unwrap(),
        &format!("{prefix}-forward-settle"),
        json!([]),
    )
    .await
    .unwrap();

    let reverse = equal_expense_payload("Reverse cent", "0.01", 3, &[2, 4]);
    let reverse_expense = create_expense(harness, &reverse, &format!("{prefix}-reverse"))
        .await
        .unwrap();
    let reverse_preview = preview(
        harness,
        &[],
        &[reverse_expense["id"].as_i64().unwrap()],
        END,
    )
    .await;
    settle_preview(
        harness,
        reverse_preview["id"].as_str().unwrap(),
        &format!("{prefix}-reverse-settle"),
        json!([]),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn netting_keeps_original_cents_orders_and_pair_scoped_permissions() {
    let harness = Harness::new().await;
    create_netting_pair(&harness, "netting-roundtrip").await;
    let detail = harness
        .app
        .service
        .netting_balance(&payer(), 2, 3)
        .await
        .unwrap();
    assert_eq!(detail["user_a"], 2);
    assert_eq!(detail["user_b"], 3);
    assert_eq!(detail["a_to_b_fen"], 1);
    assert_eq!(detail["b_to_a_fen"], 2);
    assert_eq!(detail["offset_fen"], 1);
    assert_eq!(detail["amount_fen"], 1);
    assert_eq!(detail["payer_id"], 3);
    assert_eq!(detail["creditor_id"], 2);
    assert_eq!(detail["order_count"], 2);
    assert!(
        detail["digest"]
            .as_str()
            .is_some_and(|digest| !digest.is_empty())
    );

    let mine = netting_balances(&harness, &payer(), "mine").await.unwrap();
    let receivable = netting_balances(&harness, &creditor(), "receivable")
        .await
        .unwrap();
    let all = netting_balances(&harness, &admin(), "all").await.unwrap();
    let pair_set = |rows: &[Value]| {
        rows.iter()
            .map(|row| {
                (
                    row["user_a"].as_i64().unwrap(),
                    row["user_b"].as_i64().unwrap(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(pair_set(&mine), [(2, 3)].into_iter().collect());
    assert_eq!(
        pair_set(&receivable),
        [(2, 3), (2, 4)].into_iter().collect()
    );
    assert_eq!(pair_set(&all), [(2, 3), (2, 4)].into_iter().collect());
    assert!(
        netting_balances(&harness, &stranger(), "mine")
            .await
            .unwrap()
            .is_empty()
    );
    assert_error(
        netting_balances(&harness, &payer(), "all").await,
        403,
        "管理员",
    );
    assert_error(
        harness.app.service.netting_balance(&payer(), 2, 4).await,
        404,
        "不存在",
    );
    let payment_payload = json!({
        "user_a":2,
        "user_b":3,
        "digest":detail["digest"],
    });
    assert_error(
        pay_netting_on(
            &harness.app,
            &creditor(),
            &payment_payload,
            "creditor-cannot-net-pay",
        )
        .await,
        403,
        "净付款人",
    );

    assert_error(
        harness.app.service.netting_balance(&stranger(), 2, 3).await,
        404,
        "对冲账目不存在",
    );

    let before = harness.rows("orders").await;
    let payment = json!({"user_a":2,"user_b":3,"digest":detail["digest"]});
    pay_netting_on(&harness.app, &payer(), &payment, "netting-roundtrip-pay")
        .await
        .unwrap();
    let after = harness.rows("orders").await;
    assert_eq!(after.len(), before.len());
    let pair_before = before
        .iter()
        .filter(|row| {
            (row["payer_id"] == 3 && row["creditor_id"] == 2)
                || (row["payer_id"] == 2 && row["creditor_id"] == 3)
        })
        .collect::<Vec<_>>();
    assert_eq!(pair_before.len(), 2);
    for old in pair_before {
        let current = after.iter().find(|row| row["id"] == old["id"]).unwrap();
        assert_eq!(current["amount_fen"], old["amount_fen"]);
        assert_eq!(current["status"], "paid");
        assert_eq!(current["version"], old["version"].as_i64().unwrap() + 1);
        assert!(!current["netting_payment_id"].is_null());
    }
    let payments = harness
        .app
        .service
        .netting_payments(&payer(), "mine", None)
        .await
        .unwrap()["payments"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0]["amount_fen"], 1);
    assert_eq!(payments[0]["offset_fen"], 1);
}

#[tokio::test]
async fn zero_net_can_be_confirmed_by_either_participant_without_a_transfer_timestamp() {
    for (index, actor) in [creditor(), payer(), admin()].into_iter().enumerate() {
        let harness = Harness::new().await;
        let forward = create_expense(
            &harness,
            &equal_expense_payload("One cent forward", "0.01", 2, &[3, 4]),
            &format!("zero-net-forward-{index}"),
        )
        .await
        .unwrap();
        let forward_preview = preview(&harness, &[], &[forward["id"].as_i64().unwrap()], END).await;
        settle_preview(
            &harness,
            forward_preview["id"].as_str().unwrap(),
            &format!("zero-net-forward-settle-{index}"),
            json!([]),
        )
        .await
        .unwrap();

        let reverse = create_expense(
            &harness,
            &equal_expense_payload("One cent reverse", "0.01", 3, &[2, 4]),
            &format!("zero-net-reverse-{index}"),
        )
        .await
        .unwrap();
        let reverse_preview = preview(&harness, &[], &[reverse["id"].as_i64().unwrap()], END).await;
        settle_preview(
            &harness,
            reverse_preview["id"].as_str().unwrap(),
            &format!("zero-net-reverse-settle-{index}"),
            json!([]),
        )
        .await
        .unwrap();

        let detail = harness
            .app
            .service
            .netting_balance(&payer(), 2, 3)
            .await
            .unwrap();
        assert_eq!(detail["a_to_b_fen"], 1);
        assert_eq!(detail["b_to_a_fen"], 1);
        assert_eq!(detail["offset_fen"], 1);
        assert_eq!(detail["amount_fen"], 0);
        assert!(detail["payer_id"].is_null());
        assert!(detail["creditor_id"].is_null());

        let payment = json!({
            "user_a":2,
            "user_b":3,
            "digest":detail["digest"],
        });
        pay_netting_on(
            &harness.app,
            &actor,
            &payment,
            &format!("zero-net-confirm-{index}"),
        )
        .await
        .unwrap();

        let pair_orders = harness
            .rows("orders")
            .await
            .into_iter()
            .filter(|row| {
                (row["payer_id"] == 3 && row["creditor_id"] == 2)
                    || (row["payer_id"] == 2 && row["creditor_id"] == 3)
            })
            .collect::<Vec<_>>();
        assert_eq!(pair_orders.len(), 2);
        assert!(pair_orders.iter().all(|row| row["status"] == "paid"));
        assert!(pair_orders.iter().all(|row| row["version"] == 2));
        assert!(pair_orders.iter().all(|row| row["submitted_at"].is_null()));
        assert!(
            pair_orders
                .iter()
                .all(|row| !row["netting_payment_id"].is_null())
        );
        assert_eq!(harness.rows("netting_payments").await[0]["amount_fen"], 0);
    }
}

#[tokio::test]
async fn concurrent_netting_payment_replays_one_committed_response() {
    let harness = Harness::new().await;
    create_netting_pair(&harness, "netting-concurrent").await;
    let payload = netting_payload(&harness, &payer(), 2, 3).await;
    let left = harness.peer().await;
    let right = harness.peer().await;
    let gate = Arc::new(Barrier::new(2));
    let left_gate = gate.clone();
    let right_gate = gate.clone();
    let left_payload = payload.clone();
    let right_payload = payload.clone();
    let left_task = async {
        left_gate.wait().await;
        pay_netting_on(&left, &payer(), &left_payload, "netting-same-key").await
    };
    let right_task = async {
        right_gate.wait().await;
        pay_netting_on(&right, &payer(), &right_payload, "netting-same-key").await
    };
    let (left_result, right_result) = tokio::join!(left_task, right_task);
    let left_result = left_result.unwrap();
    assert_eq!(left_result, right_result.unwrap());
    assert_eq!(harness.rows("netting_payments").await.len(), 1);
    assert_eq!(
        harness
            .rows("requests")
            .await
            .iter()
            .filter(|row| row["key"] == "netting-same-key")
            .count(),
        1
    );
    assert!(
        harness
            .rows("orders")
            .await
            .iter()
            .filter(|row| row["payer_id"] == 3 && row["creditor_id"] == 2)
            .all(|row| row["status"] == "paid")
    );
    assert_eq!(
        pay_netting_on(&left, &payer(), &payload, "netting-same-key")
            .await
            .unwrap(),
        left_result
    );
}

#[tokio::test]
async fn failed_netting_payment_rolls_back_snapshot_orders_and_request_key() {
    let harness = Harness::new().await;
    let cycle = create_cycle(
        &harness,
        &cycle_payload(100, "100.00", 2, START),
        "rollback-netting-cycle",
    )
    .await;
    let expense = create_expense(
        &harness,
        &equal_expense_payload("Second payer order", "10.00", 2, &[3, 4]),
        "rollback-netting-expense",
    )
    .await
    .unwrap();
    let settlement_preview = preview(
        &harness,
        &[cycle["id"].as_i64().unwrap()],
        &[expense["id"].as_i64().unwrap()],
        END,
    )
    .await;
    settle_preview(
        &harness,
        settlement_preview["id"].as_str().unwrap(),
        "rollback-netting-settle",
        json!([]),
    )
    .await
    .unwrap();
    let mut pair_orders = harness
        .rows("orders")
        .await
        .into_iter()
        .filter(|row| row["payer_id"] == 3 && row["creditor_id"] == 2)
        .collect::<Vec<_>>();
    pair_orders.sort_by_key(|row| row["id"].as_i64().unwrap());
    assert!(pair_orders.len() >= 2);
    let fail_order_id = pair_orders.last().unwrap()["id"].as_i64().unwrap();
    let mut db = harness.app.service.store.write().await.unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER fail_netting_order BEFORE UPDATE OF status ON orders WHEN OLD.id={fail_order_id} BEGIN SELECT RAISE(ABORT, 'fixture netting failure'); END"
    ))
    .execute(&mut *db)
    .await
    .unwrap();
    db.commit().await.unwrap();

    let before_orders = harness.rows("orders").await;
    let before_requests = harness.rows("requests").await;
    let payment = netting_payload(&harness, &payer(), 2, 3).await;
    let error = pay_netting_on(&harness.app, &payer(), &payment, "netting-rollback-key")
        .await
        .expect_err("the injected second order failure must abort payment");
    assert_eq!(error.status, 503);
    assert_eq!(harness.rows("orders").await, before_orders);
    assert!(harness.rows("netting_payments").await.is_empty());
    assert_eq!(harness.rows("requests").await, before_requests);
}

#[tokio::test]
async fn bill_import_matches_request_id_and_five_second_token_fallback_before_rounding() {
    let mut source = fixture_data();
    for usage in source["usage"].as_array_mut().unwrap() {
        match usage["id"].as_i64().unwrap() {
            2 => {
                usage["request_id"] = json!("request-2");
                usage["model"] = json!("model-a");
            }
            3 => {
                usage["request_id"] = Value::Null;
                usage["model"] = json!("model-b");
                usage["input_tokens"] = json!(100);
                usage["output_tokens"] = json!(200);
                usage["cache_read_tokens"] = json!(0);
                usage["cache_creation_tokens"] = json!(0);
                usage["created_at"] = json!("2025-01-15T00:00:00+00:00");
            }
            _ => {}
        }
    }
    let harness = Harness::with_fixture(source).await;
    let created = create_cycle(
        &harness,
        &cycle_payload(100, "1.00", 2, START),
        "bill-matching-cycle",
    )
    .await;
    let id = created["id"].as_i64().unwrap();
    let csv = csv_text(&[
        (
            "2025-01-10T23:00:00+00:00",
            "0.00499999",
            "different-model",
            10,
            20,
            "request-2",
        ),
        (
            "2025-01-15T00:00:05+00:00",
            "0.00500001",
            "model-b",
            100,
            200,
            "",
        ),
    ]);
    let preview = bill_preview(&harness, id, &csv, "./sample/usage.csv").await;
    assert_eq!(preview["matched_count"], 2);
    assert_eq!(preview["unmatched_count"], 0);
    assert_eq!(preview["total"], "0.01000000");
    assert_eq!(preview["cost_fen"], 1);
    let confirmed = confirm_bill(&harness, id, &preview, false, "bill-matching-confirm")
        .await
        .unwrap();
    let cycle = cycle_row(&harness, id).await;
    assert!(confirmed["bill_import_id"].as_i64().unwrap() > 0);
    assert_eq!(cycle["cost_fen"], 1);
    assert_eq!(cycle["cost_kind"], "exact");
    assert_eq!(cycle["cost_source"], "upstream_bill");
}

#[tokio::test]
async fn bill_import_matched_only_allocates_only_matched_cost_and_redacts_raw_data() {
    let harness = Harness::new().await;
    let created = create_cycle(
        &harness,
        &cycle_payload(100, "1.00", 2, START),
        "matched-only-bill-cycle",
    )
    .await;
    let id = created["id"].as_i64().unwrap();
    let csv = csv_text(&[
        (
            "2025-01-10T00:00:00+00:00",
            "1.005",
            "fixture-model",
            10,
            20,
            "fixture-2",
        ),
        (
            "2025-01-20T00:00:00+00:00",
            "2.005",
            "fixture-model",
            10,
            20,
            "fixture-4",
        ),
        (
            "2025-01-25T00:00:00+00:00",
            "4.000",
            "unmatched-model",
            999,
            999,
            "missing",
        ),
    ]);
    let bill =
        bill_preview_with_mode(&harness, id, &csv, "matched-only.csv", Some("matched_only")).await;
    assert_eq!(bill["allocation_mode"], "matched_only");
    assert_eq!(bill["cost_fen"], 701);
    assert_eq!(bill["billable_cost_fen"], 301);
    assert_eq!(bill["excluded_amount"], "4.000");
    assert_eq!(bill["unmatched_count"], 1);
    assert_eq!(bill["can_confirm"], true);
    let bill_allocations = allocations(&bill);
    assert_eq!(bill_allocations[&3]["amount_fen"], 101);
    assert_eq!(bill_allocations[&4]["amount_fen"], 200);

    let confirmed = confirm_bill(&harness, id, &bill, false, "matched-only-confirm")
        .await
        .unwrap();
    assert!(confirmed["bill_import_id"].as_i64().unwrap() > 0);
    let cycle = cycle_row(&harness, id).await;
    assert_eq!(cycle["cost_fen"], 701);
    assert_eq!(cycle["bill_allocation_mode"], "matched_only");
    let admin_detail = harness
        .app
        .service
        .cycle_detail(&admin(), id)
        .await
        .unwrap();
    assert_eq!(admin_detail["bill"]["allocation_mode"], "matched_only");
    assert_eq!(admin_detail["bill"]["billable_cost_fen"], 301);
    let admin_allocations = allocations(&admin_detail);
    assert_eq!(admin_allocations[&3]["amount_fen"], 101);
    assert_eq!(admin_allocations[&4]["amount_fen"], 200);

    let payer_detail = harness
        .app
        .service
        .cycle_detail(&payer(), id)
        .await
        .unwrap();
    assert!(payer_detail.get("records").is_none());
    for hidden in [
        "weights",
        "matched",
        "unmatched",
        "outside",
        "file_name",
        "file_hash",
        "import_id",
    ] {
        assert!(
            payer_detail["bill"].get(hidden).is_none(),
            "{hidden} leaked"
        );
    }
    let payer_allocations = allocations(&payer_detail);
    assert_eq!(payer_allocations.len(), 1);
    assert_eq!(payer_allocations[&3]["amount_fen"], 101);

    let current = cycle_row(&harness, id).await;
    assert_error(
        harness
            .app
            .service
            .bill_preview(
                &payer(),
                id,
                &json!({
                    "version":current["version"],
                    "end":END,
                    "csv":csv,
                    "file_name":"private-bill.csv",
                }),
            )
            .await,
        403,
        "管理员",
    );
}

#[tokio::test]
async fn matched_only_bill_import_still_requires_outside_confirmation_and_positive_match() {
    let harness = Harness::new().await;
    let outside_cycle = create_cycle(
        &harness,
        &cycle_payload(100, "1.00", 2, START),
        "matched-only-outside-cycle",
    )
    .await;
    let outside_cycle_id = outside_cycle["id"].as_i64().unwrap();
    let outside_csv = csv_text(&[
        (
            "2025-01-10T00:00:00+00:00",
            "1.00",
            "fixture-model",
            10,
            20,
            "fixture-2",
        ),
        (END, "9.00", "outside-model", 1, 1, "outside-request"),
    ]);
    let outside_bill = bill_preview_with_mode(
        &harness,
        outside_cycle_id,
        &outside_csv,
        "matched-only-outside.csv",
        Some("matched_only"),
    )
    .await;
    assert_eq!(outside_bill["outside_count"], 1);
    assert_eq!(outside_bill["can_confirm"], true);
    assert_error(
        confirm_bill(
            &harness,
            outside_cycle_id,
            &outside_bill,
            false,
            "matched-only-outside-rejected",
        )
        .await,
        422,
        "范围外",
    );
    confirm_bill(
        &harness,
        outside_cycle_id,
        &outside_bill,
        true,
        "matched-only-outside-accepted",
    )
    .await
    .unwrap();

    let empty_match_cycle = create_cycle(
        &harness,
        &cycle_payload(200, "1.00", 2, START),
        "matched-only-no-match-cycle",
    )
    .await;
    let empty_match_id = empty_match_cycle["id"].as_i64().unwrap();
    let unmatched_csv = csv_text(&[(
        "2025-01-10T00:00:00+00:00",
        "1.00",
        "never-matched",
        999,
        999,
        "missing-request",
    )]);
    let empty_match_bill = bill_preview_with_mode(
        &harness,
        empty_match_id,
        &unmatched_csv,
        "matched-only-no-match.csv",
        Some("matched_only"),
    )
    .await;
    assert_eq!(empty_match_bill["matched_count"], 0);
    assert_eq!(empty_match_bill["billable_cost_fen"], 0);
    assert_eq!(empty_match_bill["can_confirm"], false);
    assert_error(
        confirm_bill(
            &harness,
            empty_match_id,
            &empty_match_bill,
            false,
            "matched-only-no-match-confirm",
        )
        .await,
        422,
        "没有正金额",
    );
}

#[tokio::test]
async fn bill_import_outside_confirmation_duplicate_history_and_stale_preview() {
    let harness = Harness::new().await;
    let created = create_cycle(
        &harness,
        &cycle_payload(100, "1.00", 2, START),
        "bill-history-cycle",
    )
    .await;
    let id = created["id"].as_i64().unwrap();
    let first_csv = csv_text(&[
        (
            "2025-01-10T00:00:00+00:00",
            "1.00",
            "fixture-model",
            10,
            20,
            "fixture-2",
        ),
        (END, "9.00", "outside-model", 1, 1, "outside-request"),
    ]);
    let first = bill_preview(&harness, id, &first_csv, "first.csv").await;
    assert_eq!(first["outside_count"], 1);
    assert_eq!(first["can_confirm"], true);
    assert_error(
        confirm_bill(&harness, id, &first, false, "bill-outside-rejected").await,
        422,
        "范围外",
    );
    let accepted = confirm_bill(&harness, id, &first, true, "bill-outside-accepted")
        .await
        .unwrap();
    assert_eq!(cycle_row(&harness, id).await["cost_fen"], 100);

    let duplicate = bill_preview(&harness, id, &first_csv, "renamed-copy.csv").await;
    let duplicate_result = confirm_bill(&harness, id, &duplicate, true, "bill-duplicate-confirm")
        .await
        .unwrap();
    assert_eq!(
        duplicate_result["bill_import_id"],
        accepted["bill_import_id"]
    );
    assert_eq!(harness.rows("bill_imports").await.len(), 1);

    let stale = bill_preview(&harness, id, &first_csv, "stale-copy.csv").await;
    let stale_cycle = cycle_row(&harness, id).await;
    let stale_payload = bill_payload(stale_cycle["version"].clone(), &stale["id"], true);
    let stale_prepared = prepare_bill(&harness.app, id, &stale_payload)
        .await
        .unwrap();

    let replacement_csv = csv_text(&[(
        "2025-01-10T00:00:00+00:00",
        "0.50",
        "fixture-model",
        10,
        20,
        "fixture-2",
    )]);
    let replacement = bill_preview(&harness, id, &replacement_csv, "replacement.csv").await;
    let replaced = confirm_bill(
        &harness,
        id,
        &replacement,
        false,
        "bill-replacement-confirm",
    )
    .await
    .unwrap();
    assert_ne!(replaced["bill_import_id"], accepted["bill_import_id"]);
    assert_eq!(cycle_row(&harness, id).await["cost_fen"], 50);
    assert_eq!(harness.rows("bill_imports").await.len(), 2);
    assert_error(
        confirm_bill_on(
            &harness.app,
            id,
            &stale_payload,
            stale_prepared,
            "bill-stale-confirm",
        )
        .await,
        409,
        "修改",
    );
}
