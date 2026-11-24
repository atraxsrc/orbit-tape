//! History of fired alerts, shown in the sidebar's alert list and kept in
//! `alert-history.json` next to the saved state.

use crate::chart::Basis;
use exchange::Ticker;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

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
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AlertHistory {
    /// Newest first.
    entries: VecDeque<FiredAlert>,
    /// How many of the newest entries the user has not seen yet.
    unread: usize,
}

impl AlertHistory {
    /// Reads the history file; a missing or broken file gives an empty
    /// history.
    pub fn load() -> Self {
        let path = crate::data_path(Some(ALERT_HISTORY_PATH));
        match std::fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<Self>(&json) {
                Ok(mut history) => {
                    history.enforce_capacity();
                    history
                }
                Err(error) => {
                    log::warn!("ignoring unreadable {}: {error}", path.display());
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                log::warn!("could not read {}: {error}", path.display());
                Self::default()
            }
        }
    }

    /// Writes the history file on a worker thread.
    pub fn save(&self) {
        let json = match serde_json::to_string(self) {
            Ok(json) => json,
            Err(error) => {
                log::error!("could not serialize alert history: {error}");
                return;
            }
        };
        let spawned = std::thread::Builder::new()
            .name("alert-history-save".to_owned())
            .spawn(move || {
                if let Err(error) = crate::write_json_to_file(&json, ALERT_HISTORY_PATH) {
                    log::error!("could not save alert history: {error}");
                }
            });
        if let Err(error) = spawned {
            log::error!("could not start alert history save: {error}");
        }
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
}
