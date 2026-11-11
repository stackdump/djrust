//! `JournalTime` (`Journal/JournalEvent.swift`): wall-clock stamps in the journal, ISO 8601
//! UTC with milliseconds. Metadata only: nothing the engine decides ever reads them back.
//! A `Date` is Unix milliseconds here (`i64`); the engine has no clock.

use alloc::format;
use alloc::string::String;

/// `Date.distantPast`, as the sentinel a missing or unreadable stamp decodes to.
pub const DISTANT_PAST: i64 = -63_114_076_800_000;

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `JournalTime.string`: "2025-10-09T08:53:20.250Z".
pub fn string(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        t / 3600,
        (t % 3600) / 60,
        t % 60
    )
}

/// The stores' second-precision stamp (`ISO8601DateFormatter` with no fractional seconds):
/// "2023-11-14T22:13:20Z". Fractions are dropped, not rounded.
pub fn string_seconds(ms: i64) -> String {
    let s = string(ms.div_euclid(1000) * 1000);
    format!("{}Z", &s[..s.len() - 5])
}

fn digits(s: &str, n: usize) -> Option<i64> {
    if s.len() != n || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn parse_stamp(s: &str, fractional: bool) -> Option<i64> {
    // 2025-10-09T08:53:20[.250]Z
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let y = digits(&s[0..4], 4)?;
    let mo = digits(&s[5..7], 2)? as u32;
    let d = digits(&s[8..10], 2)? as u32;
    let h = digits(&s[11..13], 2)?;
    let mi = digits(&s[14..16], 2)?;
    let sec = digits(&s[17..19], 2)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut milli = 0i64;
    if fractional {
        let frac = rest.strip_prefix('.')?;
        let end = frac.find(|c: char| !c.is_ascii_digit())?;
        if end == 0 {
            return None;
        }
        let f = &frac[..end];
        // milliseconds: pad or cut to three digits
        let mut three = String::from(&f[..f.len().min(3)]);
        while three.len() < 3 {
            three.push('0');
        }
        milli = three.parse().ok()?;
        rest = &frac[end..];
    }
    // the zone: Z, or ±hh:mm
    let offset_s = if rest == "Z" {
        0
    } else if rest.len() == 6
        && (rest.starts_with('+') || rest.starts_with('-'))
        && &rest[3..4] == ":"
    {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        sign * (digits(&rest[1..3], 2)? * 3600 + digits(&rest[4..6], 2)? * 60)
    } else {
        return None;
    };
    let days = days_from_civil(y, mo, d);
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec - offset_s) * 1000) + milli)
}

/// `JournalTime.date`: a stamp with fractional seconds back; None for anything else.
pub fn date(s: &str) -> Option<i64> {
    parse_stamp(s, true)
}

/// A second-precision stamp back (`FavoritesStore.iso.date(from:)`).
pub fn date_seconds(s: &str) -> Option<i64> {
    parse_stamp(s, false)
}

/// `JournalTime.rounded` for a time in seconds with fractions: to the millisecond.
pub fn rounded_ms(unix_seconds: f64) -> i64 {
    libm::round(unix_seconds * 1000.0) as i64
}

/// A file-name stamp that sorts by time (`Journal.fileStamp`): "20261017T143205.123Z".
pub fn file_stamp(ms: i64) -> String {
    let s = string(ms);
    // 2026-10-17T14:32:05.123Z → 20261017T143205.123Z
    format!(
        "{}{}{}T{}{}{}.{}Z",
        &s[0..4],
        &s[5..7],
        &s[8..10],
        &s[11..13],
        &s[14..16],
        &s[17..19],
        &s[20..23]
    )
}
