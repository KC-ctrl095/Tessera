//! Bad scheduling input returns a useful error, never a panic or a wrapped time.
use tessera_core::jobs::{next_run, parse_duration, parse_when};

const NOW: u64 = 1_790_000_000_000;

#[test]
fn oversized_durations_and_sums_are_errors() {
    assert!(parse_duration("18446744073709551615s").is_err());
    assert!(parse_when("in 9223372036854775s 9223372036854775s", NOW).is_err());
    assert!(parse_when("in 18446744073709550s", NOW).is_err());
    assert!(next_run("@every 1m", u64::MAX - 1).is_err());
    assert_eq!(parse_duration("2h").unwrap(), 7_200_000);
    assert_eq!(parse_when("in 1h 30m", NOW).unwrap(), NOW + 5_400_000);
}

#[test]
fn large_cron_steps_do_not_wrap() {
    // The original u32 increment overflowed on the second field value.
    let step = "1/4294967295 * * * *";
    let first = next_run(step, NOW).unwrap();
    let second = next_run(step, first).unwrap();
    assert_eq!(second - first, 3_600_000);
    assert!(next_run("*/0 * * * *", NOW).is_err());
    assert!(next_run("5-1 * * * *", NOW).is_err());
}

#[test]
fn malformed_unicode_dates_are_errors() {
    for text in ["2026-12-2é", "2026-12-é", "2026-12-🍂", "éé-12-26"] {
        assert!(parse_when(text, NOW).is_err(), "{text}");
    }
}

#[test]
fn malformed_offsets_are_errors() {
    for text in [
        "2026-10-02T14:00:00+00:-128",
        "2026-10-02T14:00:00+00:-1",
        "2026-10-02T14:00:00+128:00",
        "2026-10-02T14:00:00+24:00",
        "2026-10-02T14:00:00+00:60",
    ] {
        assert!(parse_when(text, NOW).is_err(), "{text}");
    }
    assert_eq!(
        parse_when("2026-10-02T14:00:00-02:00", NOW).unwrap(),
        1_790_956_800_000
    );
}

#[test]
fn twelve_hour_times_require_one_through_twelve() {
    for text in ["0pm", "0am", "00:30pm", "13pm"] {
        assert!(parse_when(text, NOW).is_err(), "{text}");
    }
    assert_eq!(
        parse_when("2026-10-02 12amZ", NOW).unwrap(),
        1_790_899_200_000
    );
    assert_eq!(
        parse_when("2026-10-02 12pmZ", NOW).unwrap(),
        1_790_942_400_000
    );
}

#[test]
fn dates_before_the_epoch_do_not_become_future_reminders() {
    assert!(parse_when("1969-12-31 23:59Z", NOW).is_err());
    assert_eq!(parse_when("1970-01-01 00:00Z", NOW).unwrap(), 0);
}

#[test]
fn cron_search_near_the_calendar_limit_finds_representable_runs() {
    // Jiff reserves room for every supported zone offset at its boundary.
    // Use the final minute it can represent in the host's target-date zone,
    // not time::Date::MAX (which can resolve beyond Jiff's timestamp range).
    let last = (jiff::Timestamp::MAX.as_millisecond() as u64 / 60_000) * 60_000;
    assert_eq!(next_run("* * * * *", last - 60_000).unwrap(), last);
    assert!(next_run("* * * * *", last).is_err());
    assert!(next_run("* * * * *", last + 60_000).is_err());
}
