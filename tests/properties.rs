use proptest::{prelude::*, test_runner::RngSeed};
use serde_json::{Value, json};
use settlement::{
    billing,
    money::{self, Decimal},
};
use std::collections::BTreeMap;

const PERIOD_START: &str = "2025-01-01T00:00:00.000000+00:00";
const PERIOD_END: &str = "2025-02-01T00:00:00.000000+00:00";
const BASE_TIME: &str = "2025-01-15T00:01:00.000000+00:00";

fn dollars_from_micros(value: u64) -> String {
    format!("{}.{:06}", value / 1_000_000, value % 1_000_000)
}

fn parsed_weights(entries: impl IntoIterator<Item = (i64, u64)>) -> BTreeMap<i64, Decimal> {
    entries
        .into_iter()
        .map(|(user_id, micros)| {
            (
                user_id,
                Decimal::parse(&dollars_from_micros(micros)).unwrap(),
            )
        })
        .collect()
}

fn amount_fen_sum(allocation: &Value) -> i64 {
    allocation
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["amount_fen"].as_i64().unwrap())
        .sum()
}

fn allocations_are_nonnegative(allocation: &Value) -> bool {
    allocation
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["amount_fen"].as_i64().unwrap() >= 0)
}

fn allocation_by_user(allocation: &Value) -> BTreeMap<i64, i64> {
    allocation
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["user_id"].as_i64().unwrap(),
                entry["amount_fen"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn row(
    row_id: i64,
    created_at: &str,
    amount: &str,
    request_id: &str,
    model: &str,
    tokens: [i64; 4],
) -> Value {
    json!({
        "row": row_id,
        "created_at": created_at,
        "amount": amount,
        "request_id": request_id,
        "model": model,
        "input_tokens": tokens[0],
        "output_tokens": tokens[1],
        "cache_read_tokens": tokens[2],
        "cache_creation_tokens": tokens[3],
    })
}

fn usage(id: i64, user_id: i64, created_at: &str) -> Value {
    json!({
        "id": id,
        "user_id": user_id,
        "created_at": created_at,
    })
}

fn detail(
    request_id: Option<&str>,
    upstream_request_id: Option<&str>,
    model: &str,
    tokens: [i64; 4],
) -> Value {
    json!({
        "request_id": request_id,
        "upstream_request_id": upstream_request_id,
        "model": model,
        "requested_model": null,
        "upstream_model": null,
        "upstream_response_model": null,
        "input_tokens": tokens[0],
        "output_tokens": tokens[1],
        "cache_read_tokens": tokens[2],
        "cache_creation_tokens": tokens[3],
    })
}

fn matched_by_row(result: &Value) -> BTreeMap<i64, (i64, i64)> {
    result["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| {
            (
                record["row"].as_i64().unwrap(),
                (
                    record["usage_id"].as_i64().unwrap(),
                    record["user_id"].as_i64().unwrap(),
                ),
            )
        })
        .collect()
}

fn weights_from_result(result: &Value) -> BTreeMap<i64, Decimal> {
    result["weights"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(user_id, amount)| {
            (
                user_id.parse().unwrap(),
                Decimal::parse(amount.as_str().unwrap()).unwrap(),
            )
        })
        .collect()
}

fn reverse_and_rotate<T: Clone>(items: &[T]) -> Vec<T> {
    let mut reordered = items.to_vec();
    reordered.reverse();
    if reordered.len() > 1 {
        let shift = 2 % reordered.len();
        reordered.rotate_left(shift);
    }
    reordered
}

fn indexed_time(second: usize) -> String {
    format!("2025-01-15T00:01:{second:02}.000000+00:00")
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        rng_seed: RngSeed::Fixed(0x5eed_2026_1015),
        .. ProptestConfig::default()
    })]

    #[test]
    fn allocation_properties_hold_for_generated_inputs(
        cost_fen in 0i64..100_000,
        raw_weights in prop::collection::btree_map(-1_000i64..1_000, 0u64..50_000_001, 0..=12),
        precision in 1u32..=40,
    ) {
        let entries: Vec<_> = raw_weights.into_iter().collect();
        let weights = parsed_weights(entries.iter().copied());
        let reversed_weights = parsed_weights(entries.iter().rev().copied());

        let proportional = money::allocate(cost_fen, &weights, 77, precision).unwrap();
        prop_assert_eq!(amount_fen_sum(&proportional), cost_fen);
        prop_assert!(allocations_are_nonnegative(&proportional));
        prop_assert_eq!(
            &proportional,
            &money::allocate(cost_fen, &reversed_weights, 77, precision).unwrap()
        );
        prop_assert_eq!(
            &proportional,
            &money::allocate(cost_fen, &weights, 77, precision).unwrap()
        );

        let matched = money::allocate_matched(&weights, 77).unwrap();
        let total_micros: u64 = entries.iter().map(|(_, amount)| amount).sum();
        let expected_matched_fen = ((total_micros + 5_000) / 10_000) as i64;
        prop_assert_eq!(amount_fen_sum(&matched), expected_matched_fen);
        prop_assert!(allocations_are_nonnegative(&matched));
        prop_assert_eq!(
            &matched,
            &money::allocate_matched(&reversed_weights, 77).unwrap()
        );
        prop_assert_eq!(
            &matched,
            &money::allocate_matched(&weights, 77).unwrap()
        );
    }

    #[test]
    fn reconcile_joins_unique_ids_under_reordering_and_keeps_allocation_consistent(
        entries in prop::collection::btree_map(1i64..1_000, 1u64..50_000_001, 1..=8),
    ) {
        let mut rows = Vec::new();
        let mut usage_rows = Vec::new();
        let mut details = BTreeMap::new();
        let mut expected_owners = BTreeMap::new();

        for (index, (user_id, micros)) in entries.iter().enumerate() {
            let usage_id = 10_000 + index as i64;
            let row_id = 100 + index as i64;
            let request_id = format!("fixture-request-{user_id}");
            let amount = dollars_from_micros(*micros);
            rows.push(row(
                row_id,
                BASE_TIME,
                &amount,
                &request_id,
                "fixture-model",
                [10, 20, 0, 0],
            ));
            usage_rows.push(usage(usage_id, *user_id, BASE_TIME));
            details.insert(
                usage_id,
                detail(Some(&request_id), None, "fixture-model", [10, 20, 0, 0]),
            );
            expected_owners.insert(row_id, (usage_id, *user_id));
        }
        for mode in ["proportional", "matched_only"] {
            let baseline = billing::reconcile(
                &rows,
                &usage_rows,
                &details,
                PERIOD_START,
                PERIOD_END,
                mode,
            )
            .unwrap();
            let repeated = billing::reconcile(
                &rows,
                &usage_rows,
                &details,
                PERIOD_START,
                PERIOD_END,
                mode,
            )
            .unwrap();
            prop_assert_eq!(&baseline, &repeated);

            prop_assert_eq!(baseline["matched_count"].as_u64().unwrap(), entries.len() as u64);
            prop_assert_eq!(baseline["unmatched_count"].as_u64().unwrap(), 0);
            prop_assert_eq!(matched_by_row(&baseline), expected_owners.clone());

            let reordered_rows = reverse_and_rotate(&rows);
            let reordered_usage = reverse_and_rotate(&usage_rows);
            let reordered = billing::reconcile(
                &reordered_rows,
                &reordered_usage,
                &details,
                PERIOD_START,
                PERIOD_END,
                mode,
            )
            .unwrap();
            prop_assert_eq!(matched_by_row(&baseline), matched_by_row(&reordered));
            prop_assert_eq!(&baseline["weights"], &reordered["weights"]);

            let creditor = 500;
            let baseline_allocations = billing::bill_allocations(&baseline, creditor).unwrap();
            let reordered_allocations = billing::bill_allocations(&reordered, creditor).unwrap();
            prop_assert_eq!(&baseline_allocations, &reordered_allocations);

            let weights = weights_from_result(&baseline);
            let expected_allocations = if mode == "matched_only" {
                money::allocate_matched(&weights, creditor).unwrap()
            } else {
                money::allocate(
                    baseline["cost_fen"].as_i64().unwrap(),
                    &weights,
                    creditor,
                    28,
                )
                .unwrap()
            };
            prop_assert_eq!(&baseline_allocations, &expected_allocations);
        }

        let mut fallback_rows = Vec::new();
        let mut fallback_usage = Vec::new();
        let mut fallback_details = BTreeMap::new();
        let mut fallback_owners = BTreeMap::new();
        for (index, (user_id, micros)) in entries.iter().enumerate() {
            let usage_id = 20_000 + index as i64;
            let row_id = 500 + index as i64;
            let tokens = [
                100 + index as i64,
                200 + index as i64,
                300 + index as i64,
                400 + index as i64,
            ];
            let timestamp = indexed_time(index);
            let amount = dollars_from_micros(*micros);
            fallback_rows.push(row(
                row_id,
                &timestamp,
                &amount,
                "",
                "fixture-fallback-model",
                tokens,
            ));
            fallback_usage.push(usage(usage_id, *user_id, &timestamp));
            fallback_details.insert(
                usage_id,
                detail(None, None, "fixture-fallback-model", tokens),
            );
            fallback_owners.insert(row_id, (usage_id, *user_id));
        }

        for mode in ["proportional", "matched_only"] {
            let baseline = billing::reconcile(
                &fallback_rows,
                &fallback_usage,
                &fallback_details,
                PERIOD_START,
                PERIOD_END,
                mode,
            )
            .unwrap();
            let repeated = billing::reconcile(
                &fallback_rows,
                &fallback_usage,
                &fallback_details,
                PERIOD_START,
                PERIOD_END,
                mode,
            )
            .unwrap();
            prop_assert_eq!(&baseline, &repeated);
            prop_assert_eq!(matched_by_row(&baseline), fallback_owners.clone());

            let reordered_rows = reverse_and_rotate(&fallback_rows);
            let reordered_usage = reverse_and_rotate(&fallback_usage);
            let reordered = billing::reconcile(
                &reordered_rows,
                &reordered_usage,
                &fallback_details,
                PERIOD_START,
                PERIOD_END,
                mode,
            )
            .unwrap();
            prop_assert_eq!(matched_by_row(&baseline), matched_by_row(&reordered));
            prop_assert_eq!(&baseline["weights"], &reordered["weights"]);

            let creditor = 500;
            let baseline_allocations = billing::bill_allocations(&baseline, creditor).unwrap();
            let reordered_allocations = billing::bill_allocations(&reordered, creditor).unwrap();
            prop_assert_eq!(&baseline_allocations, &reordered_allocations);
        }
    }
}

#[test]
fn equal_remainders_are_awarded_by_ascending_user_id() {
    let equal_dollars = BTreeMap::from([
        (30, Decimal::parse("1").unwrap()),
        (10, Decimal::parse("1").unwrap()),
        (20, Decimal::parse("1").unwrap()),
    ]);
    let proportional = money::allocate(2, &equal_dollars, 99, 28).unwrap();
    assert_eq!(
        allocation_by_user(&proportional),
        BTreeMap::from([(10, 1), (20, 1), (30, 0)])
    );

    let equal_half_fen = BTreeMap::from([
        (30, Decimal::parse("0.005").unwrap()),
        (10, Decimal::parse("0.005").unwrap()),
        (20, Decimal::parse("0.005").unwrap()),
    ]);
    let matched = money::allocate_matched(&equal_half_fen, 99).unwrap();
    assert_eq!(
        allocation_by_user(&matched),
        BTreeMap::from([(10, 1), (20, 1), (30, 0)])
    );
}

#[test]
fn zero_cost_and_empty_or_all_zero_weights_are_deterministic() {
    let empty = BTreeMap::new();
    let all_zero = BTreeMap::from([
        (1, Decimal::parse("0").unwrap()),
        (2, Decimal::parse("0.000000").unwrap()),
    ]);
    let zero_cost = BTreeMap::from([
        (1, Decimal::parse("1.25").unwrap()),
        (2, Decimal::parse("0.75").unwrap()),
    ]);
    let self_covered_zero = json!([{
        "user_id": 77,
        "consumption": "0",
        "amount_fen": 0,
        "share": "0",
        "self_covered": true,
    }]);

    assert_eq!(
        money::allocate(0, &empty, 77, 28).unwrap(),
        self_covered_zero
    );
    assert_eq!(
        money::allocate(0, &all_zero, 77, 28).unwrap(),
        self_covered_zero
    );
    let zero_cost_allocation = money::allocate(0, &zero_cost, 77, 28).unwrap();
    assert_eq!(amount_fen_sum(&zero_cost_allocation), 0);
    assert!(allocations_are_nonnegative(&zero_cost_allocation));
    assert!(
        allocation_by_user(&zero_cost_allocation)
            .values()
            .all(|amount| *amount == 0)
    );

    assert_eq!(money::allocate_matched(&empty, 77).unwrap(), json!([]));
    assert_eq!(money::allocate_matched(&all_zero, 77).unwrap(), json!([]));
}

#[test]
fn reconcile_prefers_ids_reserves_fallback_and_allocates_the_matched_weights() {
    let rows = vec![
        row(
            2,
            "2025-01-15T00:01:04.000000+00:00",
            "2.500000",
            "",
            "fixture-model",
            [10, 20, 0, 0],
        ),
        row(
            3,
            BASE_TIME,
            "1.250000",
            "fixture-explicit",
            "different-bill-model",
            [999, 1, 0, 0],
        ),
    ];
    let usage_rows = vec![
        usage(202, 10, "2025-01-15T00:01:04.000000+00:00"),
        usage(101, 2, BASE_TIME),
    ];
    let details = BTreeMap::from([
        (
            101,
            detail(
                Some("fixture-explicit"),
                None,
                "another-model",
                [7, 8, 0, 0],
            ),
        ),
        (202, detail(None, None, "fixture-model", [10, 20, 0, 0])),
    ]);

    for mode in ["proportional", "matched_only"] {
        let result =
            billing::reconcile(&rows, &usage_rows, &details, PERIOD_START, PERIOD_END, mode)
                .unwrap();
        assert_eq!(
            matched_by_row(&result),
            BTreeMap::from([(2, (202, 10)), (3, (101, 2))])
        );

        let weights = weights_from_result(&result);
        let actual = billing::bill_allocations(&result, 10).unwrap();
        let expected = if mode == "matched_only" {
            money::allocate_matched(&weights, 10).unwrap()
        } else {
            money::allocate(result["cost_fen"].as_i64().unwrap(), &weights, 10, 28).unwrap()
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn token_fallback_includes_exact_five_second_edges_only() {
    let rows = vec![
        row(
            2,
            BASE_TIME,
            "1.000000",
            "",
            "fixture-model",
            [10, 20, 0, 0],
        ),
        row(
            3,
            BASE_TIME,
            "1.000000",
            "",
            "fixture-model",
            [11, 20, 0, 0],
        ),
        row(
            4,
            BASE_TIME,
            "1.000000",
            "",
            "fixture-model",
            [12, 20, 0, 0],
        ),
    ];
    let usage_rows = vec![
        usage(101, 1, "2025-01-15T00:00:55.000000+00:00"),
        usage(102, 2, "2025-01-15T00:01:05.000000+00:00"),
        usage(103, 3, "2025-01-15T00:01:05.000001+00:00"),
    ];
    let details = BTreeMap::from([
        (101, detail(None, None, "fixture-model", [10, 20, 0, 0])),
        (102, detail(None, None, "fixture-model", [11, 20, 0, 0])),
        (103, detail(None, None, "fixture-model", [12, 20, 0, 0])),
    ]);
    let result = billing::reconcile(
        &rows,
        &usage_rows,
        &details,
        PERIOD_START,
        PERIOD_END,
        "proportional",
    )
    .unwrap();

    assert_eq!(
        matched_by_row(&result),
        BTreeMap::from([(2, (101, 1)), (3, (102, 2))])
    );
    assert_eq!(result["unmatched_count"].as_u64(), Some(1));
    assert_eq!(result["unmatched"][0]["row"].as_i64(), Some(4));
    assert_eq!(result["unmatched"][0]["reason"], "missing");
}

#[test]
fn same_second_same_token_candidates_remain_ambiguous() {
    let rows = vec![row(
        2,
        BASE_TIME,
        "1.000000",
        "",
        "fixture-model",
        [10, 20, 0, 0],
    )];
    let usage_rows = vec![usage(101, 1, BASE_TIME), usage(102, 2, BASE_TIME)];
    let details = BTreeMap::from([
        (101, detail(None, None, "fixture-model", [10, 20, 0, 0])),
        (102, detail(None, None, "fixture-model", [10, 20, 0, 0])),
    ]);
    let result = billing::reconcile(
        &rows,
        &usage_rows,
        &details,
        PERIOD_START,
        PERIOD_END,
        "proportional",
    )
    .unwrap();

    assert_eq!(result["matched_count"].as_u64(), Some(0));
    assert_eq!(result["unmatched_count"].as_u64(), Some(1));
    assert_eq!(result["unmatched"][0]["reason"], "ambiguous");
    assert!(result["weights"].as_object().unwrap().is_empty());
}

#[test]
fn duplicate_upstream_or_local_ids_do_not_guess_a_user() {
    for duplicate_field in ["request_id", "upstream_request_id"] {
        let rows = vec![row(
            2,
            BASE_TIME,
            "1.000000",
            "fixture-duplicate",
            "bill-model",
            [10, 20, 0, 0],
        )];
        let usage_rows = vec![usage(101, 1, BASE_TIME), usage(102, 2, BASE_TIME)];
        let details = BTreeMap::from([
            (
                101,
                if duplicate_field == "request_id" {
                    detail(Some("fixture-duplicate"), None, "local-a", [1, 2, 0, 0])
                } else {
                    detail(None, Some("fixture-duplicate"), "local-a", [1, 2, 0, 0])
                },
            ),
            (
                102,
                if duplicate_field == "request_id" {
                    detail(Some("fixture-duplicate"), None, "local-b", [3, 4, 0, 0])
                } else {
                    detail(None, Some("fixture-duplicate"), "local-b", [3, 4, 0, 0])
                },
            ),
        ]);
        let result = billing::reconcile(
            &rows,
            &usage_rows,
            &details,
            PERIOD_START,
            PERIOD_END,
            "proportional",
        )
        .unwrap();
        assert_eq!(result["matched_count"].as_u64(), Some(0));
        assert_eq!(result["unmatched"][0]["reason"], "ambiguous");
        assert!(result["weights"].as_object().unwrap().is_empty());
    }

    let duplicate_rows = vec![
        row(
            2,
            BASE_TIME,
            "1.000000",
            "fixture-one-local",
            "fixture-model",
            [10, 20, 0, 0],
        ),
        row(
            3,
            BASE_TIME,
            "2.000000",
            "fixture-one-local",
            "fixture-model",
            [11, 20, 0, 0],
        ),
    ];
    let one_usage = vec![usage(201, 7, BASE_TIME)];
    let one_detail = BTreeMap::from([(
        201,
        detail(
            Some("fixture-one-local"),
            None,
            "fixture-model",
            [10, 20, 0, 0],
        ),
    )]);
    let result = billing::reconcile(
        &duplicate_rows,
        &one_usage,
        &one_detail,
        PERIOD_START,
        PERIOD_END,
        "proportional",
    )
    .unwrap();
    assert_eq!(result["matched_count"].as_u64(), Some(0));
    assert_eq!(result["unmatched_count"].as_u64(), Some(2));
    assert!(
        result["unmatched"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| { record["reason"] == "ambiguous" })
    );
    assert!(result["weights"].as_object().unwrap().is_empty());
}
