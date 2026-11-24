use crate::{
    error::{Error, Result},
    json::{billing_digest, int, merge, text},
    money::{Decimal, allocate, allocate_matched, valid_decimal_ascii},
    time,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
const CSV_TOKENS: [&str; 4] = [
    "Input Tokens",
    "Output Tokens",
    "Cache Read Tokens",
    "Cache Creation Tokens",
];
pub fn sanitize_file_name(raw: &str) -> String {
    let name = if raw.is_empty() { "usage.csv" } else { raw };
    name.replace('\\', "/")
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("usage.csv")
        .chars()
        .take(200)
        .collect()
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
    let idx_time = fields["Time"];
    let idx_cost = fields["Billed Cost"];
    let idx_model = fields["Model"];
    let idx_req = fields.get("Request ID").copied();
    let idx_up_req = fields.get("Upstream Request ID").copied();
    let idx_tokens = CSV_TOKENS.map(|label| fields[label]);
    let mut rows = Vec::new();
    for (index, r) in reader.records().enumerate() {
        let line = index + 2;
        let r = r.map_err(|_| Error::invalid(format!("CSV 第 {line} 行：列数不一致")))?;
        let physical_line = r.position().map(|p| p.line() as usize).unwrap_or(line)
            + r.iter()
                .map(|s| s.bytes().filter(|b| *b == b'\n').count())
                .sum::<usize>();
        let parse = || -> Result<BillRow> {
            if line > 50001 {
                return Err(Error::invalid("CSV 最多支持 50000 条请求"));
            }
            let amount = r.get(idx_cost).unwrap_or("").trim();
            if !valid_decimal_ascii(amount, 10, 18, true) {
                return Err(Error::invalid("Billed Cost 必须为非负十进制金额"));
            }
            let model = r.get(idx_model).unwrap_or("").trim();
            if model.is_empty() || (model.len() > 200 && model.chars().count() > 200) {
                return Err(Error::invalid("Model 无效"));
            }
            let raw_rid = idx_req.and_then(|i| r.get(i)).unwrap_or("");
            let rid = if !raw_rid.is_empty() {
                raw_rid.trim()
            } else {
                idx_up_req.and_then(|i| r.get(i)).unwrap_or("").trim()
            };
            let created_at = time::instant(r.get(idx_time).unwrap_or("").trim())?;
            let amount = Decimal::parse(amount)?.to_string();
            let mut tokens = [0_i64; 4];
            for ((label, col), out) in CSV_TOKENS.iter().zip(idx_tokens).zip(&mut tokens) {
                let v = r.get(col).unwrap_or("").trim();
                if v.is_empty() || v.len() > 12 || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::invalid(format!("{label} 必须为非负整数")));
                }
                *out = v
                    .parse::<i64>()
                    .map_err(|_| Error::invalid("Token 数无效"))?;
            }
            Ok(BillRow {
                amount,
                cache_creation_tokens: tokens[3],
                cache_read_tokens: tokens[2],
                created_at,
                input_tokens: tokens[0],
                model: model.to_owned(),
                output_tokens: tokens[1],
                request_id: rid.to_owned(),
                row: line as i64,
            })
        };
        rows.push(
            parse()
                .map_err(|e| Error::invalid(format!("CSV 第 {physical_line} 行：{}", e.message)))?,
        );
    }
    if rows.is_empty() {
        return Err(Error::invalid("CSV 没有请求记录"));
    }
    Ok(rows)
}
// Field order preserves Python's canonical sorted-key serialization.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
impl BillRow {
    pub(crate) fn tokens(&self) -> [i64; 4] {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_creation_tokens,
        ]
    }
}
// Field order preserves canonical sorted-key serialization in usage_details.payload.
#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub struct RequestDetail {
    pub cache_creation_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub model: Option<String>,
    pub output_tokens: Option<i64>,
    pub request_id: Option<String>,
    pub requested_model: Option<String>,
    pub upstream_model: Option<String>,
    pub upstream_request_id: Option<String>,
    pub upstream_response_model: Option<String>,
}
impl RequestDetail {
    pub fn is_empty(&self) -> bool {
        self.cache_creation_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.input_tokens.is_none()
            && self.model.is_none()
            && self.output_tokens.is_none()
            && self.request_id.is_none()
            && self.requested_model.is_none()
            && self.upstream_model.is_none()
            && self.upstream_request_id.is_none()
            && self.upstream_response_model.is_none()
    }
    pub(crate) fn tokens(&self) -> Option<[i64; 4]> {
        Some([
            self.input_tokens?,
            self.output_tokens?,
            self.cache_read_tokens?,
            self.cache_creation_tokens?,
        ])
    }
    pub(crate) fn unique_request_ids(&self) -> impl Iterator<Item = &str> {
        let a = self.request_id.as_deref().unwrap_or("");
        let b = self.upstream_request_id.as_deref().unwrap_or("");
        [
            (!a.is_empty()).then_some(a),
            (!b.is_empty() && b != a).then_some(b),
        ]
        .into_iter()
        .flatten()
    }
    pub(crate) fn unique_models(&self) -> impl Iterator<Item = &str> {
        let raw = [
            self.model.as_deref().unwrap_or(""),
            self.requested_model.as_deref().unwrap_or(""),
            self.upstream_model.as_deref().unwrap_or(""),
            self.upstream_response_model.as_deref().unwrap_or(""),
        ];
        let mut out = [""; 4];
        let mut len = 0;
        for m in raw {
            if !m.is_empty() && !out[..len].contains(&m) {
                out[len] = m;
                len += 1;
            }
        }
        out.into_iter().take(len)
    }
}
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Signature<'a> {
    model: &'a str,
    tokens: [i64; 4],
}
// Field order preserves canonical sorted-key serialization.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageRow {
    #[serde(default)]
    pub account_id: i64,
    #[serde(default)]
    pub amount: String,
    pub created_at: String,
    pub id: i64,
    #[serde(default)]
    pub root_id: i64,
    pub user_id: i64,
}
pub fn reconcile(
    rows: &[BillRow],
    usage: &[UsageRow],
    details: &BTreeMap<i64, RequestDetail>,
    start: &str,
    end: &str,
    mode: &str,
) -> Result<Value> {
    reconcile_inner(rows, usage, details, start, end, mode, false)
}
// Diagnostics are opt-in: legacy result serialization and billing digests stay unchanged.
pub(crate) fn reconcile_inner(
    rows: &[BillRow],
    usage: &[UsageRow],
    details: &BTreeMap<i64, RequestDetail>,
    start: &str,
    end: &str,
    mode: &str,
    diagnostics: bool,
) -> Result<Value> {
    if !["proportional", "matched_only"].contains(&mode) {
        return Err(Error::invalid("账单分摊方式无效"));
    }
    let mut included: Vec<(&BillRow, Decimal)> = Vec::with_capacity(rows.len());
    let mut outside: Vec<&BillRow> = Vec::new();
    let mut outside_total = Decimal::zero();
    for r in rows {
        let amt = Decimal::parse(&r.amount)?;
        if r.created_at.as_str() >= start && r.created_at.as_str() < end {
            included.push((r, amt));
        } else {
            outside_total.add(&amt);
            if !diagnostics {
                outside_total = outside_total.rounded(60);
            }
            outside.push(r);
        }
    }
    let mut by_id: HashMap<&str, Vec<usize>> = HashMap::with_capacity(usage.len());
    for (idx, local) in usage.iter().enumerate() {
        if let Some(detail) = details.get(&local.id) {
            for rid in detail.unique_request_ids() {
                by_id.entry(rid).or_default().push(idx);
            }
        }
    }
    let mut identified: Vec<Option<&[usize]>> = Vec::with_capacity(included.len());
    let mut id_uses = vec![0_u32; usage.len()];
    let mut identified_count = 0_usize;
    for (r, _) in &included {
        if let Some(g) = by_id.get(r.request_id.as_str()) {
            identified.push(Some(g.as_slice()));
            identified_count += 1;
            for &u in g {
                id_uses[u] += 1;
            }
        } else {
            identified.push(None);
        }
    }
    let mut fallback: Vec<Vec<usize>> = vec![Vec::new(); included.len()];
    let mut fallback_uses = vec![0_u32; usage.len()];
    if identified_count < included.len() {
        let mut reserved = vec![false; usage.len()];
        for g in identified.iter().flatten() {
            if g.len() == 1 && id_uses[g[0]] == 1 {
                reserved[g[0]] = true;
            }
        }
        let mut by_tokens: HashMap<Signature<'_>, Vec<(i64, usize)>> = HashMap::new();
        for (idx, local) in usage.iter().enumerate() {
            if reserved[idx] {
                continue;
            }
            let Some(detail) = details.get(&local.id) else {
                continue;
            };
            let Some(tokens) = detail.tokens() else {
                continue;
            };
            let stamp = time::timestamp_micros(time::parse(&local.created_at)?);
            for model in detail.unique_models() {
                by_tokens
                    .entry(Signature { model, tokens })
                    .or_default()
                    .push((stamp, idx));
            }
        }
        for group in by_tokens.values_mut() {
            group.sort_unstable();
        }
        for (i, (r, _)) in included.iter().enumerate() {
            if identified[i].is_some() {
                continue;
            }
            let stamp = time::timestamp_micros(time::parse(&r.created_at)?);
            if let Some(group) = by_tokens.get(&Signature {
                model: &r.model,
                tokens: r.tokens(),
            }) {
                let lo = group.partition_point(|(t, _)| *t < stamp - 5_000_000);
                let hi = group.partition_point(|(t, _)| *t <= stamp + 5_000_000);
                let found = &mut fallback[i];
                for &(_, u) in &group[lo..hi] {
                    found.push(u);
                    fallback_uses[u] += 1;
                }
            }
        }
    }
    let mut matched = Vec::with_capacity(included.len());
    let mut unmatched = Vec::new();
    let mut ambiguous = std::collections::BTreeSet::new();
    let mut weights: BTreeMap<i64, Decimal> = BTreeMap::new();
    let mut total = Decimal::zero();
    for (i, (r, amt)) in included.iter().enumerate() {
        total.add(amt);
        if !diagnostics {
            total = total.rounded(60);
        }
        let (group, uses): (&[usize], &[u32]) = if let Some(g) = identified[i] {
            (g, &id_uses)
        } else {
            (&fallback[i], &fallback_uses)
        };
        if group.len() == 1 && uses[group[0]] == 1 {
            let local = &usage[group[0]];
            matched.push(merge(
                serde_json::to_value(r)?,
                json!({"usage_id":local.id,"user_id":local.user_id}),
            ));
            let weight = weights.entry(local.user_id).or_insert_with(Decimal::zero);
            weight.add(amt);
            if !diagnostics {
                *weight = weight.rounded(60);
            }
        } else {
            if diagnostics {
                ambiguous.extend(group.iter().map(|i| usage[*i].id));
            }
            unmatched.push(merge(
                serde_json::to_value(r)?,
                json!({"reason":if group.is_empty(){"missing"}else{"ambiguous"}}),
            ));
        }
    }
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
    let mut result = json!({"total":total.to_string(),"cost_fen":cost,"matched_amount":matched_amount.to_string(),"unmatched_amount":unmatched_amount,"matched_count":matched.len(),"unmatched_count":unmatched.len(),"outside_count":outside.len(),"outside_amount":outside_total.to_string(),"local_unmatched_count":usage.len()-matched.len(),"weights":weight_values,"outside":outside,"record_count":included.len()});
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_inner_preserves_reserved_isolation_and_collision_semantics() {
        let start = "2026-01-01T00:00:00.000000+00:00";
        let end = "2026-01-02T00:00:00.000000+00:00";
        let ts = "2026-01-01T12:00:00.000000+00:00";
        let usage = vec![
            UsageRow {
                id: 1,
                account_id: 100,
                root_id: 100,
                user_id: 10,
                amount: "1.00".into(),
                created_at: ts.into(),
            },
            UsageRow {
                id: 2,
                account_id: 100,
                root_id: 100,
                user_id: 20,
                amount: "2.00".into(),
                created_at: ts.into(),
            },
            UsageRow {
                id: 3,
                account_id: 100,
                root_id: 100,
                user_id: 20,
                amount: "2.00".into(),
                created_at: ts.into(),
            },
            UsageRow {
                id: 4,
                account_id: 100,
                root_id: 100,
                user_id: 30,
                amount: "3.00".into(),
                created_at: ts.into(),
            },
            UsageRow {
                id: 5,
                account_id: 100,
                root_id: 100,
                user_id: 40,
                amount: "4.00".into(),
                created_at: ts.into(),
            },
        ];
        let detail = |rid: Option<&str>, m: &str, inp: i64| RequestDetail {
            request_id: rid.map(str::to_owned),
            model: Some(m.to_owned()),
            input_tokens: Some(inp),
            output_tokens: Some(5),
            cache_read_tokens: Some(0),
            cache_creation_tokens: Some(0),
            ..Default::default()
        };
        let details = BTreeMap::from([
            (1, detail(Some("req-unique"), "gpt-4o", 10)),
            (2, detail(Some("req-dup"), "gpt-4o", 20)),
            (3, detail(Some("req-dup"), "gpt-4o", 20)),
            (4, detail(None, "gpt-4o", 30)),
            (5, detail(None, "gpt-4o", 40)),
        ]);
        let bill = |row: i64, rid: &str, inp: i64, amt: &str| BillRow {
            row,
            request_id: rid.into(),
            model: "gpt-4o".into(),
            input_tokens: inp,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            amount: amt.into(),
            created_at: ts.into(),
        };
        let rows = vec![
            // 1. Unique request_id match reserves usage #1.
            bill(2, "req-unique", 10, "1.00"),
            // 2. Same tokens/model without request_id must NOT steal reserved usage #1 -> missing.
            bill(3, "", 10, "0.50"),
            // 3. Duplicate local request_id (usage #2 & #3) is ambiguous and NOT reserved.
            bill(4, "req-dup", 20, "2.00"),
            // 4. Unknown non-empty request_id falls back to by_tokens and uniquely matches usage #4.
            bill(5, "req-unknown-upstream", 30, "3.00"),
            // 5 & 6. Two request_id-less rows competing for usage #5 -> both ambiguous on fallback_uses.
            bill(6, "", 40, "4.00"),
            bill(7, "", 40, "4.00"),
        ];
        let res = reconcile_inner(&rows, &usage, &details, start, end, "matched_only", true)
            .expect("reconcile mixed fallback cases");
        assert_eq!(res["matched_count"], 2);
        assert_eq!(res["unmatched_count"], 4);
        assert_eq!(res["matched_amount"], "4.00");
        let matched = res["matched"].as_array().unwrap();
        assert_eq!(matched[0]["usage_id"], 1);
        assert_eq!(matched[1]["usage_id"], 4);
        let unmatched = res["unmatched"].as_array().unwrap();
        assert_eq!(unmatched[0]["row"], 3);
        assert_eq!(unmatched[0]["reason"], "missing");
        assert_eq!(unmatched[1]["row"], 4);
        assert_eq!(unmatched[1]["reason"], "ambiguous");
        assert_eq!(unmatched[2]["row"], 6);
        assert_eq!(unmatched[2]["reason"], "ambiguous");
        assert_eq!(unmatched[3]["row"], 7);
        assert_eq!(unmatched[3]["reason"], "ambiguous");
        assert_eq!(res["ambiguous_usage_ids"], json!([2, 3, 5]));
    }
}
