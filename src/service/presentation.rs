use super::*;
use crate::money::Decimal;
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Zero};

impl Service {
    /// Decorate response copies only: display metadata never enters the ledger or its digests.
    pub(super) async fn present_calculation(&self, mut value: Value, settled: bool) -> Value {
        if value.get("cycle").is_none() {
            return value;
        }
        if !settled {
            self.present_cycle(&mut value["cycle"]).await;
        }
        let cost = value["bill"]["billable_cost_fen"]
            .as_i64()
            .unwrap_or_else(|| int(&value["cycle"], "cost_fen"));
        value["display"] = json!({
            "cost_multiplier": multiplier(cost, value["total_consumption"].as_str()),
            "settled": settled,
        });
        value
    }

    pub(super) async fn present_cycle(&self, cycle: &mut Value) {
        if cycle["status"] == "open"
            && let Some(account) = self
                .inventory
                .state
                .read()
                .await
                .rows
                .get(&int(cycle, "account_id"))
        {
            cycle["name"] = account["name"].clone();
        }
    }
}

fn multiplier(cost_fen: i64, consumption: Option<&str>) -> Option<String> {
    let total = Decimal::parse(consumption?).ok()?;
    if cost_fen < 0 || !total.positive() {
        return None;
    }
    let scaled = BigRational::from_integer(BigInt::from(cost_fen) * 100) / total.rational();
    if scaled > BigRational::zero() && scaled < BigRational::one() {
        return Some("<0.0001".into());
    }
    let rounded = (scaled + BigRational::new(BigInt::one(), BigInt::from(2)))
        .floor()
        .to_integer();
    let digits = rounded.to_string();
    let digits = format!("{digits:0>5}");
    let split = digits.len() - 4;
    Some(format!("{}.{}", &digits[..split], &digits[split..]))
}
