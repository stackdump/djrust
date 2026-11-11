//! XML Schema `dateTime` as Verifiable Credentials use it (`validFrom`, `validUntil`, a proof's
//! `created` / `expires`), in the one form this crate writes and accepts:
//! `YYYY-MM-DDTHH:MM:SSZ`, UTC, no fraction, no offset. One form keeps the JCS bytes (and so the
//! CID) of a credential a function of the instant alone.

use crate::Error;

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// The latest instant written: 9999-12-31T23:59:59Z.
pub const MAX: u64 = 253_402_300_799;

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format(unix: u64) -> Result<String, Error> {
    if unix > MAX {
        return Err(Error::invalid("time is after year 9999"));
    }
    let (days, secs) = ((unix / 86_400) as i64, unix % 86_400);
    let (y, m, d) = civil_from_days(days);
    Ok(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60))
}

/// The inverse of [format]; anything not in exactly that form is an error.
pub fn parse(s: &str) -> Result<u64, Error> {
    let bad = || Error::invalid(format!("not a YYYY-MM-DDTHH:MM:SSZ time: {s}"));
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return Err(bad());
    }
    let num = |r: std::ops::Range<usize>| -> Result<i64, Error> {
        let t = &s[r];
        if !t.bytes().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        t.parse().map_err(|_| bad())
    };
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if y < 1970 || !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return Err(bad());
    }
    let days = days_from_civil(y, mo, d);
    if civil_from_days(days) != (y, mo, d) {
        return Err(bad());
    }
    Ok(days as u64 * 86_400 + (h * 3600 + mi * 60 + se) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects() {
        for (t, s) in [(0, "1970-01-01T00:00:00Z"), (951_782_400, "2000-02-29T00:00:00Z"), (1_790_000_000, "2026-09-21T14:13:20Z"), (MAX, "9999-12-31T23:59:59Z")] {
            assert_eq!(format(t).unwrap(), s);
            assert_eq!(parse(s).unwrap(), t);
        }
        for s in ["2026-02-30T00:00:00Z", "2026-09-21T14:13:20.0Z", "2026-09-21T14:13:20+00:00", "2026-9-21T14:13:20Z", "1969-12-31T23:59:59Z", "2026-09-21t14:13:20Z"] {
            assert!(parse(s).is_err(), "{s}");
        }
        assert!(format(MAX + 1).is_err());
    }
}
