use chrono::{DateTime, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

const SESSION_LIMIT_MARKER: &str = "You've hit your session limit · resets ";

#[derive(Debug, Clone, PartialEq)]
pub struct SessionLimit {
    pub reset_at: DateTime<Utc>,
    pub reset_time: NaiveTime,
    pub timezone: Tz,
}

impl SessionLimit {
    pub fn label_time(&self) -> String {
        // Render a compact 12-hour clock without an AM/PM suffix for the status column.
        let hour = match self.reset_time.hour() % 12 {
            0 => 12,
            hour => hour,
        };
        format!("{hour}:{:02}", self.reset_time.minute())
    }

    #[cfg(test)]
    pub fn seconds_until(&self, now: DateTime<Utc>) -> u64 {
        // An expired deadline should continue immediately instead of producing a negative sleep.
        (self.reset_at - now).num_seconds().max(0) as u64
    }
}

pub fn parse_session_limit(line: &str, reference: DateTime<Utc>) -> Option<SessionLimit> {
    // Require the complete reset marker, a clock, and a parenthesized IANA timezone.
    let suffix = line.split_once(SESSION_LIMIT_MARKER)?.1;
    let (clock, timezone) = suffix.split_once(" (")?;
    let timezone = timezone.strip_suffix(')')?.parse::<Tz>().ok()?;
    let reset_time = parse_clock(clock)?;
    let reset_at = resolve_reset_at(reset_time, timezone, reference)?;

    Some(SessionLimit {
        reset_at,
        reset_time,
        timezone,
    })
}

fn parse_clock(clock: &str) -> Option<NaiveTime> {
    // Split the meridiem before validating the numeric 12-hour clock.
    let clock = clock.to_ascii_lowercase();
    let (clock, is_pm) = if let Some(value) = clock.strip_suffix("am") {
        (value, false)
    } else {
        (clock.strip_suffix("pm")?, true)
    };
    let (hour, minute) = clock.split_once(':')?;
    let hour = hour.parse::<u32>().ok()?;
    let minute = minute.parse::<u32>().ok()?;
    if !(1..=12).contains(&hour) || minute > 59 {
        return None;
    }

    // Convert noon and midnight correctly into chrono's 24-hour representation.
    let hour = (hour % 12) + if is_pm { 12 } else { 0 };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn resolve_reset_at(
    reset_time: NaiveTime,
    timezone: Tz,
    reference: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    // Resolve the first matching wall-clock time after the rate-limit event.
    let reference = reference.with_timezone(&timezone);
    let mut date = reference.date_naive();
    let mut candidate = timezone
        .from_local_datetime(&date.and_time(reset_time))
        .earliest()?;

    // A clock earlier than the event belongs to the following local date.
    if candidate <= reference {
        date = date.succ_opt()?;
        candidate = timezone
            .from_local_datetime(&date.and_time(reset_time))
            .earliest()?;
    }

    Some(candidate.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn parses_live_session_limit_marker() {
        // Resolve the live Nicosia marker against an event shortly before reset.
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
        let limit = parse_session_limit(
            "⎿  You've hit your session limit · resets 1:10am (Asia/Nicosia)",
            reference,
        )
        .unwrap();

        assert_eq!(limit.label_time(), "1:10");
        assert_eq!(
            limit.reset_at,
            Utc.with_ymd_and_hms(2026, 7, 13, 22, 10, 0).unwrap()
        );
    }

    #[test]
    fn parses_pm_reset_time() {
        // Confirm that PM conversion and a negative UTC offset are both honored.
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 8, 0, 0).unwrap();
        let limit = parse_session_limit(
            "You've hit your session limit · resets 1:10pm (America/New_York)",
            reference,
        )
        .unwrap();

        assert_eq!(
            limit.reset_at,
            Utc.with_ymd_and_hms(2026, 7, 13, 17, 10, 0).unwrap()
        );
    }

    #[test]
    fn resolves_after_midnight_against_event_time() {
        // A reset clock earlier than the event clock belongs to the following day.
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 30, 0).unwrap();
        let limit = parse_session_limit(
            "You've hit your session limit · resets 1:10am (Asia/Nicosia)",
            reference,
        )
        .unwrap();

        assert_eq!(
            limit.reset_at,
            Utc.with_ymd_and_hms(2026, 7, 13, 22, 10, 0).unwrap()
        );
    }

    #[test]
    fn elapsed_deadline_has_zero_wait() {
        // Never turn an elapsed reset deadline into a negative sleep duration.
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
        let limit = parse_session_limit(
            "You've hit your session limit · resets 1:10am (Asia/Nicosia)",
            reference,
        )
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 13, 22, 11, 0).unwrap();

        assert_eq!(limit.seconds_until(now), 0);
    }

    #[test]
    fn rejects_unrelated_or_malformed_limit_text() {
        // Similar usage notices and incomplete reset data are not session limits.
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();

        assert!(
            parse_session_limit("You have 4 usage limit resets available", reference).is_none()
        );
        assert!(parse_session_limit(
            "You've hit your session limit · resets soon (Asia/Nicosia)",
            reference
        )
        .is_none());
        assert!(parse_session_limit(
            "You've hit your session limit · resets 1:10am (Not/AZone)",
            reference
        )
        .is_none());
    }
}
