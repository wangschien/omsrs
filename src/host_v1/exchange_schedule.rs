//! Supplied exchange-calendar observations evaluated without clock acquisition.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::America::New_York;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeScheduleResponse {
    pub schedule: ExchangeSchedule,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeSchedule {
    pub standard_hours: Vec<WeeklyHours>,
    pub maintenance_windows: Vec<MaintenanceWindow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeeklyHours {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub monday: Vec<DailyHours>,
    pub tuesday: Vec<DailyHours>,
    pub wednesday: Vec<DailyHours>,
    pub thursday: Vec<DailyHours>,
    pub friday: Vec<DailyHours>,
    pub saturday: Vec<DailyHours>,
    pub sunday: Vec<DailyHours>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DailyHours {
    pub open_time: String,
    pub close_time: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MaintenanceWindow {
    pub start_datetime: DateTime<Utc>,
    pub end_datetime: DateTime<Utc>,
}

fn hhmm_minutes(s: &str) -> Result<u16> {
    let bytes = s.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || !bytes[0..2].iter().all(u8::is_ascii_digit)
        || !bytes[3..5].iter().all(u8::is_ascii_digit)
    {
        return Err(anyhow!("invalid exchange schedule clock"));
    }
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid exchange schedule clock"))?;
    let h: u16 = h
        .parse()
        .map_err(|_| anyhow!("invalid exchange schedule hour"))?;
    let m: u16 = m
        .parse()
        .map_err(|_| anyhow!("invalid exchange schedule minute"))?;
    if h > 24 || m >= 60 || (h == 24 && m != 0) {
        return Err(anyhow!("exchange schedule clock outside 00:00–24:00"));
    }
    Ok(h * 60 + m)
}

fn utc_from_ns(ns: u64) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp((ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as u32)
        .ok_or_else(|| anyhow!("exchange schedule instant outside UTC range"))
}

impl ExchangeSchedule {
    /// First known closed instant in this market window. Maintenance and
    /// effective-date changes retain their exact timestamps; daily session
    /// boundaries are minute-granular in the official schedule.
    pub fn first_closed_ns(&self, start_ns: u64, end_ns: u64) -> Result<Option<u64>> {
        if start_ns >= end_ns || end_ns - start_ns > 3_600_000_000_000 {
            return Err(anyhow!("invalid exchange schedule window"));
        }
        let mut candidates = vec![start_ns];
        let mut minute = (start_ns / 60_000_000_000 + 1)
            .checked_mul(60_000_000_000)
            .ok_or_else(|| anyhow!("schedule minute overflow"))?;
        while minute < end_ns {
            candidates.push(minute);
            minute = minute
                .checked_add(60_000_000_000)
                .ok_or_else(|| anyhow!("schedule minute overflow"))?;
        }
        let mut add_exact = |at: DateTime<Utc>| {
            if let Some(ns) = at.timestamp_nanos_opt().and_then(|n| u64::try_from(n).ok()) {
                if start_ns < ns && ns < end_ns {
                    candidates.push(ns);
                }
            }
        };
        for w in &self.maintenance_windows {
            add_exact(w.start_datetime);
            add_exact(w.end_datetime);
        }
        for w in &self.standard_hours {
            add_exact(w.start_time);
            add_exact(w.end_time);
        }
        candidates.sort_unstable();
        candidates.dedup();
        for at in candidates {
            if !self.trading_open_ns(at)? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }

    pub fn trading_open_ns(&self, at_ns: u64) -> Result<bool> {
        self.trading_open_at(utc_from_ns(at_ns)?)
    }

    /// Official hours are ET local clocks.  Convert the queried UTC instant
    /// into New York time, so the calendar supplies EST/EDT and the repeated
    /// DST hour without inventing a fixed offset.
    pub fn trading_open_at(&self, at: DateTime<Utc>) -> Result<bool> {
        if self
            .maintenance_windows
            .iter()
            .any(|w| w.start_datetime <= at && at < w.end_datetime)
        {
            return Ok(false);
        }
        let local = at.with_timezone(&New_York);
        let weekly = self
            .standard_hours
            .iter()
            .filter(|w| w.start_time <= at && at < w.end_time)
            .max_by_key(|w| w.start_time)
            .ok_or_else(|| anyhow!("no effective exchange weekly schedule"))?;
        let sessions = match local.weekday().num_days_from_monday() {
            0 => &weekly.monday,
            1 => &weekly.tuesday,
            2 => &weekly.wednesday,
            3 => &weekly.thursday,
            4 => &weekly.friday,
            5 => &weekly.saturday,
            _ => &weekly.sunday,
        };
        let minute = (local.hour() * 60 + local.minute()) as u16;
        let mut open_now = false;
        for session in sessions {
            let open = hhmm_minutes(&session.open_time)?;
            // The wire schema encodes an end-of-day close as 00:00. The same clock
            // is midnight at the start of the day when used as an open.
            let close = if session.close_time == "00:00" {
                24 * 60
            } else {
                hhmm_minutes(&session.close_time)?
            };
            if open >= close {
                return Err(anyhow!(
                    "exchange schedule session is not half-open positive interval"
                ));
            }
            if open <= minute && minute < close {
                open_now = true;
            }
        }
        Ok(open_now)
    }

    /// A 15-minute market may start only if every instant until its end is
    /// inside a session.  Official sessions are minute-granular; explicit
    /// maintenance/effective-date boundaries are checked separately.
    pub fn window_open(&self, start_ns: u64, end_ns: u64) -> Result<bool> {
        if start_ns >= end_ns || end_ns - start_ns > 3_600_000_000_000 {
            return Err(anyhow!("invalid exchange schedule evaluation window"));
        }
        let start = utc_from_ns(start_ns)?;
        let end = utc_from_ns(end_ns)?;
        if self
            .maintenance_windows
            .iter()
            .any(|w| w.start_datetime < end && start < w.end_datetime)
        {
            return Ok(false);
        }
        if self.standard_hours.iter().any(|w| {
            (start < w.start_time && w.start_time < end) || (start < w.end_time && w.end_time < end)
        }) {
            return Ok(false);
        }
        let mut cursor = start_ns;
        loop {
            if !self.trading_open_at(utc_from_ns(cursor)?)? {
                return Ok(false);
            }
            let next_minute = (cursor / 60_000_000_000 + 1)
                .checked_mul(60_000_000_000)
                .ok_or_else(|| anyhow!("schedule minute overflow"))?;
            if next_minute >= end_ns {
                break;
            }
            cursor = next_minute;
        }
        self.trading_open_at(utc_from_ns(end_ns - 1)?)
    }
}
