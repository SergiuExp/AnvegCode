//! Small time helpers: Unix milliseconds, ISO-8601 parsing for transcript
//! timestamps, and "5 min ago" formatting.

use std::time::{SystemTime, UNIX_EPOCH};

/// Now, in Unix milliseconds.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A `SystemTime` in Unix milliseconds.
pub fn system_time_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse an RFC 3339 UTC timestamp such as `2026-11-23T18:54:05.686Z`
/// (the form Claude Code writes) into Unix milliseconds. Offsets other than
/// `Z` are accepted as `+hh:mm` / `-hh:mm`.
pub fn parse_rfc3339_millis(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Split time and zone.
    let (time, offset_minutes) = if let Some(t) = rest.strip_suffix('Z') {
        (t, 0)
    } else if let Some(pos) = rest.rfind(['+', '-']) {
        let (t, zone) = rest.split_at(pos);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (h, m) = zone[1..].split_once(':')?;
        let minutes = h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?;
        (t, sign * minutes)
    } else {
        (rest, 0)
    };
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let sec_str = t.next().unwrap_or("0");
    let (sec, frac) = match sec_str.split_once('.') {
        Some((s, f)) => (s, f),
        None => (sec_str, ""),
    };
    let second: i64 = sec.parse().ok()?;
    let mut millis: i64 = 0;
    for (i, ch) in frac.chars().take(3).enumerate() {
        millis += ch.to_digit(10)? as i64 * 10_i64.pow(2 - i as u32);
    }
    let days = days_from_civil(year, month, day);
    let total = ((days * 24 + hour) * 60 + minute - offset_minutes) * 60 + second;
    Some(total * 1000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "just now", "5 min ago", "3 h ago", "2 d ago", "4 wk ago".
pub fn relative(then_millis: i64, now: i64) -> String {
    let secs = ((now - then_millis) / 1000).max(0);
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=1_209_599 => format!("{} d ago", secs / 86_400),
        _ => format!("{} wk ago", secs / 604_800),
    }
}

/// `2026-11-23 18:54` in UTC, for tooltips.
pub fn format_utc(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_timestamps() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:01.5Z"), Some(1500));
        // 2026-11-23T18:54:05.686Z
        let t = parse_rfc3339_millis("2026-11-23T18:54:05.686Z");
        assert_eq!(t, Some(1_795_460_045_686));
        assert_eq!(
            parse_rfc3339_millis("2026-11-23T19:54:05.686+01:00"),
            Some(1_795_460_045_686)
        );
        assert_eq!(parse_rfc3339_millis("garbage"), None);
        assert_eq!(parse_rfc3339_millis("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn relative_times() {
        let now = 10_000_000_000;
        assert_eq!(relative(now - 5_000, now), "just now");
        assert_eq!(relative(now - 5 * 60_000, now), "5 min ago");
        assert_eq!(relative(now - 3 * 3_600_000, now), "3 h ago");
        assert_eq!(relative(now - 2 * 86_400_000, now), "2 d ago");
        assert_eq!(relative(now - 21 * 86_400_000, now), "3 wk ago");
        assert_eq!(relative(now + 5_000, now), "just now");
    }

    #[test]
    fn formats_utc() {
        assert_eq!(format_utc(1_795_460_045_686), "2026-11-23 18:54 UTC");
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
    }
}
