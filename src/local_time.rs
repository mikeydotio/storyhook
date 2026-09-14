//! A stored instant, rendered for a person in the process's own timezone
//! (SH-679).
//!
//! Every storyhook timestamp is stored, transported and journaled as RFC3339
//! UTC at one-second precision (`service::Clock::System`), and that string is
//! what `--json`, the SQL comparators, the lexical threshold filters and the
//! dashboard's sort keys all rely on. It is the wrong thing to *show* a
//! person: read raw, a UTC wall-clock passes for local time. The two functions
//! here are the CLI's and TUI's one door from a stored string to a displayed
//! one. The zone is the process's (`TZ`, then `/etc/localtime`), so
//! `TZ=UTC story show …` reproduces the stored string byte for byte.
//!
//! Both keep the RFC3339 grammar and only change the offset:
//! `2026-09-12T20:31:59Z` becomes `2026-09-12T13:31:59-07:00` in Los Angeles,
//! so every reader that parsed the old text still parses the new, and the
//! offset makes the zone explicit rather than implied. Nothing daemon-side
//! calls these: the daemon composes its own diagnostics in UTC with an
//! explicit `Z`, because the CLI's zone is not its to know.

use chrono::{DateTime, Local, SecondsFormat, TimeZone};

/// `at` rendered in the process's zone as RFC3339 at second precision.
///
/// A string that is not an RFC3339 instant comes back unchanged: the stored
/// value is the only evidence there is, and showing it unconverted beats
/// hiding it.
pub fn stamp(at: &str) -> String {
    stamp_in(at, &Local)
}

/// The calendar date of `at` in the process's zone, `YYYY-MM-DD` — what a
/// fixed-width date column has room for. Unparseable input comes back
/// unchanged, as for [`stamp`].
pub fn day(at: &str) -> String {
    day_in(at, &Local)
}

/// The current instant in the process's zone, in the shape of [`stamp`].
pub fn now_stamp() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// [`stamp`] with an explicit zone, for callers and tests that must not
/// depend on the process's ambient `TZ`.
pub fn stamp_in<Tz: TimeZone>(at: &str, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match parse(at) {
        // `use_z` prints `Z` when the offset is zero, so a UTC process shows
        // the stored string unchanged rather than `+00:00`.
        Some(instant) => instant
            .with_timezone(zone)
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        None => at.to_string(),
    }
}

/// [`day`] with an explicit zone.
pub fn day_in<Tz: TimeZone>(at: &str, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match parse(at) {
        Some(instant) => instant.with_timezone(zone).format("%Y-%m-%d").to_string(),
        None => at.to_string(),
    }
}

fn parse(at: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(at).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};

    const STORED: &str = "2026-03-01T23:30:00Z";

    fn east(hours: i32) -> FixedOffset {
        FixedOffset::east_opt(hours * 3600).unwrap()
    }

    #[test]
    fn a_utc_zone_reproduces_the_stored_string_byte_for_byte() {
        assert_eq!(stamp_in(STORED, &Utc), STORED);
        assert_eq!(stamp_in(STORED, &east(0)), STORED);
        assert_eq!(day_in(STORED, &Utc), "2026-03-01");
    }

    #[test]
    fn an_eastern_zone_rolls_the_date_forward_and_names_its_offset() {
        assert_eq!(stamp_in(STORED, &east(9)), "2026-03-02T08:30:00+09:00");
        assert_eq!(day_in(STORED, &east(9)), "2026-03-02");
    }

    #[test]
    fn a_western_zone_keeps_the_date_and_names_its_offset() {
        assert_eq!(stamp_in(STORED, &east(-10)), "2026-03-01T13:30:00-10:00");
        assert_eq!(day_in(STORED, &east(-10)), "2026-03-01");
    }

    #[test]
    fn a_western_zone_rolls_the_date_back_across_midnight() {
        assert_eq!(
            stamp_in("2026-03-02T03:00:00Z", &east(-7)),
            "2026-03-01T20:00:00-07:00"
        );
        assert_eq!(day_in("2026-03-02T03:00:00Z", &east(-7)), "2026-03-01");
    }

    #[test]
    fn a_stored_offset_other_than_z_is_still_an_instant() {
        assert_eq!(
            stamp_in("2026-03-01T23:30:00+02:00", &Utc),
            "2026-03-01T21:30:00Z"
        );
    }

    #[test]
    fn sub_second_precision_is_dropped_not_rounded() {
        assert_eq!(
            stamp_in("2026-03-01T23:30:00.987Z", &Utc),
            "2026-03-01T23:30:00Z"
        );
    }

    #[test]
    fn a_string_that_is_not_an_instant_is_shown_unchanged() {
        for raw in ["", "no", "2026-03-01", "2026-03-01 23:30:00", "[timestamp]"] {
            assert_eq!(stamp_in(raw, &east(9)), raw, "stamp of {raw:?}");
            assert_eq!(day_in(raw, &east(9)), raw, "day of {raw:?}");
        }
    }

    #[test]
    fn the_process_zone_doors_keep_the_rfc3339_grammar() {
        // Whatever zone the test process runs in, the instant survives the
        // round trip and the grammar stays parseable.
        let shown = stamp(STORED);
        let back = DateTime::parse_from_rfc3339(&shown).expect("stamp() output parses");
        assert_eq!(
            back.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true),
            STORED
        );
        assert_eq!(day(STORED), shown[..10]);
        DateTime::parse_from_rfc3339(&now_stamp()).expect("now_stamp() output parses");
    }
}
