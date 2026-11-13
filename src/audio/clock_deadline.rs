//! Pure fixed-offset clock-to-duration calculation for Keep Awake.
//! No geolocation, timezone guessing or DST inference.
use std::time::{SystemTime, UNIX_EPOCH};

pub fn minutes_until(clock: &str, offset: &str, now: SystemTime) -> Result<u32, String> {
    let (hh, mm) = clock.split_once(':').ok_or("Use HH:MM (24-hour)")?;
    if hh.len() != 2 || mm.len() != 2 || !hh.bytes().all(|b| b.is_ascii_digit())
        || !mm.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Use HH:MM (24-hour)".into());
    }
    let h: u32 = hh.parse().map_err(|_| "Invalid hour")?;
    let m: u32 = mm.parse().map_err(|_| "Invalid minute")?;
    if h > 23 || m > 59 { return Err("Clock time must be within 00:00–23:59".into()); }
    let raw = offset.strip_prefix("UTC").unwrap_or(offset);
    let raw = if raw.is_empty() || raw.eq_ignore_ascii_case("Z") { "+00:00" } else { raw };
    let (sign, rest) = if let Some(x) = raw.strip_prefix('+') { (1_i64, x) }
        else if let Some(x) = raw.strip_prefix('-') { (-1_i64, x) }
        else { return Err("Use a fixed UTC offset such as +05:30".into()); };
    let (oh, om) = rest.split_once(':').ok_or("Use offset ±HH:MM")?;
    if oh.len() != 2 || om.len() != 2 || !oh.bytes().all(|b| b.is_ascii_digit())
        || !om.bytes().all(|b| b.is_ascii_digit()) { return Err("Use offset ±HH:MM".into()); }
    let oh: i64 = oh.parse().map_err(|_| "Invalid UTC hour")?;
    let om: i64 = om.parse().map_err(|_| "Invalid UTC minute")?;
    if oh > 14 || om > 59 || (oh == 14 && om != 0) {
        return Err("UTC offset is outside ±14:00".into());
    }
    let secs = i64::try_from(now.duration_since(UNIX_EPOCH)
        .map_err(|_| "Clock is before Unix epoch")?.as_secs())
        .map_err(|_| "Clock overflow")?;
    let current = (secs + sign * (oh * 60 + om) * 60).rem_euclid(86_400);
    let target = i64::from(h * 60 + m) * 60;
    let remaining = (target - current).rem_euclid(86_400);
    if remaining == 0 { return Err("Target time is already now".into()); }
    let minutes = u32::try_from(remaining.div_ceil(60)).map_err(|_| "Duration overflow")?;
    if !(1..=480).contains(&minutes) {
        return Err("Target must be within the next 8 hours".into());
    }
    Ok(minutes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn offset_and_rollover_are_bounded() {
        let t = UNIX_EPOCH + Duration::from_secs(0);
        assert_eq!(minutes_until("05:31", "+05:30", t), Ok(1));
        assert_eq!(minutes_until("06:00", "UTC+05:30", t), Ok(30));
        assert_eq!(minutes_until("00:10", "UTC", t), Ok(10));
        assert!(minutes_until("23:59", "+05:30", t).is_err());
        assert!(minutes_until("05:30", "+05:30", t).is_err());
        for (clock, offset) in [
            ("25:00", "+05:30"), ("5:00", "+05:30"), ("10:00", "+25:00"),
            ("10:00", "PST"), ("10:00", "+05:99"), ("10:00", "+14:01"),
        ] { assert!(minutes_until(clock, offset, t).is_err()); }
    }

    #[test]
    fn non_whole_minutes_round_up_by_less_than_one_minute() {
        let t = UNIX_EPOCH + Duration::from_secs(31);
        assert_eq!(minutes_until("00:02", "UTC", t), Ok(2));
    }
}
