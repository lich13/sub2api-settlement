//! Opt-in supplemental basis. Legacy billing never enters this rule version.
use crate::{
    billing::{self, BillRow, RequestDetail, UsageRow},
    error::{Error, Result},
    json::{array, digest, int, text},
    money::{Decimal, from_ratio},
};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{ToPrimitive, Zero};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn normalize(v: &Value) -> Result<Value> {
    let object = v
        .as_object()
        .ok_or_else(|| Error::invalid("补计依据无效"))?;
    if object
        .keys()
        .any(|k| !["source", "rates", "ambiguous_usage_ids"].contains(&k.as_str()))
        || !["upstream", "local_only"].contains(&text(v, "source"))
    {
        return Err(Error::invalid("补计来源无效"));
    }
    let rates = v["rates"]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(|| Error::invalid("账号倍率无效"))?;
    let mut normalized = BTreeMap::new();
    for r in rates {
        let id = r["account_id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::invalid("补计账号无效"))?;
        let rate = r["rate"]
            .as_str()
            .ok_or_else(|| Error::invalid("倍率须为十进制字符串"))?
            .trim();
        static RATE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"^(?:0|[1-9][0-9]*)(?:\.[0-9]{1,6})?$").expect("fixed rate pattern")
        });
        if !rate.is_empty() && (rate.len() > 10000 || !RATE.is_match(rate)) {
            return Err(Error::invalid("倍率须为非负数，最多六位小数"));
        }
        if r.as_object().is_none_or(|o| o.len() != 2)
            || normalized.insert(id, rate.to_owned()).is_some()
        {
            return Err(Error::invalid("补计账号重复或字段无效"));
        }
    }
    let ids = v["ambiguous_usage_ids"]
        .as_array()
        .filter(|a| a.len() <= 50000)
        .ok_or_else(|| Error::invalid("歧义请求选择无效"))?;
    let mut selected = BTreeSet::new();
    for id in ids {
        let id = id
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::invalid("歧义请求无效"))?;
        if !selected.insert(id) {
            return Err(Error::invalid("歧义请求重复"));
        }
    }
    Ok(
        json!({"source":v["source"],"rates":normalized.into_iter().filter(|(_,rate)| !rate.is_empty()).map(|(id,rate)|json!({"account_id":id,"rate":rate})).collect::<Vec<_>>(),"ambiguous_usage_ids":selected}),
    )
}

/// Matching remains identical; diagnostics only classify unmatched local candidates.
#[allow(clippy::too_many_arguments)]
pub fn reconcile(
    rows: &[BillRow],
    usage: &[UsageRow],
    details: &BTreeMap<i64, RequestDetail>,
    start: &str,
    end: &str,
    mode: &str,
    config: &Value,
    roots: &[i64],
) -> Result<Value> {
    let config = normalize(config)?;
    let local_only = config["source"] == "local_only";
    let mut result = billing::reconcile_inner(
        if local_only { &[] } else { rows },
        usage,
        details,
        start,
        end,
        mode,
        true,
    )?;
    let matched: BTreeSet<_> = array(&result["matched"])
        .iter()
        .map(|r| int(r, "usage_id"))
        .collect();
    let ambiguous: BTreeSet<_> = array(&result["ambiguous_usage_ids"])
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    let selected: BTreeSet<_> = array(&config["ambiguous_usage_ids"])
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    let local_ids: BTreeSet<_> = usage.iter().map(|u| u.id).collect();
    if selected
        .iter()
        .any(|id| !local_ids.contains(id) || (!ambiguous.contains(id) && !matched.contains(id)))
    {
        return Err(Error::invalid(
            "所选请求不属于本窗口的歧义或已匹配记录，请重新预览",
        ));
    }
    let mut rates = BTreeMap::new();
    for r in array(&config["rates"]) {
        let root = int(r, "account_id");
        if !roots.contains(&root) {
            return Err(Error::invalid("补计账号不属于本周期"));
        }
        rates.insert(root, Decimal::parse(text(r, "rate"))?);
    }
    let mut local = Vec::new();
    let mut weights = BTreeMap::<i64, Decimal>::new();
    let mut total = Decimal::zero();
    let mut summaries = BTreeMap::<i64, (usize, usize, Decimal, Decimal)>::new();
    for root in roots {
        summaries.insert(*root, (0, 0, Decimal::zero(), Decimal::zero()));
    }
    for u in usage.iter().filter(|u| !matched.contains(&u.id)) {
        let is_ambiguous = ambiguous.contains(&u.id);
        let enabled = rates.contains_key(&u.root_id) && (!is_ambiguous || selected.contains(&u.id));
        if selected.contains(&u.id) && !rates.contains_key(&u.root_id) {
            return Err(Error::invalid("请先填写所选歧义请求所属账号的倍率"));
        }
        let raw = Decimal::parse(&u.amount)?;
        let amount = if enabled {
            raw.multiply(&rates[&u.root_id])
        } else {
            Decimal::zero()
        };
        if enabled {
            weights
                .entry(u.user_id)
                .or_insert_with(Decimal::zero)
                .add(&amount);
            total.add(&amount);
        }
        let summary = summaries
            .get_mut(&u.root_id)
            .ok_or_else(Error::unavailable)?;
        summary.0 += 1;
        summary.1 += usize::from(!enabled);
        summary.2.add(&raw);
        summary.3.add(&amount);
        local.push(json!({"usage_id":u.id,"account_id":u.root_id,"source_account_id":u.account_id,"user_id":u.user_id,"created_at":u.created_at,"model":details.get(&u.id).and_then(|d|d.model.as_deref()).unwrap_or(""),"local_amount":u.amount,"rate":rates.get(&u.root_id).map(ToString::to_string),"amount":amount.to_string(),"reason":if is_ambiguous{"ambiguous"}else{"missing"},"included":enabled}));
    }
    local.sort_by_key(|r| int(r, "usage_id"));
    let can_confirm = if local_only {
        total.positive()
    } else {
        let matched = Decimal::parse(text(&result, "matched_amount"))?;
        let upstream_total = Decimal::parse(text(&result, "total"))?;
        let has_manual = total.positive();
        let valid_upstream_basis =
            mode != "proportional" || !upstream_total.positive() || matched.positive();
        valid_upstream_basis && (matched.positive() || has_manual)
    };
    result["manual_basis"] = config;
    result["manual_weights"] = json!(
        weights
            .into_iter()
            .map(|(id, v)| (id.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>()
    );
    result["manual_summary"] = json!({"source":if local_only{"local_only"}else{"upstream"},"amount":total.to_string(),"accounts":summaries.into_iter().map(|(id,(count,excluded,raw,amount))|json!({"account_id":id,"rate":rates.get(&id).map(ToString::to_string),"request_count":count,"excluded_count":excluded,"local_amount":raw.to_string(),"amount":amount.to_string()})).collect::<Vec<_>>()});
    result["local_unmatched"] = json!(local);
    result["can_confirm"] = json!(can_confirm);
    result["rules_version"] = json!("manual-billing-v1");
    let base = if local_only {
        Decimal::zero()
    } else if mode == "matched_only" {
        Decimal::parse(text(&result, "matched_amount"))?
    } else {
        Decimal::parse(text(&result, "total"))?
    };
    let mut charge = base;
    charge.add(&total);
    let fen = charge.fen_half_up()?;
    if fen > 999_999_999_999 {
        return Err(Error::invalid("补计后金额超出上限"));
    }
    result["upstream_total_fen"] = if local_only {
        Value::Null
    } else {
        result["cost_fen"].clone()
    };
    result["cost_fen"] = json!(fen);
    result["billable_cost_fen"] = json!(fen);
    result
        .as_object_mut()
        .ok_or_else(Error::unavailable)?
        .remove("ambiguous_usage_ids");
    result["digest"] = json!(digest(
        &json!({"rules":"manual-billing-v1","matching":result["digest"],"basis":result["manual_basis"],"local":result["local_unmatched"],"usage":usage,"start":start,"end":end,"mode":mode})
    ));
    if local_only {
        result["total"] = Value::Null;
    }
    Ok(result)
}

pub fn allocations(result: &Value, creditor: i64) -> Result<Value> {
    if result["can_confirm"] != true {
        return Ok(json!([]));
    }
    let mut portions = BTreeMap::<i64, BigRational>::new();
    let local_only = result["manual_basis"]["source"] == "local_only";
    if !local_only {
        let matched = Decimal::parse(text(result, "matched_amount"))?.rational();
        let factor = if result["allocation_mode"] == "proportional" && !matched.is_zero() {
            Decimal::parse(text(result, "total"))?.rational() / matched
        } else {
            BigRational::from_integer(BigInt::from(1))
        };
        if let Some(weights) = result["weights"].as_object() {
            for (id, v) in weights {
                portions.insert(
                    id.parse::<i64>().map_err(|_| Error::invalid("用户无效"))?,
                    Decimal::parse(v.as_str().unwrap_or(""))?.rational() * &factor,
                );
            }
        }
    }
    if let Some(weights) = result["manual_weights"].as_object() {
        for (id, v) in weights {
            *portions
                .entry(id.parse::<i64>().map_err(|_| Error::invalid("用户无效"))?)
                .or_insert_with(BigRational::zero) +=
                Decimal::parse(v.as_str().unwrap_or(""))?.rational();
        }
    }
    portions.retain(|_, v| !v.is_zero());
    let total: BigRational = portions.values().cloned().sum();
    if total.is_zero() {
        return Ok(json!([]));
    }
    let mut amounts = BTreeMap::new();
    let mut ranked = Vec::new();
    for (id, v) in &portions {
        let cents = v * BigInt::from(100);
        let floor = cents.floor().to_integer();
        amounts.insert(
            *id,
            floor
                .to_i64()
                .ok_or_else(|| Error::invalid("金额超出范围"))?,
        );
        ranked.push((*id, cents - floor));
    }
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let target = (&total * BigInt::from(100) + BigRational::new(BigInt::from(1), BigInt::from(2)))
        .floor()
        .to_integer()
        .to_i64()
        .ok_or_else(|| Error::invalid("金额超出范围"))?;
    let remainder = target - amounts.values().sum::<i64>();
    for (id, _) in ranked.iter().take(remainder as usize) {
        *amounts.get_mut(id).expect("same portion keys") += 1;
    }
    Ok(json!(portions.iter().map(|(id,v)|json!({"user_id":id,"amount_fen":amounts[id],"consumption":from_ratio(v,60).to_string(),"share":from_ratio(&(v/&total),28).to_string(),"self_covered":*id==creditor})).collect::<Vec<_>>()))
}
