use crate::error::{Error, Result};
use time::format_description::well_known::Rfc3339;
pub use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

pub const LOCAL_OFFSET: UtcOffset = time::macros::offset!(+08:00);

pub fn parse_date(s: &str) -> Option<Date> {
    let mut parts = s.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || y.len() != 4
        || !(1..=2).contains(&m.len())
        || !(1..=2).contains(&d.len())
        || !y
            .bytes()
            .chain(m.bytes())
            .chain(d.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let month = Month::try_from(m.parse::<u8>().ok()?).ok()?;
    Date::from_calendar_date(y.parse::<i32>().ok()?, month, d.parse::<u8>().ok()?).ok()
}

pub fn format_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day())
}

fn parse_naive_datetime(s: &str) -> Option<PrimitiveDateTime> {
    let (date_part, time_part) = s.split_once('T')?;
    let date = parse_date(date_part)?;
    let (hms, nanos) = if let Some((hms, frac)) = time_part.split_once('.') {
        if frac.is_empty() || frac.len() > 9 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut padded = [b'0'; 9];
        padded[..frac.len()].copy_from_slice(frac.as_bytes());
        let nanos = std::str::from_utf8(&padded).ok()?.parse::<u32>().ok()?;
        (hms, nanos)
    } else {
        (time_part, 0)
    };
    let mut parts = hms.split(':');
    let (hh, mm, ss) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || !(1..=2).contains(&hh.len())
        || !(1..=2).contains(&mm.len())
        || !(1..=2).contains(&ss.len())
        || !hh
            .bytes()
            .chain(mm.bytes())
            .chain(ss.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let time = Time::from_hms_nano(
        hh.parse::<u8>().ok()?,
        mm.parse::<u8>().ok()?,
        ss.parse::<u8>().ok()?,
        nanos,
    )
    .ok()?;
    Some(PrimitiveDateTime::new(date, time))
}

pub fn normalize_datetime(s: &str) -> Option<String> {
    let normalized: std::borrow::Cow<'_, str> = if s.contains(' ') {
        std::borrow::Cow::Owned(s.replace(' ', "T"))
    } else {
        std::borrow::Cow::Borrowed(s)
    };
    if let Ok(d) = OffsetDateTime::parse(&normalized, &Rfc3339) {
        let micros = d.microsecond();
        let frac = if micros == 0 {
            String::new()
        } else {
            format!(".{micros:06}")
        };
        let suffix = if d.offset().is_utc() {
            "Z".to_owned()
        } else {
            let total = d.offset().whole_seconds();
            let sign = if total < 0 { '-' } else { '+' };
            let abs = total.unsigned_abs();
            let (h, m, sec) = (abs / 3600, (abs % 3600) / 60, abs % 60);
            if sec == 0 {
                format!("{sign}{h:02}:{m:02}")
            } else {
                format!("{sign}{h:02}:{m:02}:{sec:02}")
            }
        };
        return Some(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{frac}{suffix}",
            d.year(),
            d.month() as u8,
            d.day(),
            d.hour(),
            d.minute(),
            d.second(),
        ));
    }
    let d = parse_naive_datetime(s)?;
    let micros = d.microsecond();
    let frac = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{frac}",
        d.year(),
        d.month() as u8,
        d.day(),
        d.hour(),
        d.minute(),
        d.second(),
    ))
}

fn parse_canonical_utc(s: &str) -> Option<OffsetDateTime> {
    let b = s.as_bytes();
    if b.len() != 32
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || &b[26..32] != b"+00:00"
    {
        return None;
    }
    let digit = |i: usize| -> Option<u32> {
        let d = b[i].wrapping_sub(b'0');
        (d < 10).then_some(u32::from(d))
    };
    let d2 = |i: usize| Some((digit(i)? * 10 + digit(i + 1)?) as u8);
    let year = (digit(0)? * 1000 + digit(1)? * 100 + digit(2)? * 10 + digit(3)?) as i32;
    let month = Month::try_from(d2(5)?).ok()?;
    let date = Date::from_calendar_date(year, month, d2(8)?).ok()?;
    let micros = digit(20)? * 100_000
        + digit(21)? * 10_000
        + digit(22)? * 1_000
        + digit(23)? * 100
        + digit(24)? * 10
        + digit(25)?;
    let time = Time::from_hms_micro(d2(11)?, d2(14)?, d2(17)?, micros).ok()?;
    Some(date.with_time(time).assume_utc())
}

pub fn parse(s: &str) -> Result<OffsetDateTime> {
    if let Some(d) = parse_canonical_utc(s) {
        return Ok(d);
    }
    let normalized: std::borrow::Cow<'_, str> = if s.contains(' ') {
        std::borrow::Cow::Owned(s.replace(' ', "T"))
    } else {
        std::borrow::Cow::Borrowed(s)
    };
    if let Ok(d) = OffsetDateTime::parse(&normalized, &Rfc3339) {
        return Ok(d.to_offset(UtcOffset::UTC));
    }
    if parse_naive_datetime(&normalized).is_some() || parse_date(s).is_some() {
        return Err(Error::invalid("时间必须包含时区"));
    }
    if let Some(date) = s.get(..10) {
        let parts = date
            .split('-')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>();
        if let Ok(parts) = parts
            && parts.len() == 3
        {
            let (y, m, d) = (parts[0] as i32, parts[1], parts[2]);
            let Some(month) = u8::try_from(m).ok().and_then(|m| Month::try_from(m).ok()) else {
                return Err(Error::invalid("month must be in 1..12"));
            };
            if u8::try_from(d)
                .ok()
                .and_then(|day| Date::from_calendar_date(y, month, day).ok())
                .is_none()
            {
                return Err(Error::invalid("day is out of range for month"));
            }
        }
    }
    let repr = if !s.contains('\'') {
        format!(
            "'{}'",
            s.replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        )
    } else {
        format!("{s:?}")
    };
    Err(Error::invalid(format!("Invalid isoformat string: {repr}")))
}

pub fn instant(s: &str) -> Result<String> {
    if parse_canonical_utc(s).is_some() {
        return Ok(s.to_owned());
    }
    Ok(format(parse(s)?))
}

pub fn format(d: OffsetDateTime) -> String {
    let u = d.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}+00:00",
        u.year(),
        u.month() as u8,
        u.day(),
        u.hour(),
        u.minute(),
        u.second(),
        u.microsecond(),
    )
}

fn is_test_env() -> bool {
    static TEST_ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TEST_ENV.get_or_init(|| std::env::var("SETTLEMENT_ENV").as_deref() == Ok("test"))
}

fn now_utc() -> OffsetDateTime {
    // Fixed time is accepted only by the isolated fixture runtime.
    if is_test_env()
        && let Ok(v) = std::env::var("SETTLEMENT_TEST_NOW")
        && let Ok(d) = parse(&v)
    {
        return d;
    }
    OffsetDateTime::now_utc()
}

pub fn now() -> String {
    format(now_utc())
}

pub fn after(seconds: i64) -> String {
    format(now_utc() + Duration::seconds(seconds))
}

pub fn timestamp_micros(d: OffsetDateTime) -> i64 {
    d.unix_timestamp_nanos().div_euclid(1_000) as i64
}

pub fn minute(s: &str) -> Result<(String, String)> {
    let d = parse(s)?;
    let micros = timestamp_micros(d);
    let floored = micros.div_euclid(60_000_000) * 60_000_000;
    let d = OffsetDateTime::from_unix_timestamp_nanos(i128::from(floored) * 1_000)
        .map_err(|_| Error::invalid("按时间估算需要有效时间"))?;
    Ok((format(d), format(d + Duration::minutes(1))))
}

pub fn date_bounds(s: &str) -> Result<(String, String)> {
    let date = if let Ok(d) = parse(s) {
        d.to_offset(LOCAL_OFFSET).date()
    } else {
        parse_date(s.get(..10).unwrap_or(s))
            .ok_or_else(|| Error::invalid("按日期估算需要有效日期"))?
    };
    let d = date
        .with_time(Time::MIDNIGHT)
        .assume_offset(LOCAL_OFFSET)
        .to_offset(UtcOffset::UTC);
    Ok((format(d), format(d + Duration::days(1))))
}

pub fn epoch() -> f64 {
    timestamp_micros(now_utc()) as f64 / 1_000_000.0
}

pub fn is_expired(expires_at: &serde_json::Value) -> bool {
    is_expired_at(expires_at, epoch())
}

fn is_expired_at(expires_at: &serde_json::Value, now: f64) -> bool {
    expires_at
        .as_f64()
        .filter(|expiry| expiry.is_finite())
        .is_none_or(|expiry| expiry <= now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn date_boundaries_share_the_beijing_offset_with_reminders() {
        assert_eq!(LOCAL_OFFSET.whole_seconds(), 28_800);
        assert_eq!(
            date_bounds("2026-01-01T16:00:00+00:00").unwrap(),
            (
                "2026-01-01T16:00:00.000000+00:00".into(),
                "2026-01-02T16:00:00.000000+00:00".into()
            )
        );
    }

    #[test]
    fn session_expiry_is_exclusive_and_missing_values_fail_closed() {
        let now = 1_800_000_000.125;
        assert!(!is_expired_at(&json!(now + 0.000001), now));
        assert!(is_expired_at(&json!(now), now));
        assert!(is_expired_at(&json!(now - 0.000001), now));
        for value in [
            json!(null),
            json!("future"),
            json!({}),
            json!([]),
            json!(-1),
        ] {
            assert!(is_expired_at(&value, now));
        }
    }

    #[test]
    fn canonical_utc_fast_path_matches_rfc3339_and_rejects_invalid_dates() {
        let leap = "2024-02-29T23:59:59.987654+00:00";
        assert_eq!(instant(leap).unwrap(), leap);
        assert_eq!(format(parse(leap).unwrap()), leap);
        assert_eq!(instant("2024-03-01 07:59:59.987654+08:00").unwrap(), leap);
        assert_eq!(
            parse("2025-02-29T00:00:00.000000+00:00")
                .unwrap_err()
                .message,
            "day is out of range for month"
        );
        assert_eq!(
            parse("2025-13-01T00:00:00.000000+00:00")
                .unwrap_err()
                .message,
            "month must be in 1..12"
        );
    }
}
