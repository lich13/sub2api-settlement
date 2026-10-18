use crate::{
    error::{Error, Result},
    json::{billing_digest, int, merge, text},
    money::{Decimal, allocate, allocate_matched},
    time,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
pub const DETAIL_FIELDS: [&str; 10] = [
    "request_id",
    "upstream_request_id",
    "model",
    "requested_model",
    "upstream_model",
    "upstream_response_model",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
];
pub const TOKEN_FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
];
const CSV_TOKENS: [&str; 4] = [
    "Input Tokens",
    "Output Tokens",
    "Cache Read Tokens",
    "Cache Creation Tokens",
];
pub fn parse_csv(input: &str) -> Result<Value> {
    Ok(serde_json::to_value(parse_csv_rows(input)?)?)
}
pub fn parse_csv_rows(input: &str) -> Result<Vec<BillRow>> {
    if input.len() > 10 * 1024 * 1024 {
        return Err(Error::invalid("CSV 不能超过 10 MB"));
    }
    let mut reader = csv::ReaderBuilder::new()
        .flexible(false)
        .from_reader(input.trim_start_matches('\u{feff}').as_bytes());
    let header = reader
        .headers()
        .map_err(|_| Error::invalid("CSV 缺少必要字段或列名重复"))?
        .clone();
    let fields: HashMap<_, _> = header.iter().enumerate().map(|(i, h)| (h, i)).collect();
    let required = [
        "Time",
        "Billed Cost",
        "Model",
        CSV_TOKENS[0],
        CSV_TOKENS[1],
        CSV_TOKENS[2],
        CSV_TOKENS[3],
    ];
    if fields.len() != header.len() || required.iter().any(|f| !fields.contains_key(f)) {
        return Err(Error::invalid("CSV 缺少必要字段或列名重复"));
    }
    let mut rows = Vec::new();
    for (index, r) in reader.records().enumerate() {
        let line = index + 2;
        let r = r.map_err(|_| Error::invalid(format!("CSV 第 {line} 行：列数不一致")))?;
        let physical_line = r.position().map(|p| p.line() as usize).unwrap_or(line)
            + r.iter()
                .map(|s| s.bytes().filter(|b| *b == b'\n').count())
                .sum::<usize>();
        let parse = || -> Result<Value> {
            if line > 50001 {
                return Err(Error::invalid("CSV 最多支持 50000 条请求"));
            }
            let get = |key: &str| fields.get(key).and_then(|i| r.get(*i)).unwrap_or("").trim();
            let amount = get("Billed Cost");
            let mut p = amount.split('.');
            let a = p.next().unwrap_or("");
            let b = p.next();
            if a.is_empty()
                || a.len() > 10
                || !a.bytes().all(|x| x.is_ascii_digit())
                || b.is_some_and(|v| {
                    v.is_empty() || v.len() > 18 || !v.bytes().all(|x| x.is_ascii_digit())
                })
                || p.next().is_some()
            {
                return Err(Error::invalid("Billed Cost 必须为非负十进制金额"));
            }
            let model = get("Model");
            if model.is_empty() || model.chars().count() > 200 {
                return Err(Error::invalid("Model 无效"));
            }
            let raw_rid = fields
                .get("Request ID")
                .and_then(|i| r.get(*i))
                .unwrap_or("");
            let rid = if !raw_rid.is_empty() {
                raw_rid.trim()
            } else {
                get("Upstream Request ID")
            };
            let mut record = json!({"row":line,"created_at":time::instant(get("Time"))?,"amount":Decimal::parse(amount)?.to_string(),"model":model,"request_id":rid});
            for (label, field) in CSV_TOKENS.iter().zip(TOKEN_FIELDS) {
                let v = get(label);
                if v.is_empty() || v.len() > 12 || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::invalid(format!("{label} 必须为非负整数")));
                }
                record[field] = json!(
                    v.parse::<i64>()
                        .map_err(|_| Error::invalid("Token 数无效"))?
                );
            }
            Ok(record)
        };
        rows.push(serde_json::from_value(parse().map_err(|e| {
            Error::invalid(format!("CSV 第 {physical_line} 行：{}", e.message))
        })?)?);
    }
    if rows.is_empty() {
        return Err(Error::invalid("CSV 没有请求记录"));
    }
    Ok(rows)
}
// Field order preserves Python's canonical sorted-key serialization.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BillRow {
    pub amount: String,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub created_at: String,
    pub input_tokens: i64,
    pub model: String,
    pub output_tokens: i64,
    pub request_id: String,
    pub row: i64,
}
#[derive(serde::Deserialize, Default)]
#[serde(default)]
pub struct RequestDetail {
    pub request_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub upstream_model: Option<String>,
    pub upstream_response_model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
}
pub trait MatchFields {
    fn field(&self, key: &str) -> &str;
    fn number(&self, key: &str) -> Option<i64>;
}
impl MatchFields for Value {
    fn field(&self, key: &str) -> &str {
        text(self, key)
    }
    fn number(&self, key: &str) -> Option<i64> {
        (!self[key].is_null()).then(|| int(self, key))
    }
}
impl MatchFields for BillRow {
    fn field(&self, key: &str) -> &str {
        match key {
            "amount" => &self.amount,
            "created_at" => &self.created_at,
            "model" => &self.model,
            "request_id" => &self.request_id,
            _ => "",
        }
    }
    fn number(&self, key: &str) -> Option<i64> {
        match key {
            "row" => Some(self.row),
            "input_tokens" => Some(self.input_tokens),
            "output_tokens" => Some(self.output_tokens),
            "cache_read_tokens" => Some(self.cache_read_tokens),
            "cache_creation_tokens" => Some(self.cache_creation_tokens),
            _ => None,
        }
    }
}
impl MatchFields for RequestDetail {
    fn field(&self, key: &str) -> &str {
        match key {
            "request_id" => self.request_id.as_deref(),
            "upstream_request_id" => self.upstream_request_id.as_deref(),
            "model" => self.model.as_deref(),
            "requested_model" => self.requested_model.as_deref(),
            "upstream_model" => self.upstream_model.as_deref(),
            "upstream_response_model" => self.upstream_response_model.as_deref(),
            _ => None,
        }
        .unwrap_or("")
    }
    fn number(&self, key: &str) -> Option<i64> {
        match key {
            "input_tokens" => self.input_tokens,
            "output_tokens" => self.output_tokens,
            "cache_read_tokens" => self.cache_read_tokens,
            "cache_creation_tokens" => self.cache_creation_tokens,
            _ => None,
        }
    }
}
#[derive(Clone, Hash, PartialEq, Eq)]
struct Signature {
    model: String,
    tokens: [i64; 4],
}
fn signature<T: MatchFields>(r: &T, model: &str) -> Signature {
    Signature {
        model: model.to_owned(),
        tokens: TOKEN_FIELDS.map(|k| r.number(k).unwrap_or(0)),
    }
}
#[derive(serde::Serialize)]
pub struct UsageRow {
    pub account_id: i64,
    pub amount: String,
    pub created_at: String,
    pub id: i64,
    pub root_id: i64,
    pub user_id: i64,
}
pub trait UsageRecord {
    fn id(&self) -> i64;
    fn user_id(&self) -> i64;
    fn created_at(&self) -> &str;
}
impl UsageRecord for UsageRow {
    fn id(&self) -> i64 {
        self.id
    }
    fn user_id(&self) -> i64 {
        self.user_id
    }
    fn created_at(&self) -> &str {
        &self.created_at
    }
}
impl UsageRecord for Value {
    fn id(&self) -> i64 {
        int(self, "id")
    }
    fn user_id(&self) -> i64 {
        int(self, "user_id")
    }
    fn created_at(&self) -> &str {
        text(self, "created_at")
    }
}
pub fn reconcile<B: MatchFields + serde::Serialize, U: UsageRecord, D: MatchFields>(
    rows: &[B],
    usage: &[U],
    details: &BTreeMap<i64, D>,
    start: &str,
    end: &str,
    mode: &str,
) -> Result<Value> {
    reconcile_inner(rows, usage, details, start, end, mode, false)
}
// Diagnostics are opt-in: legacy result serialization and billing digests stay unchanged.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconcile_inner<B: MatchFields + serde::Serialize, U: UsageRecord, D: MatchFields>(
    rows: &[B],
    usage: &[U],
    details: &BTreeMap<i64, D>,
    start: &str,
    end: &str,
    mode: &str,
    diagnostics: bool,
) -> Result<Value> {
    if !["proportional", "matched_only"].contains(&mode) {
        return Err(Error::invalid("账单分摊方式无效"));
    }
    let (included, outside): (Vec<_>, Vec<_>) = rows
        .iter()
        .partition(|r| r.field("created_at") >= start && r.field("created_at") < end);
    let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_tokens: HashMap<Signature, Vec<(i64, usize)>> = HashMap::new();
    for (idx, local) in usage.iter().enumerate() {
        let Some(detail) = details.get(&local.id()) else {
            continue;
        };
        for rid in [
            detail.field("request_id"),
            detail.field("upstream_request_id"),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<HashSet<_>>()
        {
            by_id.entry(rid).or_default().push(idx);
        }
        if TOKEN_FIELDS.iter().any(|k| detail.number(k).is_none()) {
            continue;
        }
        let stamp = time::parse(local.created_at())?.timestamp_micros();
        for model in [
            "model",
            "requested_model",
            "upstream_model",
            "upstream_response_model",
        ]
        .iter()
        .map(|k| detail.field(k))
        .filter(|m| !m.is_empty())
        .collect::<HashSet<_>>()
        {
            by_tokens
                .entry(signature(detail, model))
                .or_default()
                .push((stamp, idx));
        }
    }
    for group in by_tokens.values_mut() {
        group.sort_unstable();
    }
    let mut identified: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut id_uses: HashMap<usize, usize> = HashMap::new();
    for r in &included {
        if let Some(g) = by_id.get(r.field("request_id")) {
            identified.insert(r.number("row").unwrap_or(0), g.clone());
            for u in g {
                *id_uses.entry(*u).or_default() += 1;
            }
        }
    }
    let reserved: HashSet<_> = identified
        .values()
        .filter(|g| g.len() == 1 && id_uses[&g[0]] == 1)
        .map(|g| g[0])
        .collect();
    let mut fallback: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut fallback_uses: HashMap<usize, usize> = HashMap::new();
    for r in &included {
        if identified.contains_key(&r.number("row").unwrap_or(0)) {
            continue;
        }
        let stamp = time::parse(r.field("created_at"))?.timestamp_micros();
        let mut found = Vec::new();
        if let Some(group) = by_tokens.get(&signature(*r, r.field("model"))) {
            let lo = group.partition_point(|(t, _)| *t < stamp - 5_000_000);
            let hi = group.partition_point(|(t, _)| *t <= stamp + 5_000_000);
            for (_, u) in &group[lo..hi] {
                if !reserved.contains(u) {
                    found.push(*u);
                    *fallback_uses.entry(*u).or_default() += 1;
                }
            }
        }
        fallback.insert(r.number("row").unwrap_or(0), found);
    }
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    let mut ambiguous = std::collections::BTreeSet::new();
    let mut weights: BTreeMap<i64, Decimal> = BTreeMap::new();
    for r in &included {
        let (group, uses) = if let Some(g) = identified.get(&r.number("row").unwrap_or(0)) {
            (g, &id_uses)
        } else {
            (&fallback[&r.number("row").unwrap_or(0)], &fallback_uses)
        };
        if group.len() == 1 && uses[&group[0]] == 1 {
            let local = &usage[group[0]];
            matched.push(merge(
                serde_json::to_value(r)?,
                json!({"usage_id":local.id(),"user_id":local.user_id()}),
            ));
            let weight = weights.entry(local.user_id()).or_insert_with(Decimal::zero);
            weight.add(&Decimal::parse(r.field("amount"))?);
            if !diagnostics {
                *weight = weight.rounded(60);
            }
        } else {
            if diagnostics {
                ambiguous.extend(group.iter().map(|i| usage[*i].id()));
            }
            unmatched.push(merge(
                serde_json::to_value(r)?,
                json!({"reason":if group.is_empty(){"missing"}else{"ambiguous"}}),
            ));
        }
    }
    let sum = |records: &Vec<&B>| -> Result<Decimal> {
        let mut n = Decimal::zero();
        for r in records {
            n.add(&Decimal::parse(r.field("amount"))?);
            if !diagnostics {
                n = n.rounded(60);
            }
        }
        Ok(n)
    };
    let total = sum(&included)?;
    let mut matched_amount = Decimal::zero();
    for v in weights.values() {
        matched_amount.add(v);
        if !diagnostics {
            matched_amount = matched_amount.rounded(60);
        }
    }
    let cost = total.fen_half_up()?;
    if cost > 999_999_999_999 {
        return Err(Error::invalid("账单总额超出金额上限"));
    }
    let weight_values: serde_json::Map<_, _> = weights
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v.to_string())))
        .collect();
    let unmatched_amount = total.subtract(&matched_amount).to_string();
    let mut result = json!({"total":total.to_string(),"cost_fen":cost,"matched_amount":matched_amount.to_string(),"unmatched_amount":unmatched_amount,"matched_count":matched.len(),"unmatched_count":unmatched.len(),"outside_count":outside.len(),"outside_amount":sum(&outside)?.to_string(),"local_unmatched_count":usage.len()-matched.len(),"weights":weight_values,"outside":outside,"record_count":included.len()});
    result["matched"] = Value::Array(matched);
    result["unmatched"] = Value::Array(unmatched);
    if mode == "matched_only" {
        result["allocation_mode"] = json!(mode);
        result["rules_version"] = json!(2);
    }
    result["digest"] = json!(billing_digest(&result));
    if diagnostics {
        result["ambiguous_usage_ids"] = json!(ambiguous);
    }
    Ok(merge(
        result,
        json!({"allocation_mode":mode,"rules_version":if mode=="matched_only"{2}else{1},"billable_cost_fen":if mode=="matched_only"{matched_amount.fen_half_up()?}else{cost},"excluded_amount":if mode=="matched_only"{unmatched_amount}else{"0".to_owned()}}),
    ))
}
pub fn bill_allocations(result: &Value, creditor: i64) -> Result<Value> {
    if !result["manual_basis"].is_null() {
        return crate::manual_billing::allocations(result, creditor);
    }
    if !Decimal::parse(text(result, "matched_amount"))?.positive() {
        return Ok(json!([]));
    }
    let mut weights = BTreeMap::new();
    if let Some(m) = result["weights"].as_object() {
        for (k, v) in m {
            weights.insert(
                k.parse::<i64>().map_err(|_| Error::invalid("用户无效"))?,
                Decimal::parse(v.as_str().unwrap_or(""))?,
            );
        }
    }
    if text(result, "allocation_mode") == "matched_only" {
        allocate_matched(&weights, creditor)
    } else {
        allocate(int(result, "cost_fen"), &weights, creditor, 28)
    }
}
