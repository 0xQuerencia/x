//! Dependency-free UTC date/time utilities for X Quick.
//! Explicit fixed offsets are supported; no implicit daylight-saving guesses.
//! All date calculations are local and deterministic.

use std::time::{SystemTime, UNIX_EPOCH};

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Gregorian civil date calculation, valid for the representable Unix range.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

fn parse_offset(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("utc") || text == "z" {
        return Some(0);
    }
    let part = text.to_ascii_lowercase();
    let part = part.strip_prefix("utc").unwrap_or(&part);
    if !part.starts_with('+') && !part.starts_with('-') {
        return None;
    }
    let (sign, rest) = if let Some(rest) = part.strip_prefix('+') {
        (1, rest)
    } else {
        (-1, part.strip_prefix('-')?)
    };
    let (hh, mm) = rest.split_once(':')?;
    if hh.len() > 2 || mm.len() != 2 {
        return None;
    }
    let hours = hh.parse::<i32>().ok()?;
    let minutes = mm.parse::<i32>().ok()?;
    if hours > 14 || minutes > 59 || (hours == 14 && minutes > 0) {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

fn format_date(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn format_clock(seconds: i64) -> String {
    let remaining = seconds.rem_euclid(86_400);
    format!("{:02}:{:02}", remaining / 3_600, remaining % 3_600 / 60)
}

/// An explicit X Quick command, e.g. "date +10", "date today",
/// "time utc", "time +05:30". Values are UTC or a named fixed offset.
/// Calendar and timezone data are not sent to any model or service.
pub fn evaluate(query: &str, when: SystemTime) -> Option<(String, String)> {
    let text = query.trim().to_ascii_lowercase();
    let secs = i64::try_from(when.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
    if matches!(text.as_str(), "date" | "date today") {
        return Some(("UTC date".into(), format_date(secs.div_euclid(86_400))));
    }
    if text == "date tomorrow" {
        return Some((
            "Tomorrow (UTC)".into(),
            format_date(secs.div_euclid(86_400).checked_add(1)?),
        ));
    }
    if text == "date yesterday" {
        return Some((
            "Yesterday (UTC)".into(),
            format_date(secs.div_euclid(86_400).checked_sub(1)?),
        ));
    }
    if let Some(shift) = text
        .strip_prefix("date +")
        .or_else(|| text.strip_prefix("date -"))
    {
        let days: i64 = shift.parse().ok()?;
        if days > 36_500 {
            return None;
        }
        let sign = if text.starts_with("date -") { -1 } else { 1 };
        let result = secs.div_euclid(86_400).checked_add(sign * days)?;
        return Some((
            format!("UTC date shifted {sign:+} {days} day(s)"),
            format_date(result),
        ));
    }
    if let Some(offset_text) = text.strip_prefix("time ") {
        let offset = parse_offset(offset_text)?;
        let seconds = secs.checked_add(i64::from(offset) * 60)?;
        return Some((
            format!("Time at fixed UTC offset {offset:+} minutes (no DST adjustment)"),
            format!(
                "{} {}",
                format_date(seconds.div_euclid(86_400)),
                format_clock(seconds)
            ),
        ));
    }
    if text == "unix timestamp" {
        return Some(("Unix seconds".into(), secs.to_string()));
    }
    None
}

pub fn now(query: &str) -> Option<(String, String)> {
    evaluate(query, SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn when(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn leap_day_and_date_offsets_are_correct() {
        // 2000-02-28 00:00 UTC
        let t = when(951_696_000);
        assert_eq!(evaluate("date +1", t).unwrap().1, "2000-02-29");
        assert_eq!(evaluate("date +2", t).unwrap().1, "2000-03-01");
        assert_eq!(evaluate("date yesterday", t).unwrap().1, "2000-02-27");
    }

    #[test]
    fn fixed_offsets_show_day_rollover_without_guessing_dst() {
        // 1970-01-01 00:00:00 UTC
        assert_eq!(evaluate("time utc", when(0)).unwrap().1, "1970-01-01 00:00");
        assert_eq!(
            evaluate("time +05:30", when(0)).unwrap().1,
            "1970-01-01 05:30"
        );
        assert_eq!(
            evaluate("time -01:00", when(0)).unwrap().1,
            "1969-12-31 23:00"
        );
    }

    #[test]
    fn malformed_or_unbounded_queries_do_not_produce_answers() {
        for query in [
            "date +9999999",
            "date +abc",
            "time PST",
            "time +25:00",
            "time +05:60",
            "tomorrow",
        ] {
            assert!(evaluate(query, when(0)).is_none(), "{query}");
        }
    }
}
