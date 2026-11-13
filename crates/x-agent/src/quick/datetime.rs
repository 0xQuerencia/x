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

/// Convert a named clock time between two explicit fixed UTC offsets.
/// Returns a day-shift label so crossing midnight is not silently hidden.
/// Example: "convert 09:00 +05:30 to -07:00".
fn convert_clock(text: &str) -> Option<(String, String)> {
    let mut fields = text.strip_prefix("convert ")?.split_whitespace();
    let clock = fields.next()?;
    let source = fields.next()?;
    if fields.next()? != "to" {
        return None;
    }
    let destination = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let (h, m) = clock.split_once(':')?;
    if h.len() != 2
        || m.len() != 2
        || !h.bytes().all(|c| c.is_ascii_digit())
        || !m.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let hour = h.parse::<i32>().ok()?;
    let minute = m.parse::<i32>().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    let from = parse_offset(source)?;
    let to = parse_offset(destination)?;
    let total = hour * 60 + minute - from + to;
    let day_delta = total.div_euclid(1_440);
    let local = total.rem_euclid(1_440);
    let relative_day = match day_delta {
        -1 => " (previous day)",
        0 => "",
        1 => " (next day)",
        _ => return None,
    };
    Some((
        format!("Clock conversion: {source} → {destination} (fixed offsets; no DST)"),
        format!("{:02}:{:02}{relative_day}", local / 60, local % 60),
    ))
}

/// An explicit X Quick command, e.g. "date +10", "date today",
/// "time utc", "time +05:30". Values are UTC or a named fixed offset.
/// Calendar and timezone data are not sent to any model or service.
pub fn evaluate(query: &str, when: SystemTime) -> Option<(String, String)> {
    let text = query.trim().to_ascii_lowercase();
    if let Some(result) = convert_clock(&text) {
        return Some(result);
    }
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
    fn converts_clock_times_with_explicit_day_rollover() {
        let t = when(0);
        assert_eq!(
            evaluate("convert 09:00 +05:30 to -07:00", t).unwrap().1,
            "20:30 (previous day)"
        );
        assert_eq!(
            evaluate("convert 23:30 -04:00 to +05:30", t).unwrap().1,
            "09:00 (next day)"
        );
        assert_eq!(
            evaluate("convert 15:00 UTC to +05:30", t).unwrap().1,
            "20:30"
        );
        for input in [
            "convert 25:00 +05:30 to UTC",
            "convert 09:00 PST to UTC",
            "convert 9:00 +05:30 to UTC",
            "convert 09:00 +05:30 until UTC",
        ] {
            assert!(evaluate(input, t).is_none(), "{input}");
        }
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
