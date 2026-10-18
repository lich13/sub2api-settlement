use crate::error::{Error, Result};
use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};
use serde_json::{Value, json};
use std::{collections::BTreeMap, str::FromStr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    pub coefficient: BigInt,
    pub scale: i64,
    negative_zero: bool,
}
fn pow(n: i64) -> BigInt {
    BigInt::from(10).pow(n as u32)
}
impl Decimal {
    pub fn zero() -> Self {
        Self {
            coefficient: BigInt::zero(),
            scale: 0,
            negative_zero: false,
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if ["nan", "snan", "infinity", "inf"].contains(
            &s.trim_start_matches(['-', '+'])
                .to_ascii_lowercase()
                .as_str(),
        ) {
            return Err(Error::invalid("消费记录包含无效或负数金额"));
        }
        let (mantissa, exponent) = if let Some(i) = s.find(['e', 'E']) {
            (
                &s[..i],
                s[i + 1..]
                    .parse::<i64>()
                    .map_err(|_| Error::invalid("消费金额无效"))?,
            )
        } else {
            (s, 0)
        };
        let negative = mantissa.starts_with('-');
        let mantissa = mantissa.trim_start_matches(['-', '+']);
        let mut pieces = mantissa.split('.');
        let whole = pieces.next().unwrap_or("");
        let fraction = pieces.next().unwrap_or("");
        if pieces.next().is_some()
            || (whole.is_empty() && fraction.is_empty())
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return Err(Error::invalid("消费金额无效"));
        }
        let scale = (fraction.len() as i64)
            .checked_sub(exponent)
            .ok_or_else(|| Error::invalid("消费金额无效"))?;
        // Bound exponent work; source NUMERIC and accepted CSV amounts are far below this.
        if scale.abs() > 10000 || whole.len() + fraction.len() > 10000 {
            return Err(Error::invalid("消费金额无效"));
        }
        let coefficient = BigInt::from_str(&format!("{whole}{fraction}"))
            .map_err(|_| Error::invalid("消费金额无效"))?;
        if negative && !coefficient.is_zero() {
            return Err(Error::invalid("消费记录包含无效或负数金额"));
        }
        Ok(Self {
            coefficient,
            scale,
            negative_zero: negative,
        })
    }
    pub fn positive(&self) -> bool {
        self.coefficient > BigInt::zero()
    }
    pub fn rational(&self) -> BigRational {
        if self.scale >= 0 {
            BigRational::new(self.coefficient.clone(), pow(self.scale))
        } else {
            BigRational::from_integer(&self.coefficient * pow(-self.scale))
        }
    }
    pub fn add(&mut self, other: &Self) {
        match self.scale.cmp(&other.scale) {
            std::cmp::Ordering::Equal => self.coefficient += &other.coefficient,
            std::cmp::Ordering::Less => {
                self.coefficient *= pow(other.scale - self.scale);
                self.coefficient += &other.coefficient;
                self.scale = other.scale;
            }
            std::cmp::Ordering::Greater => {
                self.coefficient += &other.coefficient * pow(self.scale - other.scale);
            }
        }
        self.negative_zero = false;
    }
    pub fn subtract(&self, other: &Self) -> Self {
        let scale = self.scale.max(other.scale);
        Self {
            coefficient: &self.coefficient * pow(scale - self.scale)
                - &other.coefficient * pow(scale - other.scale),
            scale,
            negative_zero: false,
        }
    }
    pub fn multiply(&self, other: &Self) -> Self {
        Self {
            coefficient: &self.coefficient * &other.coefficient,
            scale: self.scale + other.scale,
            negative_zero: false,
        }
    }
    pub fn rounded(&self, precision: u32) -> Self {
        // 2^(3p) < 10^p: ordinary amounts need no decimal string allocation to
        // prove that they already fit the requested significant digits.
        if precision != 0 && self.coefficient.bits() <= u64::from(precision) * 3 {
            return self.clone();
        }
        let count = self.coefficient.abs().to_string().len() as i64;
        if count <= precision as i64 {
            return self.clone();
        }
        let n = count - precision as i64;
        let d = pow(n);
        let (q, r) = self.coefficient.div_rem(&d);
        let twice = r.abs() * 2;
        let up = twice > d || (twice == d && q.is_odd());
        Self {
            coefficient: q + if up { BigInt::one() } else { BigInt::zero() },
            scale: self.scale - n,
            negative_zero: false,
        }
    }
    pub fn fen_half_up(&self) -> Result<i64> {
        let r = self.rational() * BigInt::from(100);
        let x = (r + BigRational::new(BigInt::one(), BigInt::from(2)))
            .floor()
            .to_integer();
        x.to_i64().ok_or_else(|| Error::invalid("金额超出范围"))
    }
}
impl std::fmt::Display for Decimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let digits = self.coefficient.abs().to_string();
        let adjusted = digits.len() as i64 - self.scale - 1;
        let sign = if self.coefficient.is_negative() || self.negative_zero {
            "-"
        } else {
            ""
        };
        if self.scale >= 0 && adjusted >= -6 {
            if self.scale == 0 {
                return write!(f, "{sign}{digits}");
            }
            let point = digits.len() as i64 - self.scale;
            if point > 0 {
                write!(
                    f,
                    "{sign}{}.{}",
                    &digits[..point as usize],
                    &digits[point as usize..]
                )
            } else {
                write!(f, "{sign}0.{}{digits}", "0".repeat((-point) as usize))
            }
        } else {
            let body = if digits.len() > 1 {
                format!("{}.{}", &digits[..1], &digits[1..])
            } else {
                digits
            };
            write!(f, "{sign}{body}E{adjusted:+}")
        }
    }
}
pub fn cents(s: &str) -> Result<i64> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:0|[1-9][0-9]{0,9})(?:\.[0-9]{1,2})?$").unwrap()
    });
    if !RE.is_match(s) {
        return Err(Error::invalid("金额须为非负数，最多两位小数"));
    }
    Decimal::parse(s)?.fen_half_up()
}
pub fn from_ratio(r: &BigRational, precision: u32) -> Decimal {
    if r.is_zero() {
        return Decimal::zero();
    }
    let n = r.numer().abs();
    let d = r.denom();
    let mut e = n.to_string().len() as i64 - d.to_string().len() as i64;
    if (e >= 0 && n < d * pow(e)) || (e < 0 && &n * pow(-e) < *d) {
        e -= 1;
    }
    let mut scale = precision as i64 - 1 - e;
    let (nn, dd) = if scale >= 0 {
        (&n * pow(scale), d.clone())
    } else {
        (n, d * pow(-scale))
    };
    let (mut q, rem) = nn.div_rem(&dd);
    if &rem * 2 > dd || (&rem * 2 == dd && q.is_odd()) {
        q += 1;
    }
    if q.to_string().len() > precision as usize {
        q /= 10;
        scale -= 1;
    }
    while rem.is_zero() && scale > 0 && !q.is_zero() && (&q % 10u8).is_zero() {
        q /= 10;
        scale -= 1;
    }
    Decimal {
        coefficient: q,
        scale,
        negative_zero: false,
    }
}
pub fn allocate(
    cost: i64,
    weights: &BTreeMap<i64, Decimal>,
    creditor: i64,
    precision: u32,
) -> Result<Value> {
    let exact: BTreeMap<_, _> = weights
        .iter()
        .filter(|(_, v)| v.positive())
        .map(|(u, v)| (*u, v.rational()))
        .collect();
    let total: BigRational = exact.values().cloned().sum();
    if total.is_zero() {
        return Ok(
            json!([{"user_id":creditor,"consumption":"0","amount_fen":cost,"share":"0","self_covered":true}]),
        );
    }
    let portions: BTreeMap<_, _> = exact
        .iter()
        .map(|(u, v)| (*u, v * BigInt::from(cost) / &total))
        .collect();
    let mut amounts: BTreeMap<_, _> = portions
        .iter()
        .map(|(u, v)| {
            Ok((
                *u,
                v.floor()
                    .to_integer()
                    .to_i64()
                    .ok_or_else(|| Error::invalid("金额超出范围"))?,
            ))
        })
        .collect::<Result<_>>()?;
    let mut ranked: Vec<_> = portions
        .iter()
        .map(|(u, v)| (*u, v - BigInt::from(amounts[u])))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let remainder = cost - amounts.values().sum::<i64>();
    for (u, _) in ranked.iter().take(remainder as usize) {
        *amounts
            .get_mut(u)
            .expect("ranked users come from the same allocation map") += 1;
    }
    let total_decimal = from_ratio(&total, precision);
    Ok(Value::Array(exact.iter().map(|(uid,v)|{
        let first=from_ratio(v,precision);let share=from_ratio(&(first.rational()/total_decimal.rational()),precision);
        json!({"user_id":uid,"consumption":weights[uid].to_string(),"amount_fen":amounts[uid],"share":share.to_string(),"self_covered":*uid==creditor})
    }).collect()))
}
pub fn allocate_matched(weights: &BTreeMap<i64, Decimal>, creditor: i64) -> Result<Value> {
    let exact: BTreeMap<_, _> = weights
        .iter()
        .filter(|(_, v)| v.positive())
        .map(|(u, v)| (*u, v.rational()))
        .collect();
    let total: BigRational = exact.values().cloned().sum();
    if total.is_zero() {
        return Ok(json!([]));
    }
    let mut amounts = BTreeMap::new();
    let mut ranks = Vec::new();
    for (uid, v) in &exact {
        let p = v * BigInt::from(100);
        let floor = p.floor().to_integer();
        let amount = floor
            .to_i64()
            .ok_or_else(|| Error::invalid("金额超出范围"))?;
        ranks.push((*uid, &p - floor));
        amounts.insert(*uid, amount);
    }
    let target = (&total * BigInt::from(100) + BigRational::new(BigInt::one(), BigInt::from(2)))
        .floor()
        .to_integer()
        .to_i64()
        .ok_or_else(|| Error::invalid("金额超出范围"))?;
    ranks.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let remainder = target - amounts.values().sum::<i64>();
    for (uid, _) in ranks.iter().take(remainder as usize) {
        *amounts
            .get_mut(uid)
            .expect("matched ranks and amounts are inserted together") += 1;
    }
    Ok(Value::Array(exact.iter().map(|(uid,v)|json!({"user_id":uid,"consumption":weights[uid].to_string(),"amount_fen":amounts[uid],"share":from_ratio(&(v/&total),28).to_string(),"self_covered":*uid==creditor})).collect()))
}
