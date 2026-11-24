//! History of fired alerts, shown in the sidebar's alert list and kept in
//! `alert-history.json` next to the saved state.
//!
//! The app and `orbit-tape watch` both add to it, under the file lock of
//! [`shared_file`]. It doubles as the record of what was delivered, so a
//! restart or a second program does not deliver the same bar again.

use super::shared_file;
use crate::chart::Basis;
use exchange::Ticker;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};

pub const ALERT_HISTORY_PATH: &str = "alert-history.json";
/// Oldest entries are dropped beyond this.
pub const CAPACITY: usize = 500;

/// One delivered alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiredAlert {
    /// When the alert was delivered, Unix milliseconds.
    pub fired_at: u64,
    /// The chart's ticker, used to find the chart again.
    pub ticker: Ticker,
    pub basis: Basis,
    /// Display symbol, for example "BTCUSDT".
    pub symbol: String,
    /// Exchange name, for example "BinanceLinear".
    pub exchange: String,
    /// Indicator name, or "Price alert".
    pub source: String,
    /// The condition that fired.
    pub condition: String,
    /// Bar close (indicator alerts) or live price (price alerts), formatted
    /// with the ticker's tick size.
    pub price: String,
    /// Open time of the bar the alert fired on, Unix milliseconds. Missing in
    /// entries saved by older versions.
    #[serde(default)]
    pub bar_time: Option<u64>,
    /// The condition's key (`indicator-id/plot-id`, `price-level/<id>`), so
    /// a restart does not deliver the same bar again. Missing in entries
    /// saved by older versions.
    #[serde(default)]
    pub condition_key: Option<String>,
    /// The bar closed while the app was not running; delivered at launch.
    #[serde(default)]
    pub missed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AlertHistory {
    /// Newest first.
    entries: VecDeque<FiredAlert>,
    /// How many of the newest entries the user has not seen yet.
    unread: usize,
}

impl AlertHistory {
    pub fn path() -> PathBuf {
        crate::data_path(Some(ALERT_HISTORY_PATH))
    }

    /// Reads the history file; a missing or broken file gives an empty
    /// history.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        match shared_file::read::<Self>(path) {
            Ok(mut history) => {
                history.enforce_capacity();
                history
            }
            Err(error) => {
                log::warn!("ignoring unreadable alert history: {error}");
                Self::default()
            }
        }
    }

    /// Saves the alerts that were not delivered before, by this or another
    /// program, and returns them with the saved history. Deliver only what
    /// this returns: two programs that see the same signal both ask, and
    /// only the first one gets it.
    pub fn claim(alerts: Vec<FiredAlert>) -> io::Result<(Vec<FiredAlert>, Self)> {
        Self::claim_in(&Self::path(), alerts)
    }

    pub fn claim_in(path: &Path, alerts: Vec<FiredAlert>) -> io::Result<(Vec<FiredAlert>, Self)> {
        shared_file::update(path, |history: &mut Self| {
            let mut claimed = Vec::new();
            for alert in alerts {
                if history.was_delivered(&alert) {
                    continue;
                }
                history.push(alert.clone());
                claimed.push(alert);
            }
            (claimed, history.clone())
        })
    }

    /// Marks the saved history as read and returns it.
    pub fn mark_read_saved() -> io::Result<Self> {
        shared_file::update(&Self::path(), |history: &mut Self| {
            history.mark_read();
            history.clone()
        })
    }

    /// Empties the saved history.
    pub fn clear_saved() -> io::Result<Self> {
        shared_file::update(&Self::path(), |history: &mut Self| {
            history.clear();
            history.clone()
        })
    }

    /// Whether the same condition already fired on the same bar of the same
    /// chart. Entries saved by older versions have no key and never match.
    pub fn was_delivered(&self, alert: &FiredAlert) -> bool {
        let (Some(key), Some(bar_time)) = (&alert.condition_key, alert.bar_time) else {
            return false;
        };
        self.entries.iter().any(|entry| {
            entry.bar_time == Some(bar_time)
                && entry.condition_key.as_ref() == Some(key)
                && entry.ticker == alert.ticker
                && entry.basis == alert.basis
        })
    }

    pub fn push(&mut self, alert: FiredAlert) {
        self.entries.push_front(alert);
        self.unread += 1;
        self.enforce_capacity();
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = &FiredAlert> {
        self.entries.iter()
    }

    pub fn get(&self, index: usize) -> Option<&FiredAlert> {
        self.entries.get(index)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn unread(&self) -> usize {
        self.unread
    }

    /// Whether the entry at `index` (newest first) is unread.
    pub fn is_unread(&self, index: usize) -> bool {
        index < self.unread
    }

    /// Returns true when something changed.
    pub fn mark_read(&mut self) -> bool {
        std::mem::replace(&mut self.unread, 0) != 0
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.unread = 0;
    }

    fn enforce_capacity(&mut self) {
        self.entries.truncate(CAPACITY);
        self.unread = self.unread.min(self.entries.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::adapter::Exchange;

    fn alert(n: u64) -> FiredAlert {
        FiredAlert {
            fired_at: n,
            ticker: Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            basis: Basis::Time(exchange::Timeframe::H1),
            symbol: "BTCUSDT".to_owned(),
            exchange: "BinanceLinear".to_owned(),
            source: "Price alert".to_owned(),
            condition: "Crossed 100000".to_owned(),
            price: "100001.0".to_owned(),
            bar_time: Some(n),
            condition_key: Some("x.test/signal".to_owned()),
            missed: false,
        }
    }

    #[test]
    fn capped_newest_first_with_unread_count() {
        let mut history = AlertHistory::default();
        for n in 0..(CAPACITY as u64 + 20) {
            history.push(alert(n));
        }
        assert_eq!(history.len(), CAPACITY);
        assert_eq!(history.unread(), CAPACITY);
        assert_eq!(history.get(0).unwrap().fired_at, CAPACITY as u64 + 19);
        assert!(history.mark_read());
        assert!(!history.mark_read());
        history.push(alert(1_000));
        assert!(history.is_unread(0) && !history.is_unread(1));
    }

    #[test]
    fn round_trips_through_json() {
        let mut history = AlertHistory::default();
        history.push(alert(1));
        history.push(alert(2));
        let json = serde_json::to_string(&history).unwrap();
        assert_eq!(
            serde_json::from_str::<AlertHistory>(&json).unwrap(),
            history
        );
    }

    #[test]
    fn entries_from_older_versions_have_no_bar_time() {
        let mut saved = serde_json::to_value(alert(1)).unwrap();
        for field in ["bar_time", "condition_key", "missed"] {
            saved.as_object_mut().unwrap().remove(field);
        }
        let entry: FiredAlert = serde_json::from_value(saved).unwrap();
        assert_eq!(entry.bar_time, None);
        assert_eq!(entry.condition_key, None);
        assert!(!entry.missed);
    }

    #[test]
    fn a_claim_is_granted_once() {
        let dir = std::env::temp_dir().join(format!("orbit-tape-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(ALERT_HISTORY_PATH);
        let (claimed, history) = AlertHistory::claim_in(&path, vec![alert(1), alert(2)]).unwrap();
        assert_eq!(claimed.len(), 2);
        assert_eq!(history.unread(), 2);
        // Another program saw the same bar 2 and a new bar 3.
        let again = FiredAlert {
            fired_at: 99,
            ..alert(2)
        };
        let (claimed, history) = AlertHistory::claim_in(&path, vec![again, alert(3)]).unwrap();
        assert_eq!(claimed, vec![alert(3)]);
        assert_eq!(history.len(), 3);
        assert_eq!(AlertHistory::load_from(&path), history);
        // Old entries without a key are never treated as delivered.
        let old = FiredAlert {
            condition_key: None,
            ..alert(1)
        };
        assert!(!history.was_delivered(&old));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
