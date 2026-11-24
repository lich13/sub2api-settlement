use crate::error::{Error, Result};
use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};
use serde_json::{Value, json};
use std::{borrow::Cow, collections::BTreeMap, str::FromStr, sync::LazyLock};

static POW10: LazyLock<[BigInt; 65]> =
    LazyLock::new(|| std::array::from_fn(|i| BigInt::from(10u8).pow(i as u32)));

fn pow_cow(n: i64) -> Cow<'static, BigInt> {
    if let Ok(idx) = usize::try_from(n)
        && let Some(v) = POW10.get(idx)
    {
        Cow::Borrowed(v)
    } else {
        Cow::Owned(BigInt::from(10u8).pow(n as u32))
    }
}

fn decimal_len_abs(x: &BigInt) -> i64 {
    debug_assert!(!x.is_negative());
    if x.is_zero() {
        return 1;
    }
    let mut d = (((x.bits() - 1) as f64) * std::f64::consts::LOG10_2) as i64 + 1;
    if d > 1 && *x < *pow_cow(d - 1) {
        d -= 1;
    } else if *x >= *pow_cow(d) {
        d += 1;
    }
    d
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    pub coefficient: BigInt,
    pub scale: i64,
    negative_zero: bool,
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
        if s.len() <= 16
            && ["nan", "snan", "infinity", "inf"].contains(
                &s.trim_start_matches(['-', '+'])
                    .to_ascii_lowercase()
                    .as_str(),
            )
        {
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
        let mut acc: u128 = 0;
        let mut sig_digits: usize = 0;
        let mut overflow = false;
        for b in whole.bytes().chain(fraction.bytes()) {
            if sig_digits == 0 && b == b'0' {
                continue;
            }
            sig_digits += 1;
            if sig_digits <= 38 {
                acc = acc * 10 + u128::from(b - b'0');
            } else {
                overflow = true;
                break;
            }
        }
        let coefficient = if sig_digits == 0 {
            BigInt::zero()
        } else if !overflow {
            BigInt::from(acc)
        } else {
            BigInt::from_str(&format!("{whole}{fraction}"))
                .map_err(|_| Error::invalid("消费金额无效"))?
        };
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
        self.coefficient.is_positive()
    }
    pub fn rational(&self) -> BigRational {
        if self.scale >= 0 {
            BigRational::new(self.coefficient.clone(), pow_cow(self.scale).into_owned())
        } else {
            BigRational::from_integer(&self.coefficient * pow_cow(-self.scale).as_ref())
        }
    }
    pub fn add(&mut self, other: &Self) {
        match self.scale.cmp(&other.scale) {
            std::cmp::Ordering::Equal => self.coefficient += &other.coefficient,
            std::cmp::Ordering::Less => {
                self.coefficient *= pow_cow(other.scale - self.scale).as_ref();
                self.coefficient += &other.coefficient;
                self.scale = other.scale;
            }
            std::cmp::Ordering::Greater => {
                self.coefficient += &other.coefficient * pow_cow(self.scale - other.scale).as_ref();
            }
        }
        self.negative_zero = false;
    }
    pub fn subtract(&self, other: &Self) -> Self {
        let scale = self.scale.max(other.scale);
        Self {
            coefficient: &self.coefficient * pow_cow(scale - self.scale).as_ref()
                - &other.coefficient * pow_cow(scale - other.scale).as_ref(),
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
        // 2^(3p) < 10^p: ordinary amounts need no decimal length check to
        // prove that they already fit the requested significant digits.
        if precision != 0 && self.coefficient.bits() <= u64::from(precision) * 3 {
            return self.clone();
        }
        let abs = self.coefficient.abs();
        let count = decimal_len_abs(&abs);
        if count <= precision as i64 {
            return self.clone();
        }
        let n = count - precision as i64;
        let d = pow_cow(n);
        let (q, r) = self.coefficient.div_rem(d.as_ref());
        let twice = r.abs() * 2;
        let up = &twice > d.as_ref() || (&twice == d.as_ref() && q.is_odd());
        Self {
            coefficient: q + if up { BigInt::one() } else { BigInt::zero() },
            scale: self.scale - n,
            negative_zero: false,
        }
    }
    pub fn fen_half_up(&self) -> Result<i64> {
        round_cents(&self.rational())
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
pub fn valid_decimal_ascii(
    s: &str,
    max_int: usize,
    max_frac: usize,
    allow_leading_zero: bool,
) -> bool {
    if s.is_empty() {
        return false;
    }
    let int_part = match s.split_once('.') {
        Some((i, f)) => {
            if max_frac == 0
                || f.is_empty()
                || f.len() > max_frac
                || !f.bytes().all(|b| b.is_ascii_digit())
            {
                return false;
            }
            i
        }
        None => s,
    };
    if int_part.is_empty()
        || int_part.len() > max_int
        || !int_part.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    if !allow_leading_zero && int_part.len() > 1 && int_part.starts_with('0') {
        return false;
    }
    true
}
pub fn valid_amount(s: &str) -> bool {
    valid_decimal_ascii(s, 10, 2, false)
}
pub fn cents(s: &str) -> Result<i64> {
    if !valid_amount(s) {
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
    let mut e = decimal_len_abs(&n) - decimal_len_abs(d);
    if (e >= 0 && n < d * pow_cow(e).as_ref()) || (e < 0 && &n * pow_cow(-e).as_ref() < *d) {
        e -= 1;
    }
    let mut scale = precision as i64 - 1 - e;
    let (nn, dd) = if scale >= 0 {
        (&n * pow_cow(scale).as_ref(), d.clone())
    } else {
        (n, d * pow_cow(-scale).as_ref())
    };
    let (mut q, rem) = nn.div_rem(&dd);
    if &rem * 2 > dd || (&rem * 2 == dd && q.is_odd()) {
        q += 1;
    }
    if decimal_len_abs(&q) > precision as i64 {
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
pub fn round_cents(total: &BigRational) -> Result<i64> {
    (total * BigInt::from(100) + BigRational::new(BigInt::one(), BigInt::from(2)))
        .floor()
        .to_integer()
        .to_i64()
        .ok_or_else(|| Error::invalid("金额超出范围"))
}
pub fn largest_remainder(
    portions: &BTreeMap<i64, BigRational>,
    target: i64,
) -> Result<BTreeMap<i64, i64>> {
    let mut amounts = BTreeMap::new();
    let mut ranked = Vec::new();
    for (uid, v) in portions {
        let floor = v.floor().to_integer();
        let amount = floor
            .to_i64()
            .ok_or_else(|| Error::invalid("金额超出范围"))?;
        ranked.push((*uid, v - floor));
        amounts.insert(*uid, amount);
    }
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let remainder = target - amounts.values().sum::<i64>();
    for (uid, _) in ranked.iter().take(remainder as usize) {
        *amounts
            .get_mut(uid)
            .expect("ranked users come from the same allocation map") += 1;
    }
    Ok(amounts)
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
    let amounts = largest_remainder(&portions, cost)?;
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
    let portions: BTreeMap<_, _> = exact
        .iter()
        .map(|(uid, v)| (*uid, v * BigInt::from(100)))
        .collect();
    let amounts = largest_remainder(&portions, round_cents(&total)?)?;
    Ok(Value::Array(exact.iter().map(|(uid,v)|json!({"user_id":uid,"consumption":weights[uid].to_string(),"amount_fen":amounts[uid],"share":from_ratio(&(v/&total),28).to_string(),"self_covered":*uid==creditor})).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_decimal_ascii_enforces_digit_length_and_leading_zero_rules() {
        for ok in ["0", "9", "10", "9999999999", "0.01", "12.3"] {
            assert!(
                valid_decimal_ascii(ok, 10, 2, false),
                "expected valid: {ok}"
            );
        }
        for bad in [
            "",
            ".",
            ".5",
            "5.",
            "1.2.3",
            "00",
            "01",
            "01.5",
            "-1",
            "+1",
            " 1",
            "1\n",
            "1.234",
            "10000000000",
        ] {
            assert!(
                !valid_decimal_ascii(bad, 10, 2, false),
                "expected invalid: {bad:?}"
            );
        }
        assert!(valid_decimal_ascii("00", 10, 18, true));
        assert!(valid_decimal_ascii("01.000000000000000001", 10, 18, true));
        assert!(!valid_decimal_ascii("1.0000000000000000001", 10, 18, true));
        assert!(!valid_decimal_ascii("1.0", 10, 0, false));
    }

    #[test]
    fn decimal_len_and_u128_parse_boundary_match_bigint_strings() {
        assert_eq!(decimal_len_abs(&BigInt::zero()), 1);
        for k in 0..=80_u32 {
            let p = BigInt::from(10u8).pow(k);
            assert_eq!(decimal_len_abs(&p), i64::from(k) + 1);
            if k > 0 {
                let below = &p - 1u8;
                assert_eq!(decimal_len_abs(&below), below.to_string().len() as i64);
            }
            let above = &p + 1u8;
            assert_eq!(decimal_len_abs(&above), above.to_string().len() as i64);
        }
        for b in 1..=256_u32 {
            let v = BigInt::one() << b;
            let below = &v - 1u8;
            assert_eq!(decimal_len_abs(&v), v.to_string().len() as i64);
            assert_eq!(decimal_len_abs(&below), below.to_string().len() as i64);
        }
        let d38 = "9".repeat(38);
        let d39 = "9".repeat(39);
        assert_eq!(Decimal::parse(&d38).unwrap().to_string(), d38);
        assert_eq!(Decimal::parse(&d39).unwrap().to_string(), d39);
        assert_eq!(
            Decimal::parse(&format!("{d38}e-18")).unwrap().to_string(),
            format!("{}.{}", &d38[..20], &d38[20..])
        );
        assert_eq!(
            Decimal::parse(&format!("{d39}e-18")).unwrap().to_string(),
            format!("{}.{}", &d39[..21], &d39[21..])
        );
        assert_eq!(Decimal::parse("1e-18").unwrap().to_string(), "1E-18");
        assert_eq!(Decimal::parse("-0.00e5").unwrap().to_string(), "-0E+3");
        assert_eq!(Decimal::parse("0000.0000").unwrap().to_string(), "0.0000");
    }
}
