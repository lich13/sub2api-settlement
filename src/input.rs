use crate::{
    error::{Error, Result},
    json::{array, int, text},
    time,
};
use regex::Regex;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::LazyLock};

static COST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(0|[1-9][0-9]{0,9})(\.[0-9]{1,2})?$").expect("fixed cost validation expression")
});
static PERCENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(0|[1-9][0-9]{0,2})(\.[0-9]{1,2})?$")
        .expect("fixed percent validation expression")
});
static DIGEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{64}$").expect("fixed SHA-256 validation expression"));
fn issue(kind: &str, field: &str, message: &str, value: &Value, ctx: Option<Value>) -> Error {
    let mut d = json!({"type":kind,"loc":if field.is_empty(){json!(["body"])}else{json!(["body",field])},"msg":message,"input":value});
    if let Some(ctx) = ctx {
        d["ctx"] = ctx;
    }
    Error::validation(json!([d]))
}
fn required(v: &Value, k: &str) -> Result<()> {
    if v.get(k).is_none() {
        return Err(issue("missing", k, "Field required", v, None));
    }
    Ok(())
}
fn string(v: &Value, k: &str, min: usize, max: usize) -> Result<()> {
    required(v, k)?;
    let Some(s) = v[k].as_str() else {
        return Err(issue(
            "string_type",
            k,
            "Input should be a valid string",
            &v[k],
            None,
        ));
    };
    if s.chars().count() < min {
        return Err(issue(
            "string_too_short",
            k,
            &format!(
                "String should have at least {min} character{}",
                if min == 1 { "" } else { "s" }
            ),
            &v[k],
            Some(json!({"min_length":min})),
        ));
    }
    if s.chars().count() > max {
        return Err(issue(
            "string_too_long",
            k,
            &format!("String should have at most {max} characters"),
            &v[k],
            Some(json!({"max_length":max})),
        ));
    }
    Ok(())
}
fn positive(v: &mut Value, k: &str) -> Result<()> {
    required(v, k)?;
    if let Some(s) = v[k].as_str()
        && let Ok(i) = s.parse::<i64>()
    {
        v[k] = json!(i);
    }
    if v[k].as_i64().is_none() {
        return Err(issue(
            "int_parsing",
            k,
            "Input should be a valid integer, unable to parse string as an integer",
            &v[k],
            None,
        ));
    }
    if int(v, k) < 1 {
        return Err(issue(
            "greater_than",
            k,
            "Input should be greater than 0",
            &v[k],
            Some(json!({"gt":0})),
        ));
    }
    Ok(())
}
fn choice(v: &Value, k: &str, values: &[&str], nullable: bool) -> Result<()> {
    let (last, preceding) = values.split_last().ok_or_else(Error::unavailable)?;
    if nullable && v[k].is_null() {
        return Ok(());
    }
    if values.contains(&text(v, k)) {
        return Ok(());
    }
    let expected = if values.len() == 2 {
        format!("'{}' or '{}'", values[0], values[1])
    } else {
        format!(
            "{} or '{}'",
            preceding
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", "),
            last
        )
    };
    Err(issue(
        "literal_error",
        k,
        &format!("Input should be {expected}"),
        &v[k],
        Some(json!({"expected":expected})),
    ))
}
fn cost(v: &Value, k: &str, regex: &Regex) -> Result<()> {
    string(v, k, 0, usize::MAX)?;
    if !regex.is_match(text(v, k)) {
        let pattern = regex.as_str();
        return Err(issue(
            "string_pattern_mismatch",
            k,
            &format!("String should match pattern '{pattern}'"),
            &v[k],
            Some(json!({"pattern":pattern})),
        ));
    }
    Ok(())
}
fn date_time(s: &str) -> Result<String> {
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&s.replace(' ', "T")) {
        let precision = if d.timestamp_subsec_micros() == 0 {
            chrono::SecondsFormat::Secs
        } else {
            chrono::SecondsFormat::Micros
        };
        return Ok(d.to_rfc3339_opts(precision, true));
    }
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Ok(d
            .format(if d.and_utc().timestamp_subsec_micros() == 0 {
                "%Y-%m-%dT%H:%M:%S"
            } else {
                "%Y-%m-%dT%H:%M:%S%.6f"
            })
            .to_string());
    }
    Err(Error::invalid("时间格式无效"))
}
fn ids(v: &Value, k: &str, min: usize) -> Result<()> {
    let Some(values) = v[k].as_array() else {
        return Err(Error::invalid("请选择不同的有效记录"));
    };
    if values.len() < min || values.len() > 100 {
        return Err(Error::invalid("记录数量无效"));
    }
    let unique = values
        .iter()
        .filter_map(Value::as_i64)
        .collect::<BTreeSet<_>>();
    if unique.len() != values.len() || unique.iter().any(|i| *i < 1) {
        return Err(issue(
            "value_error",
            "",
            "Value error, 请选择不同的有效记录",
            v,
            Some(json!({"error":{}})),
        ));
    }
    Ok(())
}
pub fn normalize(kind: &str, v: Value) -> Result<Value> {
    let original = v.clone();
    normalize_inner(kind, v).map_err(|mut error| {
        if let Some(Value::Array(items)) = error.detail.as_mut() {
            for item in items {
                if item["loc"] == json!(["body"]) {
                    item["input"] = original.clone();
                }
            }
        }
        error
    })
}
fn normalize_inner(kind: &str, mut v: Value) -> Result<Value> {
    if !v.is_object() {
        return Err(Error::invalid("请求必须是对象"));
    }
    let (fields, defaults): (&[&str], Value) = match kind {
        "create" => (
            &[
                "account_id",
                "name",
                "cost",
                "creditor_id",
                "start",
                "start_mode",
                "cost_kind",
            ],
            json!({"start":null,"start_mode":"exact","cost_kind":"exact"}),
        ),
        "edit" => (
            &[
                "account_id",
                "name",
                "cost",
                "creditor_id",
                "start",
                "start_mode",
                "cost_kind",
                "version",
            ],
            json!({"start":null,"start_mode":"exact","cost_kind":null}),
        ),
        "start-confirm" => (
            &["version", "start", "method", "source_usage_id"],
            json!({"method":"legacy","source_usage_id":null}),
        ),
        "free" => (&["name", "version"], json!({"version":null})),
        "upgrade-paid" => (
            &["name", "cost", "creditor_id", "cost_kind", "version"],
            json!({"cost_kind":"exact"}),
        ),
        "preview" => (
            &["ids", "expense_ids", "end"],
            json!({"ids":[],"expense_ids":[]}),
        ),
        "create-expense" => (
            &[
                "name",
                "cost",
                "incurred_on",
                "creditor_id",
                "allocation_mode",
                "members",
            ],
            json!({"allocation_mode":"equal"}),
        ),
        "edit-expense" => (
            &[
                "name",
                "cost",
                "incurred_on",
                "creditor_id",
                "allocation_mode",
                "members",
                "version",
            ],
            json!({"allocation_mode":"equal"}),
        ),
        "bill-preview" => (
            &[
                "version",
                "end",
                "csv",
                "file_name",
                "import_id",
                "allocation_mode",
            ],
            json!({"csv":null,"file_name":"usage.csv","import_id":null,"allocation_mode":null}),
        ),
        "bill-confirm" => (
            &[
                "version",
                "preview_id",
                "accept_difference",
                "accept_outside",
            ],
            json!({"accept_difference":false,"accept_outside":false}),
        ),
        "settle" => (
            &[
                "preview_id",
                "renewals",
                "delete_cycle_ids",
                "delete_scopes",
            ],
            json!({"renewals":[],"delete_cycle_ids":[]}),
        ),
        "transition" => (&["action", "version"], json!({})),
        "netting-payment" => (
            &["user_a", "user_b", "digest", "note", "reference"],
            json!({}),
        ),
        "merge" => (&["ids"], json!({})),
        "delete-retry" => (&["version", "scope_digest"], json!({})),
        "delete-cancel" => (&["version"], json!({})),
        "renewal" => (
            &["cycle_id", "cost", "creditor_id", "cost_kind"],
            json!({"cost_kind":null}),
        ),
        "member" => (&["user_id", "percent"], json!({"percent":null})),
        _ => return Ok(json!({})),
    };
    let Some(object) = v.as_object() else {
        return Err(Error::invalid("请求必须是对象"));
    };
    for (k, val) in object {
        if !fields.contains(&k.as_str()) {
            return Err(issue(
                "extra_forbidden",
                k,
                "Extra inputs are not permitted",
                val,
                None,
            ));
        }
    }
    let Some(defaults) = defaults.as_object() else {
        return Err(Error::unavailable());
    };
    for (k, val) in defaults {
        if v.get(k).is_none() {
            v[k] = val.clone();
        }
    }
    for k in fields {
        if !matches!(
            (kind, *k),
            ("netting-payment", "note" | "reference")
                | ("settle", "delete_scopes")
                | ("delete-retry", "scope_digest")
        ) {
            required(&v, k)?;
        }
    }
    for k in [
        "account_id",
        "creditor_id",
        "cycle_id",
        "user_id",
        "user_a",
        "user_b",
        "version",
        "source_usage_id",
        "import_id",
    ] {
        if fields.contains(&k)
            && !(v[k].is_null()
                && (["source_usage_id", "import_id"].contains(&k) || kind == "free"))
        {
            positive(&mut v, k)?;
        }
    }
    if fields.contains(&"name") {
        string(&v, "name", 1, 120)?;
    }
    if fields.contains(&"cost") {
        cost(&v, "cost", &COST)?;
    }
    if fields.contains(&"cost_kind") {
        choice(
            &v,
            "cost_kind",
            &["exact", "estimated"],
            matches!(kind, "edit" | "renewal"),
        )?;
    }
    if fields.contains(&"start_mode") {
        choice(
            &v,
            "start_mode",
            &[
                "exact",
                "date_estimate",
                "minute_estimate",
                "earliest_usage",
            ],
            false,
        )?;
    }
    if fields.contains(&"method") {
        choice(
            &v,
            "method",
            &["legacy", "provided_time", "first_usage_after"],
            false,
        )?;
    }
    for k in ["start", "end"] {
        if fields.contains(&k) && !v[k].is_null() {
            v[k] = json!(date_time(text(&v, k))?);
        }
    }
    if fields.contains(&"preview_id") {
        string(&v, "preview_id", 1, 64)?;
    }
    match kind {
        "preview" => {
            ids(&v, "ids", 0)?;
            ids(&v, "expense_ids", 0)?;
            if array(&v["ids"]).is_empty() && array(&v["expense_ids"]).is_empty() {
                return Err(issue(
                    "value_error",
                    "",
                    "Value error, 请选择周期或固定费用",
                    &v,
                    Some(json!({"error":{}})),
                ));
            }
        }
        "create-expense" | "edit-expense" => {
            choice(&v, "allocation_mode", &["equal", "custom"], false)?;
            let date = chrono::NaiveDate::parse_from_str(text(&v, "incurred_on"), "%Y-%m-%d")
                .map_err(|_| Error::invalid("费用日期无效"))?;
            v["incurred_on"] = json!(date.to_string());
            let members = v["members"]
                .as_array()
                .ok_or_else(|| Error::invalid("参与用户无效"))?;
            if members.is_empty() || members.len() > 500 {
                return Err(Error::invalid("参与用户数量无效"));
            }
            v["members"] = json!(
                members
                    .iter()
                    .cloned()
                    .map(|m| normalize("member", m))
                    .collect::<Result<Vec<_>>>()?
            );
        }
        "member" => {
            if !v["percent"].is_null() {
                cost(&v, "percent", &PERCENT)?;
            }
        }
        "bill-preview" => {
            choice(
                &v,
                "allocation_mode",
                &["proportional", "matched_only"],
                true,
            )?;
            string(&v, "file_name", 0, 200)?;
            if !v["csv"].is_null() {
                string(&v, "csv", 0, 10 * 1024 * 1024)?;
            }
        }
        "bill-confirm" => {
            for k in ["accept_difference", "accept_outside"] {
                if !v[k].is_boolean() {
                    return Err(Error::invalid("确认项无效"));
                }
            }
        }
        "settle" => {
            ids(&v, "delete_cycle_ids", 0)?;
            if let Some(scopes) = v.get("delete_scopes") {
                let scopes = scopes
                    .as_object()
                    .filter(|m| m.len() <= 100)
                    .ok_or_else(|| Error::invalid("删除范围无效"))?;
                let delete_ids = array(&v["delete_cycle_ids"]);
                if scopes.len() != delete_ids.len() {
                    return Err(Error::invalid("请确认每个账号的删除范围"));
                }
                for (key, value) in scopes {
                    if key
                        .parse::<i64>()
                        .ok()
                        .is_none_or(|id| key != &id.to_string() || !delete_ids.contains(&json!(id)))
                        || !value.as_str().is_some_and(|v| {
                            v.len() == 64
                                && v.bytes()
                                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        })
                    {
                        return Err(Error::invalid("删除范围无效"));
                    }
                }
            }
            let r = v["renewals"]
                .as_array()
                .ok_or_else(|| Error::invalid("续期数据无效"))?;
            if r.len() > 100 {
                return Err(Error::invalid("续期数量无效"));
            }
            let r = r
                .iter()
                .cloned()
                .map(|r| normalize("renewal", r))
                .collect::<Result<Vec<_>>>()?;
            if r.iter()
                .map(|r| int(r, "cycle_id"))
                .collect::<BTreeSet<_>>()
                .len()
                != r.len()
            {
                return Err(Error::invalid("续期重复"));
            }
            v["renewals"] = json!(r);
            if array(&v["delete_cycle_ids"]).is_empty() {
                v.as_object_mut()
                    .ok_or_else(|| Error::invalid("请求必须是对象"))?
                    .remove("delete_cycle_ids");
            }
        }
        "transition" => choice(
            &v,
            "action",
            &["submit", "mark_paid", "confirm", "reject", "split"],
            false,
        )?,
        "netting-payment" => {
            cost(&v, "digest", &DIGEST)?;
            for (key, max) in [("note", 500), ("reference", 128)] {
                if v.get(key).is_some() {
                    string(&v, key, 0, max)?;
                    if text(&v, key).chars().any(|c| {
                        c.is_control() && !(key == "note" && ['\n', '\r', '\t'].contains(&c))
                    }) {
                        return Err(Error::invalid("付款备注或凭证编号包含无效字符"));
                    }
                }
            }
        }
        "delete-retry" => {
            if v.get("scope_digest").is_some() {
                cost(&v, "scope_digest", &DIGEST)?;
            }
        }
        "merge" => {
            ids(&v, "ids", 1)?;
            if array(&v["ids"]).len() < 2 {
                return Err(Error::invalid("至少选择两张订单"));
            }
        }
        _ => {}
    }
    Ok(v)
}
pub fn timestamp(s: &str) -> Result<String> {
    time::instant(s)
}

pub fn query_choices(
    query: &std::collections::BTreeMap<String, String>,
    fields: &[(&str, &[&str])],
) -> Result<()> {
    let mut errors = Vec::new();
    for (name, values) in fields {
        if let Some(value) = query.get(*name)
            && let Err(e) = choice(&json!({*name:value}), name, values, false)
        {
            for mut detail in e
                .detail
                .into_iter()
                .flat_map(|d| d.as_array().cloned().unwrap_or_default())
            {
                detail["loc"][0] = json!("query");
                errors.push(detail);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::validation(json!(errors)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_objects_and_empty_choice_definitions_return_errors() {
        for value in [Value::Null, json!([]), json!(42), json!("object")] {
            let error = normalize("create", value).unwrap_err();
            assert_eq!(error.status, 422);
            assert_eq!(error.message, "请求必须是对象");
        }
        let error = normalize(
            "create-expense",
            json!({
                "name": "Fixture", "cost": "1.00", "incurred_on": "2026-01-01",
                "creditor_id": 1, "members": [null]
            }),
        )
        .unwrap_err();
        assert_eq!(error.status, 422);
        assert_eq!(error.message, "请求必须是对象");
        assert!(choice(&json!({"state": "x"}), "state", &[], false).is_err());
    }

    #[test]
    fn cached_patterns_preserve_validation_boundaries_and_error_details() {
        for value in ["0", "100", "9999999999.99", "0.01"] {
            cost(&json!({"cost": value}), "cost", &COST).unwrap();
        }
        for value in ["01", "-1", "1.001", "10000000000", "1\n"] {
            let error = cost(&json!({"cost": value}), "cost", &COST).unwrap_err();
            assert_eq!(error.status, 422);
            assert_eq!(
                error.detail,
                Some(json!([{
                    "type": "string_pattern_mismatch", "loc": ["body", "cost"],
                    "msg": "String should match pattern '^(0|[1-9][0-9]{0,9})(\\.[0-9]{1,2})?$'",
                    "input": value, "ctx": {"pattern": "^(0|[1-9][0-9]{0,9})(\\.[0-9]{1,2})?$"}
                }]))
            );
        }
        for value in ["0", "33.33", "100.00", "999.99"] {
            normalize("member", json!({"user_id": 1, "percent": value})).unwrap();
        }
        for value in ["0.001", "1000", "01"] {
            assert!(normalize("member", json!({"user_id": 1, "percent": value})).is_err());
        }
        for kind in ["netting-payment", "delete-retry"] {
            let key = if kind == "netting-payment" {
                "digest"
            } else {
                "scope_digest"
            };
            let payload = if kind == "netting-payment" {
                json!({"user_a": 1, "user_b": 2, key: "a".repeat(64)})
            } else {
                json!({"version": 1, key: "a".repeat(64)})
            };
            normalize(kind, payload.clone()).unwrap();
            for invalid in ["a".repeat(63), "a".repeat(65), "A".repeat(64)] {
                let mut invalid_payload = payload.clone();
                invalid_payload[key] = json!(invalid);
                let error = normalize(kind, invalid_payload).unwrap_err();
                assert_eq!(error.status, 422);
                assert_eq!(error.detail.unwrap()[0]["ctx"]["pattern"], "^[0-9a-f]{64}$");
            }
        }
    }
}
