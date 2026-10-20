//! Schedule parsing for background jobs.

use anyhow::{Result, anyhow, bail};

use crate::util::now_ms;

/// Compatibility entry point; offsets are no longer captured at startup.
pub fn init_local_offset() {}

fn local_zone() -> Result<jiff::tz::TimeZone> {
    jiff::tz::TimeZone::try_system()
        .map_err(|e| anyhow!("cannot determine the server time zone: {e}"))
}

/// Current offset for display callers, never used to schedule a future instant.
pub fn local_offset() -> time::UtcOffset {
    local_zone()
        .ok()
        .and_then(|tz| {
            time::UtcOffset::from_whole_seconds(tz.to_offset(jiff::Timestamp::now()).seconds()).ok()
        })
        .unwrap_or(time::UtcOffset::UTC)
}

pub(crate) fn local_datetime(dt: time::PrimitiveDateTime) -> Result<u64> {
    resolve_local(dt, &local_zone()?)
}

fn resolve_local(dt: time::PrimitiveDateTime, tz: &jiff::tz::TimeZone) -> Result<u64> {
    let civil = jiff::civil::DateTime::new(
        dt.year() as i16,
        u8::from(dt.month()) as i8,
        dt.day() as i8,
        dt.hour() as i8,
        dt.minute() as i8,
        dt.second() as i8,
        dt.nanosecond() as i32,
    )?;
    let ambiguous = tz.to_ambiguous_timestamp(civil);
    let timestamp = match ambiguous.offset() {
        jiff::tz::AmbiguousOffset::Gap { .. } => {
            // The transition itself is the first valid instant after a gap.
            tz.following(ambiguous.earlier()?)
                .next()
                .ok_or_else(|| anyhow!("no transition resolves this local time"))?
                .timestamp()
        }
        _ => ambiguous.earlier()?, // folded clock time: first occurrence only
    };
    u64::try_from(timestamp.as_millisecond()).map_err(|_| anyhow!("scheduled time is before 1970"))
}

fn local_at(ms: u64, tz: &jiff::tz::TimeZone) -> Result<time::PrimitiveDateTime> {
    // Jiff's millisecond constructor rejects the fractional part of its
    // last supported second. Preserve it through the seconds/nanos form.
    let dt = tz.to_datetime(jiff::Timestamp::new(
        i64::try_from(ms / 1000).map_err(|_| anyhow!("date is out of range"))?,
        ((ms % 1000) * 1_000_000) as i32,
    )?);
    Ok(time::Date::from_calendar_date(
        dt.year().into(),
        time::Month::try_from(dt.month() as u8)?,
        dt.day() as u8,
    )?
    .with_hms(dt.hour() as u8, dt.minute() as u8, dt.second() as u8)?)
}

/// Next run time (unix ms) strictly after `after_ms`.
pub fn next_run(schedule: &str, after_ms: u64) -> Result<u64> {
    next_run_in(schedule, after_ms, &local_zone()?)
}

fn next_run_in(schedule: &str, after_ms: u64, tz: &jiff::tz::TimeZone) -> Result<u64> {
    let s = schedule.trim();
    if let Some(rest) = s.strip_prefix("@every").map(str::trim) {
        let ms = parse_duration(rest)?;
        if ms < 60_000 {
            bail!("the minimum interval is 1 minute");
        }
        return after_ms
            .checked_add(ms)
            .ok_or_else(|| anyhow!("scheduled time is too far in the future"));
    }
    let expr = match s {
        "@hourly" => "0 * * * *",
        "@daily" | "@midnight" => "0 0 * * *",
        "@weekly" => "0 0 * * 0",
        "@monthly" => "0 0 1 * *",
        _ => s,
    };
    Cron::parse(expr)?
        .next_after_in(after_ms, tz)
        .ok_or_else(|| anyhow!("schedule `{expr}` never fires"))
}

/// Standard 5-field cron (minute hour day-of-month month day-of-week) with
/// lists, ranges, steps and month/day names. A leading seconds field is ignored.
struct Cron {
    minutes: u64,
    hours: u32,
    dom: u32,
    months: u16,
    dow: u8,
    dom_any: bool,
    dow_any: bool,
}

impl Cron {
    fn parse(expr: &str) -> Result<Self> {
        let mut fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() == 6 {
            fields.remove(0);
        }
        if fields.len() != 5 {
            bail!("invalid cron expression `{expr}`: expected 5 fields");
        }
        const MONTHS: [&str; 12] = [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ];
        const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
        let minutes = field(fields[0], 0, 59, &[])?;
        let hours = field(fields[1], 0, 23, &[])? as u32;
        let dom = field(fields[2], 1, 31, &[])? as u32;
        let months = (field(fields[3], 1, 12, &MONTHS)? >> 1) as u16;
        let mut dow = field(fields[4], 0, 7, &DAYS)?;
        if dow & (1 << 7) != 0 {
            dow |= 1; // 7 = Sunday
        }
        Ok(Self {
            minutes,
            hours,
            dom,
            months,
            dow: (dow & 0x7f) as u8,
            dom_any: fields[2] == "*" || fields[2] == "?",
            dow_any: fields[4] == "*" || fields[4] == "?",
        })
    }

    fn matches(&self, t: &time::OffsetDateTime) -> bool {
        let dom_ok = self.dom & (1 << t.day()) != 0;
        let dow_ok = self.dow & (1 << t.weekday().number_days_from_sunday()) != 0;
        let day_ok = match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (false, true) => dom_ok,
            (true, false) => dow_ok,
            (false, false) => dom_ok || dow_ok, // vixie-cron semantics
        };
        day_ok
            && self.months & (1 << (u8::from(t.month()) - 1)) != 0
            && self.hours & (1 << t.hour()) != 0
            && self.minutes & (1 << t.minute()) != 0
    }

    fn next_after_in(&self, after_ms: u64, tz: &jiff::tz::TimeZone) -> Option<u64> {
        let from = local_at(after_ms, tz).ok()?;
        // Include today's earlier civil candidates: a skipped time may still
        // resolve to a transition instant in the future.
        let mut t = from.date().midnight();
        let limit = from
            .checked_add(time::Duration::days(366 * 5))
            .unwrap_or(time::PrimitiveDateTime::MAX);
        while t <= limit {
            if self.matches(&t.assume_utc())
                && let Ok(at) = resolve_local(t, tz)
                && at > after_ms
            {
                return Some(at);
            }
            if self.months & (1 << (u8::from(t.month()) - 1)) == 0
                || self.hours & (1 << t.hour()) == 0
            {
                t = t
                    .replace_minute(0)
                    .ok()?
                    .checked_add(time::Duration::hours(1))?;
            } else {
                t = t.checked_add(time::Duration::minutes(1))?;
            }
        }
        None
    }

    #[cfg(test)]
    fn next_after(&self, from: time::OffsetDateTime) -> Option<time::OffsetDateTime> {
        let tz = jiff::tz::TimeZone::fixed(
            jiff::tz::Offset::from_seconds(from.offset().whole_seconds()).ok()?,
        );
        let ms = self.next_after_in(
            u64::try_from(from.unix_timestamp_nanos() / 1_000_000).ok()?,
            &tz,
        )?;
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
            .ok()
            .and_then(|t| t.checked_to_offset(from.offset()))
    }
}

fn field(src: &str, min: u32, max: u32, names: &[&str]) -> Result<u64> {
    let value = |s: &str| -> Result<u32> {
        let l = s.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == l) {
            return Ok(i as u32 + min.max(if names.len() == 12 { 1 } else { 0 }));
        }
        let v: u32 = s.parse().map_err(|_| anyhow!("invalid cron value `{s}`"))?;
        if v < min || v > max {
            bail!("cron value {v} out of range {min}-{max}");
        }
        Ok(v)
    };
    let mut bits = 0u64;
    for part in src.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (
                r,
                s.parse::<u32>()
                    .map_err(|_| anyhow!("invalid step `{s}`"))?,
            ),
            None => (part, 1),
        };
        if step == 0 {
            bail!("cron step must be at least 1");
        }
        let (lo, hi) = if range == "*" || range == "?" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
            (v, if part.contains('/') { max } else { v })
        };
        if lo > hi {
            bail!("cron range `{range}` starts after it ends");
        }
        for v in (lo..=hi).step_by(step as usize) {
            bits |= 1 << v;
        }
    }
    Ok(bits)
}

pub fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().map_err(|_| anyhow!("invalid duration `{s}`"))?;
    let mult = match unit.trim() {
        "s" | "sec" | "secs" => 1_000,
        "m" | "min" | "mins" | "" => 60_000,
        "h" | "hr" | "hour" | "hours" => 3_600_000,
        "d" | "day" | "days" => 86_400_000,
        other => bail!("unknown duration unit `{other}`"),
    };
    n.checked_mul(mult)
        .ok_or_else(|| anyhow!("duration is too large"))
}

/// When a one-off thing should happen (unix ms): `in 20m`, `in 1h 30m`,
/// `15:30`, `3pm`, `tomorrow 9:00`, `2026-10-02 14:00`, or RFC 3339. Times
/// without a date are the next such time; all are server-local unless an
/// offset is given.
pub fn parse_when(s: &str, now_ms: u64) -> Result<u64> {
    parse_when_in(s, now_ms, &local_zone()?)
}

fn parse_when_in(s: &str, now_ms: u64, tz: &jiff::tz::TimeZone) -> Result<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s == "now" {
        return Ok(now_ms);
    }
    let now = local_at(now_ms, tz)?;
    let to_ms = |t: time::OffsetDateTime| {
        u64::try_from(t.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| anyhow!("scheduled time is before 1970"))
    };
    if let Some(rest) = s.strip_prefix("in ").or_else(|| {
        s.chars()
            .next()
            .filter(char::is_ascii_digit)
            .and(s.find(|c: char| c.is_ascii_alphabetic()))
            .filter(|_| {
                !s.contains(':') && !s.contains('-') && !s.ends_with("am") && !s.ends_with("pm")
            })
            .map(|_| s.as_str())
    }) {
        let mut total = 0u64;
        let mut num = String::new();
        let mut unit = String::new();
        let flush = |num: &mut String, unit: &mut String, total: &mut u64| -> Result<()> {
            if !num.is_empty() {
                *total = total
                    .checked_add(parse_duration(&format!("{num}{}", unit.trim()))?)
                    .ok_or_else(|| anyhow!("duration is too large"))?;
            } else if !unit.trim().is_empty() && unit.trim() != "and" {
                bail!("invalid duration `{rest}`");
            }
            num.clear();
            unit.clear();
            Ok(())
        };
        for c in rest.chars() {
            if c.is_ascii_digit() {
                if !unit.trim().is_empty() {
                    flush(&mut num, &mut unit, &mut total)?;
                }
                num.push(c);
            } else if c == ',' {
                flush(&mut num, &mut unit, &mut total)?;
            } else {
                unit.push(c);
            }
        }
        flush(&mut num, &mut unit, &mut total)?;
        if total == 0 {
            bail!("invalid duration `{rest}`");
        }
        return now_ms
            .checked_add(total)
            .ok_or_else(|| anyhow!("scheduled time is too far in the future"));
    }
    // A clock time: `15:30`, `9`, `3pm`, `3:15 pm`.
    let clock = |t: &str| -> Result<time::Time> {
        let t = t.trim();
        let (t, pm) = match t.strip_suffix("pm").or_else(|| t.strip_suffix("p.m.")) {
            Some(r) => (r.trim(), Some(true)),
            None => match t.strip_suffix("am").or_else(|| t.strip_suffix("a.m.")) {
                Some(r) => (r.trim(), Some(false)),
                None => (t, None),
            },
        };
        let (h, m) = t.split_once(':').unwrap_or((t, "0"));
        let (m, sec) = m.split_once(':').unwrap_or((m, "0"));
        let mut h: u8 = h
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid time `{t}`"))?;
        let m: u8 = m
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid time `{t}`"))?;
        let sec: u8 = sec
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid time `{t}`"))?;
        if pm.is_some() && !(1..=12).contains(&h) {
            bail!("invalid time `{t}`");
        }
        match pm {
            Some(true) if h < 12 => h += 12,
            Some(false) if h == 12 => h = 0,
            Some(_) if h > 12 => bail!("invalid time `{t}`"),
            _ => {}
        }
        time::Time::from_hms(h, m, sec).map_err(|_| anyhow!("invalid time `{t}`"))
    };
    if let Some(rest) = s.strip_prefix("tomorrow") {
        let rest = rest.trim().trim_start_matches("at").trim();
        let at = if rest.is_empty() {
            time::Time::from_hms(9, 0, 0)?
        } else {
            clock(rest)?
        };
        let day = now
            .date()
            .next_day()
            .ok_or_else(|| anyhow!("no tomorrow"))?;
        let target = resolve_local(day.with_time(at), tz)?;
        if target <= now_ms {
            bail!("tomorrow is outside the supported date range");
        }
        return Ok(target);
    }
    if let Some(date) = s
        .get(..10)
        .filter(|date| date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-')
    {
        // A date, with a time (`2026-10-02 14:00`, `2026-10-02T14:00:00+02:00`) or without (9:00).
        let date = time::Date::parse(
            date,
            time::macros::format_description!("[year]-[month]-[day]"),
        )
        .map_err(|_| anyhow!("invalid date"))?;
        let rest = s[10..].trim_start_matches(['t', ' ']).trim();
        let (rest, zone) = if let Some(r) = rest.strip_suffix('z') {
            (r, Some(time::UtcOffset::UTC))
        } else if let Some(i) = rest.rfind(['+', '-']).filter(|&i| i > 0) {
            let z = &rest[i..];
            let sign: i8 = if z.starts_with('-') { -1 } else { 1 };
            let (zh, zm) = z[1..].split_once(':').unwrap_or((&z[1..], "0"));
            let number = |s: &str| -> Result<i8> {
                let n: u8 = s.parse().map_err(|_| anyhow!("invalid offset `{z}`"))?;
                i8::try_from(n).map_err(|_| anyhow!("invalid offset `{z}`"))
            };
            let zh = number(zh)?;
            let zm = number(zm)?;
            // `time` supports offsets beyond ISO 8601's 23:59 limit.
            if zh > 23 || zm > 59 {
                bail!("invalid offset `{z}`");
            }
            (
                &rest[..i],
                Some(time::UtcOffset::from_hms(sign * zh, sign * zm, 0).map_err(|e| anyhow!(e))?),
            )
        } else {
            (rest, None)
        };
        let at = if rest.trim().is_empty() {
            time::Time::from_hms(9, 0, 0)?
        } else {
            clock(rest.split('.').next().unwrap_or(rest))?
        };
        return if let Some(zone) = zone {
            to_ms(date.with_time(at).assume_offset(zone))
        } else {
            resolve_local(date.with_time(at), tz)
        };
    }
    let at = clock(
        s.trim_start_matches("today")
            .trim()
            .trim_start_matches("at")
            .trim(),
    )?;
    let mut t = now.date().with_time(at);
    if resolve_local(t, tz)?.saturating_add(30_000) < now_ms {
        t = t
            .checked_add(time::Duration::days(1))
            .ok_or_else(|| anyhow!("scheduled time is too far in the future"))?;
    }
    resolve_local(t, tz)
}

pub fn fmt_time(ms: u64) -> String {
    let Ok(tz) = local_zone() else {
        return ms.to_string();
    };
    fmt_time_in(ms, &tz)
}

fn fmt_time_in(ms: u64, tz: &jiff::tz::TimeZone) -> String {
    let Ok(t) = local_at(ms, tz) else {
        return ms.to_string();
    };
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    t.format(&fmt).unwrap_or_else(|_| ms.to_string())
}

pub fn validate(schedule: &str) -> Result<u64> {
    next_run(schedule, now_ms())
}

/// Why `job`'s schedule is over at `now` (its runs used up, its end date
/// come), if it is.
pub fn schedule_over(job: &tessera_proto::Job, now: u64) -> Option<String> {
    if let Some(max) = job.max_runs.filter(|m| job.runs >= *m) {
        return Some(if max == 1 {
            "it ran once, as set".into()
        } else {
            format!("it ran {max} times, as set")
        });
    }
    job.until
        .filter(|u| now >= *u)
        .map(|_| "its end date came".into())
}

/// The next run after `after`, if one comes before the end date.
pub fn next_run_within(job: &tessera_proto::Job, after: u64) -> Option<u64> {
    next_run(&job.schedule, after)
        .ok()
        .filter(|t| job.until.is_none_or(|u| *t < u))
}

/// An end date as people say it: a bare date means the end of that day.
pub fn parse_until(s: &str, now_ms: u64) -> Result<u64> {
    parse_until_in(s, now_ms, &local_zone()?)
}

fn parse_until_in(s: &str, now_ms: u64, tz: &jiff::tz::TimeZone) -> Result<u64> {
    let s = s.trim();
    if let Ok(date) =
        time::Date::parse(s, time::macros::format_description!("[year]-[month]-[day]"))
    {
        return resolve_local(
            date.next_day()
                .ok_or_else(|| anyhow!("the end date is out of range"))?
                .midnight(),
            tz,
        );
    }
    parse_when_in(s, now_ms, tz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_and_cron() {
        assert_eq!(next_run("@every 30m", 0).unwrap(), 30 * 60_000);
        assert!(next_run("@every 10s", 0).is_err());
        let now = now_ms();
        let n = next_run("*/5 * * * *", now).unwrap();
        assert!(n > now && n <= now + 5 * 60_000 + 60_000);
        assert!(next_run("not a cron", now).is_err());
        assert!(next_run("@daily", now).unwrap() <= now + 86_400_000 + 60_000);
        assert!(next_run("0 9 * * mon-fri", now).is_ok());
        assert!(next_run("0 0 31 2 *", now).is_err(), "Feb 31 never fires");
    }

    #[test]
    fn calendar_horizon_clamps_in_the_current_offset() {
        let cron = Cron::parse("* * * * *").unwrap();
        for hours in [-12, 0, 12] {
            let offset = time::UtcOffset::from_hms(hours, 0, 0).unwrap();
            let last = time::OffsetDateTime::from_unix_timestamp_nanos(
                i128::from(jiff::Timestamp::MAX.as_millisecond()) * 1_000_000,
            )
            .unwrap()
            .to_offset(offset);
            let last = if last.year() > 9999 {
                time::Date::MAX
                    .with_hms(23, 59, 0)
                    .unwrap()
                    .assume_offset(offset)
            } else {
                last.replace_second(0)
                    .unwrap()
                    .replace_nanosecond(0)
                    .unwrap()
            };
            assert_eq!(
                cron.next_after(last - time::Duration::minutes(1)),
                Some(last)
            );
            assert!(cron.next_after(last).is_none());
        }
    }

    #[test]
    fn target_zone_conversion_is_fallible_and_formatting_never_panics_or_goes_blank() {
        let maximum = jiff::Timestamp::MAX.as_millisecond() as u64;
        for hours in [-12, 0, 12] {
            let tz = jiff::tz::TimeZone::fixed(jiff::tz::offset(hours));
            assert!(local_at(maximum, &tz).is_ok());
            assert!(parse_when_in("tomorrow 09:00", maximum, &tz).is_err());
            assert!(!fmt_time_in(maximum, &tz).is_empty());
            for invalid in [maximum + 1, u64::MAX] {
                assert!(local_at(invalid, &tz).is_err());
                assert!(next_run_in("* * * * *", invalid, &tz).is_err());
                assert!(parse_when_in("09:00", invalid, &tz).is_err());
                assert_eq!(fmt_time_in(invalid, &tz), invalid.to_string());
            }
        }
        let tz = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        assert_eq!(
            fmt_time_in(utc("2026-11-01T09:30:00Z"), &tz),
            "2026-11-01 03:30"
        );
        assert_eq!(
            fmt_time_in(utc("2026-03-08T08:30:00Z"), &tz),
            "2026-03-08 03:30"
        );
    }
    #[test]
    fn one_off_times() {
        let now = 1_790_000_000_000; // 2026-09-21
        assert_eq!(parse_when("in 20m", now).unwrap(), now + 20 * 60_000);
        assert_eq!(parse_when("in 1h 30m", now).unwrap(), now + 90 * 60_000);
        assert_eq!(parse_when("2h", now).unwrap(), now + 2 * 3_600_000);
        assert_eq!(parse_when("now", now).unwrap(), now);
        assert!(parse_when("in soon", now).is_err());
        let utc = parse_when("2026-10-02T14:00:00Z", now).unwrap();
        assert_eq!(utc, 1_790_949_600_000);
        assert_eq!(parse_when("2026-10-02 16:00+02:00", now).unwrap(), utc);
        // Clock times are the next such time, within a day.
        for t in ["15:30", "3pm", "3:30 pm", "tomorrow 9:00", "tomorrow"] {
            let at = parse_when(t, now).unwrap();
            assert!(at > now && at <= now + 2 * 86_400_000, "{t}");
        }
        assert!(parse_when("25:00", now).is_err());
        assert!(parse_when("13pm", now).is_err());
    }

    #[test]
    fn a_schedule_ends_by_runs_or_date() {
        let now = 1_790_000_000_000;
        let mut job = tessera_proto::Job {
            schedule: "@every 1h".into(),
            ..Default::default()
        };
        assert_eq!(schedule_over(&job, now), None);
        assert_eq!(next_run_within(&job, now), Some(now + 3_600_000));
        job.max_runs = Some(2);
        job.runs = 1;
        assert_eq!(schedule_over(&job, now), None);
        job.runs = 2;
        assert_eq!(
            schedule_over(&job, now).as_deref(),
            Some("it ran 2 times, as set")
        );
        job.max_runs = Some(1);
        assert_eq!(
            schedule_over(&job, now).as_deref(),
            Some("it ran once, as set")
        );
        job.max_runs = None;
        job.until = Some(now + 30 * 60_000);
        assert_eq!(schedule_over(&job, now), None);
        assert_eq!(next_run_within(&job, now), None, "no run before the end");
        assert_eq!(
            schedule_over(&job, now + 30 * 60_000).as_deref(),
            Some("its end date came")
        );
        // A bare date lasts the whole day.
        let end = parse_until("2026-09-22", now).unwrap();
        assert_eq!(end, parse_when("2026-09-23 00:00", now).unwrap());
        assert!(end > parse_when("2026-09-22 12:00", now).unwrap());
        assert_eq!(parse_until("in 2d", now).unwrap(), now + 2 * 86_400_000);
    }

    fn utc(s: &str) -> u64 {
        s.parse::<jiff::Timestamp>().unwrap().as_millisecond() as u64
    }

    #[test]
    fn chicago_2027_march_8_is_ordinary_and_march_14_is_the_gap() {
        let tz = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        let before = utc("2027-03-07T18:00:00Z");
        for parser in [parse_when_in, parse_until_in] {
            assert_eq!(
                parser("2027-03-08 02:30", before, &tz).unwrap(),
                utc("2027-03-08T08:30:00Z")
            );
            assert_eq!(
                parser("2027-03-14 02:30", before, &tz).unwrap(),
                utc("2027-03-14T08:00:00Z")
            );
            assert_eq!(
                parser("2027-03-14 02:30-06:00", before, &tz).unwrap(),
                utc("2027-03-14T08:30:00Z")
            );
        }
        assert_eq!(
            next_run_in("30 2 * * *", utc("2027-03-08T07:59:00Z"), &tz).unwrap(),
            utc("2027-03-08T08:30:00Z")
        );
        assert_eq!(
            next_run_in("30 2 * * *", utc("2027-03-14T07:59:00Z"), &tz).unwrap(),
            utc("2027-03-14T08:00:00Z")
        );
        assert_eq!(
            parse_when_in("tomorrow 9:00", utc("2027-03-13T18:00:00Z"), &tz).unwrap(),
            utc("2027-03-14T14:00:00Z")
        );
        assert_eq!(
            parse_until_in("2027-03-08", before, &tz).unwrap(),
            utc("2027-03-09T06:00:00Z")
        );
        assert_eq!(
            parse_until_in("2027-03-14", before, &tz).unwrap(),
            utc("2027-03-15T05:00:00Z")
        );
    }
    #[test]
    fn chicago_2027_november_7_fold_uses_only_first_occurrence_and_date_ends_afterward() {
        let tz = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        let before = utc("2027-11-06T12:00:00Z");
        for parser in [parse_when_in, parse_until_in] {
            assert_eq!(
                parser("2027-11-07 01:30", before, &tz).unwrap(),
                utc("2027-11-07T06:30:00Z")
            );
            assert_eq!(
                parser("2027-11-07 01:30-05:00", before, &tz).unwrap(),
                utc("2027-11-07T06:30:00Z")
            );
            assert_eq!(
                parser("2027-11-07 01:30-06:00", before, &tz).unwrap(),
                utc("2027-11-07T07:30:00Z")
            );
        }
        let first = next_run_in("30 1 * * *", utc("2027-11-07T05:00:00Z"), &tz).unwrap();
        assert_eq!(first, utc("2027-11-07T06:30:00Z"));
        for after in [first, utc("2027-11-07T07:00:00Z")] {
            assert_eq!(
                next_run_in("30 1 * * *", after, &tz).unwrap(),
                utc("2027-11-08T07:30:00Z")
            );
        }
        let end = parse_until_in("2027-11-07", before, &tz).unwrap();
        assert_eq!(end, utc("2027-11-08T06:00:00Z"));
        let job = tessera_proto::Job {
            schedule: "@every 1m".into(),
            until: Some(end),
            ..Default::default()
        };
        assert_eq!(next_run_within(&job, end - 60_001), Some(end - 1));
        assert!(next_run_within(&job, end - 60_000).is_none());
        assert!(schedule_over(&job, end).is_some());
    }
    #[test]
    fn chicago_rules_at_the_target_instant_not_the_daemon_start_offset() {
        let tz = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        let october = utc("2026-10-12T12:00:00Z");
        assert_eq!(
            parse_when_in("2026-11-01 03:30", october, &tz).unwrap(),
            utc("2026-11-01T09:30:00Z")
        );
        let next = next_run_in("30 3 * * *", utc("2026-10-31T12:00:00Z"), &tz).unwrap();
        assert_eq!(next, utc("2026-11-01T09:30:00Z"));
        assert_eq!(
            parse_when_in("tomorrow 03:30", utc("2026-10-31T12:00:00Z"), &tz).unwrap(),
            next
        );
        assert_eq!(
            parse_until_in("2026-11-01 03:30", october, &tz).unwrap(),
            next
        );
        assert_eq!(
            parse_when_in("2026-11-01 03:30-05:00", october, &tz).unwrap(),
            utc("2026-11-01T08:30:00Z"),
            "explicit offsets remain exact"
        );
    }

    #[test]
    fn chicago_gaps_use_first_valid_instant_and_folds_only_the_first_occurrence() {
        let tz = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        let spring = utc("2026-03-08T07:59:00Z");
        for parser in [parse_when_in, parse_until_in] {
            assert_eq!(
                parser("2026-03-08 02:30", spring, &tz).unwrap(),
                utc("2026-03-08T08:00:00Z")
            );
            assert_eq!(
                parser("2026-11-01 01:30", spring, &tz).unwrap(),
                utc("2026-11-01T06:30:00Z")
            );
        }
        assert_eq!(
            next_run_in("30 2 * * *", spring, &tz).unwrap(),
            utc("2026-03-08T08:00:00Z")
        );
        let first = next_run_in("30 1 * * *", utc("2026-11-01T05:00:00Z"), &tz).unwrap();
        assert_eq!(first, utc("2026-11-01T06:30:00Z"));
        assert_eq!(
            next_run_in("30 1 * * *", first, &tz).unwrap(),
            utc("2026-11-02T07:30:00Z")
        );
        assert_eq!(
            next_run_in("30 1 * * *", utc("2026-11-01T07:00:00Z"), &tz).unwrap(),
            utc("2026-11-02T07:30:00Z")
        );
    }

    #[test]
    fn until_is_exclusive_and_a_date_is_the_following_local_midnight_even_on_dst_days() {
        let tz = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        assert_eq!(
            parse_until_in("2026-11-01", 0, &tz).unwrap(),
            utc("2026-11-02T06:00:00Z")
        );
        assert_eq!(
            parse_until_in("2026-03-08", 0, &tz).unwrap(),
            utc("2026-03-09T05:00:00Z")
        );
        let end = utc("2026-11-02T06:00:00Z");
        let job = tessera_proto::Job {
            schedule: "@every 1m".into(),
            until: Some(end),
            ..Default::default()
        };
        assert_eq!(
            next_run_within(&job, end - 120_000),
            Some(end - 60_000),
            "the final minute of the day is included"
        );
        assert!(next_run_within(&job, end - 60_000).is_none());
        assert_eq!(next_run_within(&job, end - 60_001), Some(end - 1));
        assert!(schedule_over(&job, end - 1).is_none());
        assert!(schedule_over(&job, end).is_some());
        assert!(schedule_over(&job, end + 1).is_some());
    }
}
