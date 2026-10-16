use crate::error::{Error, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, SecondsFormat, Utc};
pub fn parse(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(d) = DateTime::parse_from_rfc3339(&s.replace(' ', "T")) {
        return Ok(d.with_timezone(&Utc));
    }
    if chrono::NaiveDateTime::parse_from_str(&s.replace(' ', "T"), "%Y-%m-%dT%H:%M:%S%.f").is_ok()
        || NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
    {
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
            if !(1..=12).contains(&m) {
                return Err(Error::invalid("month must be in 1..12"));
            }
            if NaiveDate::from_ymd_opt(y, m, d).is_none() {
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
    Ok(format(parse(s)?))
}
pub fn format(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::Micros, false)
}
pub fn now() -> String {
    // Fixed time is accepted only by the isolated fixture runtime.
    if std::env::var("SETTLEMENT_ENV").as_deref() == Ok("test")
        && let Ok(v) = std::env::var("SETTLEMENT_TEST_NOW")
        && let Ok(d) = parse(&v)
    {
        return format(d);
    }
    format(Utc::now())
}
pub fn after(seconds: i64) -> String {
    format(parse(&now()).unwrap_or_else(|_| Utc::now()) + chrono::Duration::seconds(seconds))
}
pub fn minute(s: &str) -> Result<(String, String)> {
    let d = parse(s)?;
    let micros = d.timestamp_micros();
    let d = DateTime::from_timestamp_micros(micros.div_euclid(60_000_000) * 60_000_000)
        .ok_or_else(|| Error::invalid("按时间估算需要有效时间"))?;
    Ok((format(d), format(d + chrono::Duration::minutes(1))))
}
pub fn date_bounds(s: &str) -> Result<(String, String)> {
    let offset = FixedOffset::east_opt(8 * 3600).unwrap();
    let date = if let Ok(d) = parse(s) {
        d.with_timezone(&offset).date_naive()
    } else {
        NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d")
            .map_err(|_| Error::invalid("按日期估算需要有效日期"))?
    };
    let d = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| Error::invalid("按日期估算需要有效日期"))?
        .and_local_timezone(offset)
        .single()
        .ok_or_else(|| Error::invalid("按日期估算需要有效日期"))?
        .with_timezone(&Utc);
    Ok((format(d), format(d + chrono::Duration::days(1))))
}

pub fn epoch() -> f64 {
    parse(&now())
        .unwrap_or_else(|_| Utc::now())
        .timestamp_micros() as f64
        / 1_000_000.0
}
