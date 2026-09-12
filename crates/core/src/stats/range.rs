//! Time bounds as typed by users (`--since 7d`, `--until 2026-09-05`) turned
//! into store timestamps.

use time::macros::format_description;
use time::{Date, Duration, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::trace::format_ts;

/// Which end of the range a value bounds; decides how a bare date is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundKind {
    /// Inclusive start: a date means its local midnight.
    Since,
    /// Exclusive end: a date means the local midnight *after* it, so
    /// `--until 2026-09-05` includes the 5th.
    Until,
}

/// Parses one bound into the store's `YYYY-MM-DDTHH:MM:SS.mmmZ` form.
///
/// Accepted: a relative age (`30m`, `12h`, `7d`, `2w`) measured back from
/// `now`; a date `YYYY-MM-DD` interpreted in the local `offset`; an RFC 3339
/// timestamp (`2026-09-01T12:00:00Z`, `2026-09-01T14:00:00+02:00`).
///
/// # Errors
/// A message naming the accepted forms.
pub fn parse_bound(
    text: &str,
    kind: BoundKind,
    now: OffsetDateTime,
    offset: UtcOffset,
) -> Result<String, String> {
    let text = text.trim();
    if let Some(age) = parse_age(text) {
        return Ok(format_ts(now - age));
    }
    if let Ok(date) = Date::parse(text, format_description!("[year]-[month]-[day]")) {
        let date = match kind {
            BoundKind::Since => date,
            BoundKind::Until => date
                .next_day()
                .ok_or_else(|| format!("date out of range: {text}"))?,
        };
        let local = PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_offset(offset);
        return Ok(format_ts(local));
    }
    if let Ok(t) = OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339) {
        return Ok(format_ts(t));
    }
    Err(format!(
        "cannot parse {text:?}: use an age like 7d/12h/30m/2w, a date YYYY-MM-DD, \
         or an RFC 3339 timestamp"
    ))
}

fn parse_age(text: &str) -> Option<Duration> {
    let (digits, unit) = text.split_at(text.len().checked_sub(1)?);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    let unit_secs = match unit {
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return None,
    };
    Some(Duration::seconds(n.checked_mul(unit_secs)?))
}

/// `+HH:MM` / `-HH:MM` for the `tz` field of the stats result.
pub fn format_offset(offset: UtcOffset) -> String {
    let total = offset.whole_seconds();
    let sign = if total < 0 { '-' } else { '+' };
    let abs = total.abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-12 10:30:00 UTC);

    #[test]
    fn relative_ages_count_back_from_now() {
        let p = |s| parse_bound(s, BoundKind::Since, NOW, UtcOffset::UTC).unwrap();
        assert_eq!(p("7d"), "2026-09-05T10:30:00.000Z");
        assert_eq!(p("12h"), "2026-09-11T22:30:00.000Z");
        assert_eq!(p("30m"), "2026-09-12T10:00:00.000Z");
        assert_eq!(p("1w"), "2026-09-05T10:30:00.000Z");
        assert_eq!(p(" 0d "), "2026-09-12T10:30:00.000Z");
    }

    #[test]
    fn dates_use_the_local_offset_and_until_is_inclusive() {
        let cest = UtcOffset::from_hms(2, 0, 0).unwrap();
        assert_eq!(
            parse_bound("2026-09-01", BoundKind::Since, NOW, cest).unwrap(),
            "2026-08-31T22:00:00.000Z"
        );
        assert_eq!(
            parse_bound("2026-09-05", BoundKind::Until, NOW, cest).unwrap(),
            "2026-09-05T22:00:00.000Z"
        );
    }

    #[test]
    fn rfc3339_is_normalised_to_utc() {
        assert_eq!(
            parse_bound(
                "2026-09-01T14:00:00+02:00",
                BoundKind::Since,
                NOW,
                UtcOffset::UTC
            )
            .unwrap(),
            "2026-09-01T12:00:00.000Z"
        );
        assert_eq!(
            parse_bound(
                "2026-09-01T12:00:00.5Z",
                BoundKind::Until,
                NOW,
                UtcOffset::UTC
            )
            .unwrap(),
            "2026-09-01T12:00:00.500Z"
        );
    }

    #[test]
    fn garbage_is_rejected_with_a_hint() {
        for s in [
            "",
            "d",
            "7",
            "7x",
            "-7d",
            "yesterday",
            "2026-13-01",
            "2026-09-01T25:00:00Z",
        ] {
            let err = parse_bound(s, BoundKind::Since, NOW, UtcOffset::UTC).unwrap_err();
            assert!(err.contains("RFC 3339"), "{s}: {err}");
        }
    }

    #[test]
    fn offsets_format_with_sign() {
        assert_eq!(format_offset(UtcOffset::UTC), "+00:00");
        assert_eq!(
            format_offset(UtcOffset::from_hms(5, 30, 0).unwrap()),
            "+05:30"
        );
        assert_eq!(
            format_offset(UtcOffset::from_hms(-7, 0, 0).unwrap()),
            "-07:00"
        );
    }
}
