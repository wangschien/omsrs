//! Serialized daily risk accounting values; persistence remains at the caller.

use anyhow::{Context, Result, anyhow, ensure};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

pub const STATE_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DailyLossStatus {
    Clear { day_pnl: Decimal },
    Tripped { day_pnl: Option<Decimal> },
    Unprovable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskState {
    pub version: u8,
    pub et_day: String,
    pub closed_pnl_usd: String,
    pub closed_windows: u64,
    pub pnl_proven: bool,
    #[serde(default)]
    pub tripped: bool,
    #[serde(default)]
    pub trip_day: Option<String>,
    #[serde(default)]
    pub trip_pnl_usd: Option<String>,
    /// The limit in force at the first trip; release cannot raise it later.
    #[serde(default)]
    pub trip_limit_usd: Option<String>,
    #[serde(default)]
    pub closed_window_ids: Vec<String>,
    #[serde(default)]
    pub live_window_mtm_usd: BTreeMap<String, String>,
    #[serde(default)]
    pub live_window_unproven: BTreeSet<String>,
    /// Closed windows to re-prove when a trip survives an ET rollover.
    #[serde(default)]
    pub release_trip_day: Option<String>,
    #[serde(default)]
    pub release_window_ids: Vec<String>,
}

impl DiskState {
    pub fn fresh(day: NaiveDate) -> Self {
        Self {
            version: STATE_VERSION,
            et_day: day.to_string(),
            closed_pnl_usd: Decimal::ZERO.to_string(),
            closed_windows: 0,
            pnl_proven: true,
            tripped: false,
            trip_day: None,
            trip_pnl_usd: None,
            trip_limit_usd: None,
            closed_window_ids: Vec::new(),
            live_window_mtm_usd: BTreeMap::new(),
            live_window_unproven: BTreeSet::new(),
            release_trip_day: None,
            release_window_ids: Vec::new(),
        }
    }

    pub fn closed_pnl(&self) -> Result<Decimal> {
        Decimal::from_str(&self.closed_pnl_usd).context("parse closed-window P&L")
    }

    pub fn trip_pnl(&self) -> Option<Decimal> {
        self.trip_pnl_usd
            .as_deref()
            .and_then(|value| Decimal::from_str(value).ok())
    }
}

/// Pure process-local risk values. The caller owns paths, locks and persistence.
#[derive(Debug)]
pub struct DailyLossState {
    pub limit: Decimal,
    pub state: Option<DiskState>,
    pub tripped: bool,
    pub tripped_pnl: Option<Decimal>,
    pub unprovable: bool,
    pub must_hold_process: bool,
    pub shared_window_id: Option<String>,
}

impl DailyLossState {
    pub fn local_status(&self) -> DailyLossStatus {
        if self.unprovable {
            return DailyLossStatus::Unprovable;
        }
        let Some(state) = self.state.as_ref() else {
            return DailyLossStatus::Unprovable;
        };
        if !state.pnl_proven {
            return DailyLossStatus::Unprovable;
        }
        let Ok(day_pnl) = state.closed_pnl() else {
            return DailyLossStatus::Unprovable;
        };
        if self.tripped {
            DailyLossStatus::Tripped {
                day_pnl: self.tripped_pnl.or(Some(day_pnl)),
            }
        } else {
            DailyLossStatus::Clear { day_pnl }
        }
    }

    pub fn shared_total(state: &DiskState) -> Result<Decimal> {
        let mut total = state.closed_pnl()?;
        for mark in state.live_window_mtm_usd.values() {
            let value = Decimal::from_str(mark).context("parse shared live-window MTM")?;
            ensure!(
                value <= Decimal::ZERO,
                "shared live-window MTM must be nonpositive"
            );
            total = total
                .checked_add(value)
                .context("shared daily P&L overflow")?;
        }
        Ok(total)
    }

    pub fn close_window_value(&mut self, window_pnl: Option<Decimal>) -> Option<DiskState> {
        let (snapshot, state_error) = match self.state.as_mut() {
            Some(state) => {
                state.closed_windows = state.closed_windows.saturating_add(1);
                let state_error = match window_pnl {
                    Some(pnl) if state.pnl_proven => match state.closed_pnl() {
                        Ok(closed) => match closed.checked_add(pnl) {
                            Some(total) => {
                                state.closed_pnl_usd = total.to_string();
                                false
                            }
                            None => {
                                state.pnl_proven = false;
                                true
                            }
                        },
                        Err(_) => {
                            state.pnl_proven = false;
                            true
                        }
                    },
                    Some(_) => false,
                    None => {
                        state.pnl_proven = false;
                        true
                    }
                };
                (Some(state.clone()), state_error)
            }
            None => (None, true),
        };
        if state_error {
            self.unprovable = true;
        }
        snapshot
    }
}

impl DailyLossStatus {
    pub fn admission_blocked(&self) -> bool {
        !matches!(self, DailyLossStatus::Clear { .. })
    }
    pub fn is_unprovable(&self) -> bool {
        matches!(self, DailyLossStatus::Unprovable)
    }
}

impl DailyLossState {
    pub fn prepare_roll_day(&mut self, today: NaiveDate) -> Result<Option<DiskState>> {
        let reset_state = match self.state.as_mut() {
            Some(state) if state.et_day != today.to_string() => {
                let mut fresh = DiskState::fresh(today);
                // ET rollover resets the daily amount, not a persisted trip or
                // an unprovable result; only manual release clears those latches.
                fresh.pnl_proven = state.pnl_proven;
                fresh.tripped = state.tripped;
                fresh.trip_day = state.trip_day.clone();
                fresh.trip_pnl_usd = state.trip_pnl_usd.clone();
                fresh.trip_limit_usd = state.trip_limit_usd.clone();
                fresh.release_trip_day = state
                    .release_trip_day
                    .clone()
                    .or_else(|| state.tripped.then(|| state.et_day.clone()));
                fresh.release_window_ids = if state.release_window_ids.is_empty() && state.tripped {
                    state.closed_window_ids.clone()
                } else {
                    state.release_window_ids.clone()
                };
                *state = fresh;
                Some(state.clone())
            }
            Some(_) => None,
            None => return Err(anyhow!("daily loss state unavailable")),
        };
        Ok(reset_state)
    }
    pub fn finish_roll_day_persisted(&mut self) {
        if self.tripped_pnl.is_none() && self.tripped {
            self.tripped_pnl = Some(Decimal::ZERO);
        }
    }
    pub fn begin_trip(&mut self, day_pnl: Option<Decimal>) -> bool {
        let first_trip = !self.tripped;
        self.tripped = true;
        if first_trip {
            self.tripped_pnl = day_pnl;
        }
        first_trip
    }
    pub fn prepare_trip_snapshot(
        &mut self,
        first_trip: bool,
        day_pnl: Option<Decimal>,
        trip_day: NaiveDate,
    ) -> Result<DiskState> {
        let state_result = match self.state.as_mut() {
            Some(state) => {
                state.tripped = true;
                if first_trip {
                    state.trip_day = Some(trip_day.to_string());
                    state.trip_pnl_usd = day_pnl.map(|value| value.to_string());
                    state.trip_limit_usd = Some(self.limit.to_string());
                    state.release_trip_day = Some(trip_day.to_string());
                    state.release_window_ids = state.closed_window_ids.clone();
                }
                if day_pnl.is_none() {
                    state.pnl_proven = false;
                }
                let snapshot = state.clone();
                Ok(snapshot)
            }
            None => Err(anyhow!("daily loss state unavailable while latching trip")),
        };
        state_result
    }
}

impl DiskState {
    pub fn validate_loaded(&self) -> Result<()> {
        ensure!(
            self.version == STATE_VERSION,
            "unsupported daily loss state version"
        );
        ensure!(
            NaiveDate::parse_from_str(&self.et_day, "%Y-%m-%d").is_ok(),
            "invalid ET day in daily loss state"
        );
        self.closed_pnl()?;
        Ok(())
    }
    pub fn into_day(self, today: NaiveDate) -> (Self, bool) {
        let mut state = self;
        if state.et_day != today.to_string() {
            let mut fresh = DiskState::fresh(today);
            fresh.pnl_proven = state.pnl_proven;
            fresh.tripped = state.tripped;
            fresh.trip_day = state.trip_day;
            fresh.trip_pnl_usd = state.trip_pnl_usd;
            fresh.trip_limit_usd = state.trip_limit_usd;
            fresh.release_trip_day = state
                .release_trip_day
                .or_else(|| state.tripped.then(|| state.et_day.clone()));
            fresh.release_window_ids = if state.release_window_ids.is_empty() && state.tripped {
                state.closed_window_ids.clone()
            } else {
                state.release_window_ids
            };
            // A still-running window owns these obligations across the ET
            // date boundary. Its final P&L is booked only when it closes.
            fresh.live_window_mtm_usd = state.live_window_mtm_usd;
            fresh.live_window_unproven = state.live_window_unproven;
            state = fresh;
            (state, true)
        } else {
            (state, false)
        }
    }
}

pub fn release_window_ids(
    state: &DiskState,
    series: &str,
    limit: Decimal,
    adopted: bool,
) -> Result<(String, Vec<String>)> {
    ensure!(
        state.version == STATE_VERSION && state.tripped,
        "daily-loss state is not a valid trip"
    );
    let trip_day = state.trip_day.as_deref().context("trip day absent")?;
    NaiveDate::parse_from_str(trip_day, "%Y-%m-%d").context("invalid trip day")?;
    NaiveDate::parse_from_str(&state.et_day, "%Y-%m-%d").context("invalid state ET day")?;
    state.closed_pnl()?;

    match state.trip_limit_usd.as_deref() {
        Some(trip_limit) => ensure!(
            Decimal::from_str(trip_limit).context("invalid trip limit")? == limit,
            "release config daily limit differs from limit at trip"
        ),
        None => ensure!(
            adopted,
            "trip limit absent; original limit cannot be proven"
        ),
    }
    ensure!(
        state.live_window_mtm_usd.is_empty() && state.live_window_unproven.is_empty(),
        "release refused: an in-flight window obligation remains"
    );
    ensure!(
        state.closed_windows == state.closed_window_ids.len() as u64,
        "daily-loss closed-window count does not match identities"
    );
    if let Some(day) = &state.release_trip_day {
        ensure!(
            day == trip_day,
            "release trip day conflicts with daily-loss trip day"
        );
    }
    let ids = if !state.release_window_ids.is_empty() {
        state.release_window_ids.clone()
    } else {
        ensure!(
            state.et_day == trip_day,
            "older trip lost its window identities across ET rollover"
        );
        state.closed_window_ids.clone()
    };

    ensure!(
        !ids.is_empty() || adopted,
        "release has no closed windows to re-prove"
    );
    let mut seen = BTreeSet::new();
    for id in &ids {
        ensure!(
            id.starts_with(&format!("{series}-"))
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && seen.insert(id),
            "release window identity is invalid or duplicated"
        );
    }
    if state.et_day == trip_day {
        ensure!(
            state.closed_window_ids.iter().all(|id| seen.contains(id))
                && state.closed_window_ids.len() == ids.len(),
            "release trip-day window set differs from state"
        );
    } else {
        ensure!(
            state.closed_window_ids.is_empty(),
            "release cannot combine prior-trip and current-day closes"
        );
    }
    Ok((trip_day.to_owned(), ids))
}

pub fn adopt_killed_open_windows(state: &mut DiskState) {
    if state.live_window_mtm_usd.is_empty() && state.live_window_unproven.is_empty() {
        return;
    }
    if state.trip_day.as_deref() != Some(state.et_day.as_str()) {
        return;
    }
    let open: BTreeSet<String> = state
        .live_window_mtm_usd
        .keys()
        .cloned()
        .chain(state.live_window_unproven.iter().cloned())
        .collect();
    for id in open {
        if !state.release_window_ids.is_empty() && !state.release_window_ids.contains(&id) {
            state.release_window_ids.push(id.clone());
        }
        if !state.closed_window_ids.contains(&id) {
            state.closed_window_ids.push(id);
        }
    }
    state.live_window_mtm_usd.clear();
    state.live_window_unproven.clear();
    state.closed_windows = state.closed_window_ids.len() as u64;
}

/// Result before the caller's existing persistence and trip effects.
pub enum LiveWindowValue {
    Unavailable,
    Total(Decimal),
    Overflow(Option<DiskState>),
}
impl DailyLossState {
    pub fn observe_live_value(&mut self, live_window_mtm: Decimal) -> LiveWindowValue {
        let Some(state) = self.state.as_ref() else {
            self.unprovable = true;
            return LiveWindowValue::Unavailable;
        };
        if !state.pnl_proven {
            return LiveWindowValue::Unavailable;
        }
        let Ok(day_pnl) = state.closed_pnl() else {
            self.unprovable = true;
            return LiveWindowValue::Unavailable;
        };
        let Some(total) = day_pnl.checked_add(live_window_mtm) else {
            let snapshot = self.state.as_mut().map(|state| {
                state.pnl_proven = false;
                state.clone()
            });
            return LiveWindowValue::Overflow(snapshot);
        };
        LiveWindowValue::Total(total)
    }
}
impl DiskState {
    pub fn shared_close_is_durable(&self, id: &String) -> bool {
        self.version == STATE_VERSION
            && self.pnl_proven
            && self.closed_window_ids.contains(id)
            && !self.live_window_mtm_usd.contains_key(id)
            && !self.live_window_unproven.contains(id)
    }
    pub fn observe_shared_value(
        &mut self,
        id: String,
        live_window_mtm: Decimal,
    ) -> Result<Option<(Decimal, Self)>> {
        if self.closed_window_ids.contains(&id) {
            return Ok(None);
        }
        // Positive unrealized P&L never offsets another runner's loss.
        self.live_window_unproven.remove(&id);
        self
            .live_window_mtm_usd
            .insert(id, live_window_mtm.min(Decimal::ZERO).to_string());
        let total = DailyLossState::shared_total(self)?;
        let snapshot = self.clone();
        Ok(Some((total, snapshot)))
    }
    pub fn mark_shared_unproven(&mut self, id: String) -> Option<Self> {
        if !self.closed_window_ids.contains(&id) {
            self.live_window_unproven.insert(id);
            let snapshot = self.clone();
            Some(snapshot)
        } else {
            None
        }
    }
    pub fn close_shared_value(&mut self, id: &String, window_pnl: Option<Decimal>) -> Result<Self> {
        self.live_window_mtm_usd.remove(id);
        self.live_window_unproven.remove(id);
        if !self.closed_window_ids.contains(id) {
            self.closed_window_ids.push(id.clone());
            self.closed_windows = self.closed_windows.saturating_add(1);
            if self.tripped && !self.release_window_ids.contains(id) {
                self.release_window_ids.push(id.clone());
            }
            match window_pnl {
                Some(pnl) if self.pnl_proven => {
                    self.closed_pnl_usd = self
                        .closed_pnl()?
                        .checked_add(pnl)
                        .context("shared closed P&L overflow")?
                        .to_string();
                }
                Some(_) => {}
                None => self.pnl_proven = false,
            }
        }
        Ok(self.clone())
    }
}
