use crate::{
    error::{Error, Result},
    json::{array, int, text},
    time,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
const COST: &str = r"^(0|[1-9][0-9]{0,9})(\.[0-9]{1,2})?$";
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
            values[..values.len() - 1]
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", "),
            values.last().unwrap()
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
fn cost(v: &Value, k: &str, pattern: &str) -> Result<()> {
    string(v, k, 0, usize::MAX)?;
    if !regex::Regex::new(pattern).unwrap().is_match(text(v, k)) {
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
            &["preview_id", "renewals", "delete_cycle_ids"],
            json!({"renewals":[],"delete_cycle_ids":[]}),
        ),
        "transition" => (&["action", "version"], json!({})),
        "netting-payment" => (&["user_a", "user_b", "digest"], json!({})),
        "merge" => (&["ids"], json!({})),
        "delete-retry" | "delete-cancel" => (&["version"], json!({})),
        "renewal" => (
            &["cycle_id", "cost", "creditor_id", "cost_kind"],
            json!({"cost_kind":null}),
        ),
        "member" => (&["user_id", "percent"], json!({"percent":null})),
        _ => return Ok(json!({})),
    };
    for (k, val) in v.as_object().unwrap() {
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
    for (k, val) in defaults.as_object().unwrap() {
        if v.get(k).is_none() {
            v[k] = val.clone();
        }
    }
    for k in fields {
        required(&v, k)?;
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
        cost(&v, "cost", COST)?;
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
                cost(&v, "percent", r"^(0|[1-9][0-9]{0,2})(\.[0-9]{1,2})?$")?;
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
                v.as_object_mut().unwrap().remove("delete_cycle_ids");
            }
        }
        "transition" => choice(
            &v,
            "action",
            &["submit", "mark_paid", "confirm", "reject", "split"],
            false,
        )?,
        "netting-payment" => cost(&v, "digest", r"^[0-9a-f]{64}$")?,
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
