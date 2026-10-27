//! The little date handling AI Lens needs: ISO 8601 as git prints it

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, now
pub fn now_timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds since the epoch for a date/time given in UTC
pub fn timestamp_utc(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: f64) -> f64 {
    let days = days_from_civil(year, i64::from(month), i64::from(day));
    days as f64 * 86_400.0 + f64::from(hour) * 3600.0 + f64::from(minute) * 60.0 + second
}

fn digits(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Parse an ISO 8601 date as `datetime.fromisoformat` does for what git
/// prints: `YYYY-MM-DD[THH:MM[:SS[.fff]]][Z|±HH:MM]`. A date without a time
/// zone is taken to be UTC. Returns seconds since the epoch.
pub fn parse_iso8601(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.len() < 10 || !value.is_char_boundary(10) {
        return None;
    }
    let (date, rest) = value.split_at(10);
    let mut parts = date.split('-');
    let year = digits(parts.next()?)?;
    let month = digits(parts.next()?)?;
    let day = digits(parts.next()?)?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (mut hour, mut minute, mut second, mut offset) = (0i64, 0i64, 0f64, 0i64);
    if !rest.is_empty() {
        let rest = rest.strip_prefix(['T', ' '])?;
        let (time, zone) = match rest.find(['Z', 'z', '+', '-']) {
            Some(index) => rest.split_at(index),
            None => (rest, ""),
        };
        let mut fields = time.split(':');
        hour = digits(fields.next()?)?;
        minute = digits(fields.next()?)?;
        if let Some(seconds) = fields.next() {
            let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
            second = digits(whole)? as f64;
            if !fraction.is_empty() {
                digits(fraction)?;
                second += format!("0.{fraction}").parse::<f64>().ok()?;
            }
        }
        if fields.next().is_some() || hour > 23 || minute > 59 || second >= 61.0 {
            return None;
        }
        if !zone.is_empty() && zone != "Z" && zone != "z" {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let zone = &zone[1..];
            let (zone_hours, zone_minutes) = match zone.split_once(':') {
                Some((h, m)) => (digits(h)?, digits(m)?),
                None if zone.len() == 4 => (digits(&zone[..2])?, digits(&zone[2..])?),
                None => (digits(zone)?, 0),
            };
            offset = sign * (zone_hours * 3600 + zone_minutes * 60);
        }
    }
    let days = days_from_civil(year, month, day);
    Some(days as f64 * 86_400.0 + (hour * 3600 + minute * 60) as f64 + second - offset as f64)
}
