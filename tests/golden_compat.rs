//! Frozen synthetic v0.8.0 vectors. Treat these bytes as an immutable contract.
use std::collections::BTreeMap;

use serde_json::{Value, json};
use settlement::{billing, json as compat_json, money, time};

fn fixture(name: &str) -> Value {
    let text = match name {
        "money" => include_str!("golden/money.json"),
        "instant" => include_str!("golden/instant.json"),
        "csv" => include_str!("golden/csv.json"),
        "reconcile" => include_str!("golden/reconcile.json"),
        "serialization" => include_str!("golden/serialization.json"),
        "netting" => include_str!("golden/netting.json"),
        "fernet" => include_str!("golden/fernet.json"),
        other => panic!("Unknown golden fixture {other}"),
    };
    serde_json::from_str(text).expect("Valid golden fixture JSON")
}

fn cases<'a>(fixture: &'a Value, group: &str) -> &'a [Value] {
    fixture[group].as_array().expect("Golden group is an array")
}

fn label(case: &Value) -> String {
    case["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| case["input"].to_string())
}

fn first_difference(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    if expected == actual {
        return None;
    }
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, value) in expected {
                let next = format!("{path}.{key}");
                match actual.get(key) {
                    Some(other) => {
                        if let Some(diff) = first_difference(value, other, &next) {
                            return Some(diff);
                        }
                    }
                    None => return Some(format!("{next}: missing, expected {value}")),
                }
            }
            for key in actual.keys() {
                if !expected.contains_key(key) {
                    return Some(format!("{path}.{key}: unexpected {}", actual[key]));
                }
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                return Some(format!(
                    "{path}: expected {} rows, got {}",
                    expected.len(),
                    actual.len()
                ));
            }
            for (index, (left, right)) in expected.iter().zip(actual).enumerate() {
                if let Some(diff) = first_difference(left, right, &format!("{path}[{index}]")) {
                    return Some(diff);
                }
            }
        }
        _ => {}
    }
    Some(format!("{path}: expected {expected}, got {actual}"))
}

fn compare(failures: &mut Vec<String>, label: &str, expected: &Value, actual: &Value) {
    if let Some(diff) = first_difference(expected, actual, "$") {
        failures.push(format!("{label}: {diff}"));
    }
}

fn compare_result(
    failures: &mut Vec<String>,
    case: &Value,
    actual: settlement::error::Result<Value>,
) {
    match actual {
        Ok(actual) if case.get("expected").is_some() => {
            compare(failures, &label(case), &case["expected"], &actual);
        }
        Ok(actual) => failures.push(format!(
            "{}: expected error {}, got {actual}",
            label(case),
            case["expected_error"]
        )),
        Err(error) if case.get("expected_error").is_some() => compare(
            failures,
            &label(case),
            &case["expected_error"]["message"],
            &json!(error.to_string()),
        ),
        Err(error) => failures.push(format!("{}: unexpected error {error}", label(case))),
    }
}

fn finish(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} Python compatibility differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn weights(entries: &Value) -> BTreeMap<i64, money::Decimal> {
    entries
        .as_array()
        .expect("Weight entries")
        .iter()
        .map(|entry| {
            (
                entry[0].as_i64().expect("Integer user ID"),
                money::Decimal::parse(entry[1].as_str().expect("Decimal string"))
                    .expect("Valid fixture weight"),
            )
        })
        .collect()
}

#[test]
fn decimal_scale_and_scientific_rendering_match_python() {
    let data = fixture("money");
    let mut failures = vec![];
    // Non-string coercion belongs to the API boundary; Decimal::parse takes &str.
    for case in cases(&data, "decimal") {
        if let Some(input) = case["input"].as_str() {
            compare_result(
                &mut failures,
                case,
                money::Decimal::parse(input).map(|value| json!(value.to_string())),
            );
        }
    }
    finish(failures);
}

#[test]
fn cents_validation_and_limits_match_python() {
    let data = fixture("money");
    let mut failures = vec![];
    for case in cases(&data, "cents") {
        if let Some(input) = case["input"].as_str() {
            compare_result(
                &mut failures,
                case,
                money::cents(input).map(|value| json!(value)),
            );
        }
    }
    finish(failures);
}

#[test]
fn proportional_largest_remainders_and_context_match_python() {
    let data = fixture("money");
    let mut failures = vec![];
    for case in cases(&data, "allocate") {
        compare_result(
            &mut failures,
            case,
            money::allocate(
                case["cost_fen"].as_i64().unwrap(),
                &weights(&case["weights"]),
                case["creditor_id"].as_i64().unwrap(),
                case["precision"].as_u64().unwrap() as u32,
            ),
        );
    }
    finish(failures);
}

#[test]
fn matched_amounts_round_once_and_resolve_numeric_ties() {
    let data = fixture("money");
    let mut failures = vec![];
    // Product allocate_matched uses the normal 28-digit Python call context.
    for case in cases(&data, "allocate_matched")
        .iter()
        .filter(|case| case["precision"] == 28)
    {
        compare_result(
            &mut failures,
            case,
            money::allocate_matched(
                &weights(&case["weights"]),
                case["creditor_id"].as_i64().unwrap(),
            ),
        );
    }
    finish(failures);
}

#[test]
fn utc_and_microsecond_formatting_match_python() {
    let data = fixture("instant");
    let mut failures = vec![];
    for case in cases(&data, "cases") {
        compare_result(
            &mut failures,
            case,
            time::instant(case["input"].as_str().unwrap()).map(|value| json!(value)),
        );
    }
    finish(failures);
}

#[test]
fn csv_parsing_and_validation_match_python() {
    let data = fixture("csv");
    let mut failures = vec![];
    for case in cases(&data, "cases") {
        compare_result(
            &mut failures,
            case,
            billing::parse_csv(case["input"].as_str().unwrap()),
        );
    }
    finish(failures);
}

#[test]
fn reconciliation_matching_modes_and_digest_match_python() {
    let data = fixture("reconcile");
    let mut failures = vec![];
    for case in cases(&data, "cases") {
        let details = case["details"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (entry[0].as_i64().unwrap(), entry[1].clone()))
            .collect();
        compare_result(
            &mut failures,
            case,
            billing::reconcile(
                case["rows"].as_array().unwrap(),
                case["usage"].as_array().unwrap(),
                &details,
                case["start"].as_str().unwrap(),
                case["end"].as_str().unwrap(),
                case["allocation_mode"].as_str().unwrap(),
            ),
        );
    }
    finish(failures);
}

#[test]
fn reconciled_weights_allocate_like_python() {
    let data = fixture("reconcile");
    let mut failures = vec![];
    for case in cases(&data, "cases") {
        let Some(expected) = case.get("expected") else {
            continue;
        };
        let creditor = case["creditor_id"].as_i64().unwrap();
        let actual = billing::bill_allocations(expected, creditor);
        match actual {
            Ok(actual) => compare(
                &mut failures,
                &label(case),
                &case["expected_allocations"],
                &actual,
            ),
            Err(error) => failures.push(format!("{}: unexpected error {error}", label(case))),
        }
    }
    finish(failures);
}

#[test]
fn canonical_json_and_numeric_weight_digest_match_python() {
    let data = fixture("serialization");
    let mut failures = vec![];
    for case in cases(&data, "cases") {
        let paths = case["integer_key_paths"].as_array().unwrap();
        if paths.is_empty() {
            compare(
                &mut failures,
                &label(case),
                &case["expected_json"],
                &json!(compat_json::dumps(&case["value"])),
            );
            compare(
                &mut failures,
                &label(case),
                &case["expected_sha256"],
                &json!(compat_json::digest(&case["value"])),
            );
        } else if paths == &vec![json!(["weights"])] {
            compare(
                &mut failures,
                &label(case),
                &case["expected_sha256"],
                &json!(compat_json::billing_digest(&case["value"])),
            );
        }
    }
    for case in cases(&fixture("reconcile"), "cases") {
        if let Some(payload) = case["digest_payload_json"].as_str() {
            let parsed = serde_json::from_str(payload).unwrap();
            compare(
                &mut failures,
                &label(case),
                &case["expected"]["digest"],
                &json!(compat_json::billing_digest(&parsed)),
            );
        }
    }
    finish(failures);
}

#[test]
fn netting_full_rows_versions_and_membership_digest_match_python() {
    let data = fixture("netting");
    let mut failures = vec![];
    for case in cases(&data, "cases") {
        compare(
            &mut failures,
            &label(case),
            &case["digest_payload_json"],
            &json!(compat_json::dumps(&case["orders"])),
        );
        compare(
            &mut failures,
            &label(case),
            &case["expected"]["digest"],
            &json!(compat_json::digest(&case["orders"])),
        );
    }
    finish(failures);
}

#[tokio::test]
async fn netting_balances_and_current_names_match_python() {
    use sqlx::Connection;

    let data = fixture("netting");
    let mut failures = vec![];
    let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
        .await
        .expect("Isolated in-memory fixture database");
    sqlx::query("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&mut db)
        .await
        .unwrap();
    for case in cases(&data, "cases") {
        sqlx::query("DELETE FROM users")
            .execute(&mut db)
            .await
            .unwrap();
        for user in case["current_users"].as_array().unwrap() {
            sqlx::query("INSERT INTO users VALUES(?,?)")
                .bind(user[0].as_i64().unwrap())
                .bind(user[1].as_str().unwrap())
                .execute(&mut db)
                .await
                .unwrap();
        }
        compare_result(
            &mut failures,
            case,
            settlement::service::Service::balance(&mut db, case["orders"].as_array().unwrap())
                .await,
        );
    }
    db.close().await.unwrap();
    finish(failures);
}

#[test]
fn fernet_decrypts_python_tokens_and_rejects_tampering() {
    let data = fixture("fernet");
    let mut failures = vec![];
    for case in cases(&data, "encrypt") {
        let cipher = fernet::Fernet::new(case["key_base64url"].as_str().unwrap())
            .expect("Public synthetic key is valid");
        match cipher.decrypt(case["token"].as_str().unwrap()) {
            Ok(bytes) => compare(
                &mut failures,
                &label(case),
                &case["plaintext_hex"],
                &json!(hex::encode(bytes)),
            ),
            Err(_) => failures.push(format!("{}: Python token rejected", label(case))),
        }
    }
    for case in cases(&data, "decrypt")
        .iter()
        .filter(|case| case["ttl"].is_null())
    {
        let cipher = fernet::Fernet::new(case["key_base64url"].as_str().unwrap()).unwrap();
        let result = cipher.decrypt(case["token"].as_str().unwrap());
        if case.get("expected_error").is_some() {
            if result.is_ok() {
                failures.push(format!("{}: invalid token accepted", label(case)));
            }
        } else {
            match result {
                Ok(bytes) => compare(
                    &mut failures,
                    &label(case),
                    &case["expected"],
                    &json!(String::from_utf8(bytes).unwrap()),
                ),
                Err(_) => failures.push(format!("{}: Python token rejected", label(case))),
            }
        }
    }
    finish(failures);
}
