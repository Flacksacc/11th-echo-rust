use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    pub enabled: bool,
    pub offsets: Vec<i64>,
    pub repeat_minutes: i64,
    pub repeat_enabled: bool,
    pub snooze_minutes: i64,
    pub all_day_minute: u32,
    pub quiet: bool,
    pub quiet_start: u32,
    pub quiet_end: u32,
    pub timezone: String,
    pub voice: String,
    pub spoken: bool,
}
impl Default for Rule {
    fn default() -> Self {
        Self {
            enabled: true,
            offsets: vec![60, 15, 5, 0],
            repeat_minutes: 5,
            repeat_enabled: true,
            snooze_minutes: 10,
            all_day_minute: 540,
            quiet: false,
            quiet_start: 1320,
            quiet_end: 480,
            timezone: "local".into(),
            voice: crate::voices::FOLLOW_DEFAULT.into(),
            spoken: true,
        }
    }
}
impl Rule {
    pub fn validate(&mut self) -> Result<(), String> {
        if self.offsets.is_empty()
            || self.offsets.len() > 16
            || self.offsets.iter().any(|v| !(0..=43200).contains(v))
            || !(1..=10080).contains(&self.repeat_minutes)
            || !(1..=10080).contains(&self.snooze_minutes)
            || self.all_day_minute >= 1440
            || self.quiet_start >= 1440
            || self.quiet_end >= 1440
        {
            return Err("Use 1–16 reminder offsets (0–43200 minutes, up to 30 days), repetition/snooze of 1–10080 minutes, and valid clock times.".into());
        }
        if self.timezone != "local" && self.timezone.parse::<chrono_tz::Tz>().is_err() {
            return Err("Choose local or an IANA time zone, such as America/New_York.".into());
        }
        if self.voice.len() > 4096 {
            return Err("Voice selection is too long.".into());
        }
        self.offsets.sort_unstable_by(|a, b| b.cmp(a));
        self.offsets.dedup();
        Ok(())
    }
    pub fn minute(&self, now: DateTime<Utc>) -> u32 {
        if let Ok(zone) = self.timezone.parse::<chrono_tz::Tz>() {
            let t = now.with_timezone(&zone);
            t.hour() * 60 + t.minute()
        } else {
            let t = now.with_timezone(&Local);
            t.hour() * 60 + t.minute()
        }
    }
    pub fn quiet_now(&self, now: DateTime<Utc>) -> bool {
        if !self.quiet {
            return false;
        }
        let m = self.minute(now);
        if self.quiet_start == self.quiet_end {
            true
        } else if self.quiet_start < self.quiet_end {
            (self.quiet_start..self.quiet_end).contains(&m)
        } else {
            m >= self.quiet_start || m < self.quiet_end
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Occurrence {
    pub key: String,
    pub provider_id: String,
    pub account: String,
    pub calendar: String,
    pub series: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: Option<String>,
    pub cancelled: bool,
    pub link: String,
}
impl Occurrence {
    pub fn trigger(&self, rule: &Rule) -> i64 {
        let Some(date) = self
            .all_day
            .as_ref()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        else {
            return self.start;
        };
        let time = date
            .and_hms_opt(rule.all_day_minute / 60, rule.all_day_minute % 60, 0)
            .unwrap();
        // Spring-forward gaps advance to the first valid local minute; an
        // ambiguous fall-back time uses the first occurrence, ledger-deduped.
        for minutes in 0..180 {
            let time = time + chrono::Duration::minutes(minutes);
            let value = if let Ok(zone) = rule.timezone.parse::<chrono_tz::Tz>() {
                zone.from_local_datetime(&time)
                    .earliest()
                    .map(|d| d.timestamp())
            } else {
                Local
                    .from_local_datetime(&time)
                    .earliest()
                    .map(|d| d.timestamp())
            };
            if let Some(value) = value {
                return value;
            }
        }
        self.start
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub activated: bool,
    pub dismissed: bool,
    pub snooze_until: i64,
    pub last_ticket: String,
    pub spoken_ticket: String,
    pub last_text: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Notice {
    pub occurrence: Occurrence,
    pub ticket: String,
    pub text: String,
    pub rule: Rule,
}

pub fn due(event: &Occurrence, state: &Progress, rule: &Rule, now: i64) -> Option<Notice> {
    if !rule.enabled || event.cancelled || state.dismissed || state.snooze_until > now {
        return None;
    }
    let start = event.trigger(rule);
    let offsets = if event.all_day.is_some() {
        vec![0]
    } else {
        rule.offsets.clone()
    };
    let offset = offsets
        .into_iter()
        .filter(|offset| now >= start - offset * 60)
        .min()?;
    let overdue = now >= start;
    let slot = if overdue && rule.repeat_enabled {
        (now - start) / (rule.repeat_minutes * 60)
    } else {
        0
    };
    let ticket = format!("{start}/{offset}/{slot}");
    let snooze_expired = state.snooze_until > 0 && state.snooze_until <= now;
    if ticket
        == state
            .last_ticket
            .split("/snooze:")
            .next()
            .unwrap_or_default()
        && !snooze_expired
    {
        return None;
    }
    let end = if event.all_day.is_some() {
        start + (1440 - rule.all_day_minute as i64) * 60
    } else {
        event.end
    };
    if !state.activated && end < now {
        return None;
    }
    let title = crate::assistant_text::fragment(&event.title, 512).0;
    let text = if event.all_day.is_some() && now < end {
        format!("Reminder: {title} is today.")
    } else if event.all_day.is_some() {
        format!("Reminder: {title} still needs your acknowledgment.")
    } else if overdue {
        format!("Reminder: {title} has started.")
    } else {
        let minutes = ((start - now) + 59) / 60;
        format!("Reminder: {title} starts in {minutes} minutes.")
    };
    // Snoozing within the same milestone still yields a distinct delivery.
    let ticket = if snooze_expired {
        format!("{ticket}/snooze:{}", state.snooze_until)
    } else {
        ticket
    };
    Some(Notice {
        occurrence: event.clone(),
        ticket,
        text,
        rule: rule.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Occurrence {
        Occurrence {
            key: "event".into(),
            provider_id: "one".into(),
            account: "a".into(),
            calendar: "c".into(),
            series: "s".into(),
            title: "Project review".into(),
            start: 100000,
            end: 103600,
            all_day: None,
            cancelled: false,
            link: String::new(),
        }
    }
    #[test]
    fn milestones_repeats_dismissal_and_missed_phase_collapse() {
        let e = event();
        let r = Rule::default();
        let mut p = Progress::default();
        assert!(due(&e, &p, &r, e.start - 3601).is_none());
        for seconds in [-3600, -900, -300, 0, 300, 7200] {
            let n = due(&e, &p, &r, e.start + seconds).unwrap();
            p.last_ticket = n.ticket;
            p.activated = true;
            assert!(due(&e, &p, &r, e.start + seconds + 1).is_none());
        }
        p.dismissed = true;
        assert!(due(&e, &p, &r, e.start + 9000).is_none());
        let n = due(&e, &Progress::default(), &r, e.start - 600).unwrap();
        assert!(n.text.contains("10 minutes"));
    }
    #[test]
    fn snooze_and_historical_import() {
        let e = event();
        let r = Rule::default();
        let mut p = Progress {
            activated: true,
            snooze_until: e.start + 600,
            ..Default::default()
        };
        assert!(due(&e, &p, &r, e.start + 599).is_none());
        assert!(due(&e, &p, &r, e.start + 600).is_some());
        p.activated = false;
        p.snooze_until = 0;
        assert!(due(&e, &p, &r, e.end + 1).is_none());
    }
    #[test]
    fn quiet_hours_and_all_day_time() {
        let mut r = Rule {
            timezone: "America/New_York".into(),
            quiet: true,
            ..Default::default()
        };
        r.validate().unwrap();
        assert!(r.quiet_now(Utc.with_ymd_and_hms(2026, 10, 13, 3, 0, 0).unwrap()));
        assert!(!r.quiet_now(Utc.with_ymd_and_hms(2026, 10, 13, 16, 0, 0).unwrap()));
        let mut e = event();
        e.all_day = Some("2026-11-01".into());
        assert_eq!(
            e.trigger(&r),
            Utc.with_ymd_and_hms(2026, 11, 1, 14, 0, 0)
                .unwrap()
                .timestamp()
        );
    }
}
