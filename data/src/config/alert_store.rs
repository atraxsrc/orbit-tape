//! Saved alerts, shared by the app, `orbit-tape watch` and `orbit-tape mcp`
//! in `alerts.json` next to the saved state.
//!
//! An alert watches one market on one timeframe: a price crossing a level
//! (live trades), or an indicator's alert condition on a closed bar. Every
//! change goes through [`shared_file::update`], so the three programs can
//! write it at the same time. Whichever program holds the alert engine lock
//! ([`ENGINE_LOCK_PATH`]) evaluates the alerts; the others only edit them.
//!
//! The file also holds the notification channels the alerts can use.

use super::shared_file;
use crate::chart::Basis;
use exchange::{Ticker, Timeframe};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const ALERTS_PATH: &str = "alerts.json";
/// Held by the program that evaluates the alerts.
pub const ENGINE_LOCK_PATH: &str = "alert-engine";
/// At most this many alerts can be enabled at once.
pub const MAX_ACTIVE: usize = 50;
/// Used when the ntfy server is left empty.
pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertStore {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub channels: Channels,
    #[serde(default)]
    pub alerts: Vec<StoredAlert>,
}

fn version() -> u32 {
    VERSION
}

impl Default for AlertStore {
    fn default() -> Self {
        Self {
            version: VERSION,
            channels: Channels::default(),
            alerts: Vec::new(),
        }
    }
}

/// Where alerts can be sent besides the desktop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    #[serde(default)]
    pub ntfy: NtfySettings,
}

/// ntfy push notifications (<https://ntfy.sh>): anyone who knows the topic
/// can read it, so it should be hard to guess.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NtfySettings {
    /// Empty = ntfy is not set up.
    #[serde(default)]
    pub topic: String,
    /// Empty = [`DEFAULT_NTFY_SERVER`].
    #[serde(default)]
    pub server: String,
}

impl NtfySettings {
    pub fn is_set(&self) -> bool {
        !self.topic.trim().is_empty()
    }

    /// The server, without a trailing slash.
    pub fn server(&self) -> String {
        match self.server.trim() {
            "" => DEFAULT_NTFY_SERVER,
            server => server,
        }
        .trim_end_matches('/')
        .to_owned()
    }

    /// The topic's web page, `None` when ntfy is not set up.
    pub fn url(&self) -> Option<String> {
        self.is_set()
            .then(|| format!("{}/{}", self.server(), self.topic.trim()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredAlert {
    /// Short and unique within the file, e.g. `a3f9c2e1`.
    pub id: String,
    /// Exchange and instrument, e.g. `OKXLinear:BTC-USDT-SWAP`.
    pub ticker: Ticker,
    /// Indicator alerts run on this timeframe's bars; `every_bar` price
    /// alerts fire at most once per bar of it.
    pub timeframe: Timeframe,
    #[serde(flatten)]
    pub kind: AlertKind,
    /// Extra text for the notification.
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub frequency: Frequency,
    #[serde(default)]
    pub channel: Channel,
    pub enabled: bool,
    pub created_by: Creator,
    /// Unix milliseconds.
    pub created_at: u64,
    /// When it last fired, Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<u64>,
    /// Why the engine could not check it, e.g. a missing indicator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertKind {
    /// Live price crossing a level.
    Price { cross: PriceCross, level: f64 },
    /// An indicator's alert plot true on a closed bar.
    Indicator {
        /// Indicator ID, e.g. `roman.roman-setup`.
        indicator: String,
        /// Alert plot ID; `None` = any of the indicator's alert plots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plot: Option<String>,
        /// Inputs that differ from the indicator's defaults.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        inputs: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceCross {
    Above,
    Below,
    /// Either direction, as the chart's alert lines always worked.
    Cross,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frequency {
    /// Fire, then turn off.
    #[default]
    Once,
    /// At most once per bar of the alert's timeframe.
    EveryBar,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Desktop,
    Ntfy,
    Both,
}

impl Channel {
    pub fn desktop(self) -> bool {
        matches!(self, Self::Desktop | Self::Both)
    }

    pub fn ntfy(self) -> bool {
        matches!(self, Self::Ntfy | Self::Both)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Creator {
    App,
    Claude,
}

impl AlertStore {
    pub fn path() -> PathBuf {
        crate::data_path(Some(ALERTS_PATH))
    }

    pub fn engine_lock_path() -> PathBuf {
        crate::data_path(Some(ENGINE_LOCK_PATH))
    }

    /// Reads the saved alerts; a broken file gives no alerts (and is kept
    /// aside by the next change).
    pub fn load() -> Self {
        let path = Self::path();
        shared_file::read(&path).unwrap_or_else(|error| {
            log::warn!("could not read {}: {error}", path.display());
            Self::default()
        })
    }

    /// Changes the saved alerts and returns the result of `change`.
    pub fn update<R>(change: impl FnOnce(&mut Self) -> R) -> std::io::Result<R> {
        Self::update_in(&Self::path(), change)
    }

    pub fn update_in<R>(
        path: &std::path::Path,
        change: impl FnOnce(&mut Self) -> R,
    ) -> std::io::Result<R> {
        shared_file::update(path, change)
    }

    pub fn get(&self, id: &str) -> Option<&StoredAlert> {
        self.alerts.iter().find(|alert| alert.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut StoredAlert> {
        self.alerts.iter_mut().find(|alert| alert.id == id)
    }

    pub fn active_count(&self) -> usize {
        self.alerts.iter().filter(|alert| alert.enabled).count()
    }

    /// An ID no alert in the file uses.
    pub fn new_id(&self) -> String {
        loop {
            let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
            if self.get(&id).is_none() {
                return id;
            }
        }
    }

    /// Adds an alert, keeping at most [`MAX_ACTIVE`] enabled.
    pub fn add(&mut self, alert: StoredAlert) -> Result<(), String> {
        if alert.enabled && self.active_count() >= MAX_ACTIVE {
            return Err(too_many());
        }
        if self.get(&alert.id).is_some() {
            return Err(format!("an alert with ID {} already exists", alert.id));
        }
        self.alerts.push(alert);
        Ok(())
    }

    /// Turns an alert on or off, keeping at most [`MAX_ACTIVE`] enabled.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let active = self.active_count();
        let alert = self
            .get_mut(id)
            .ok_or_else(|| format!("no alert with ID {id}"))?;
        if enabled && !alert.enabled && active >= MAX_ACTIVE {
            return Err(too_many());
        }
        alert.enabled = enabled;
        if enabled {
            alert.error = None;
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Option<StoredAlert> {
        let index = self.alerts.iter().position(|alert| alert.id == id)?;
        Some(self.alerts.remove(index))
    }
}

fn too_many() -> String {
    format!("at most {MAX_ACTIVE} alerts can be active; pause or delete one first")
}

impl StoredAlert {
    /// The key alert history entries of this alert carry.
    pub fn condition_key(&self) -> String {
        format!("alert/{}", self.id)
    }

    pub fn basis(&self) -> Basis {
        Basis::Time(self.timeframe)
    }

    /// A `once` alert that fired and turned itself off.
    pub fn is_done(&self) -> bool {
        self.frequency == Frequency::Once && !self.enabled && self.fired_at.is_some()
    }

    /// The display symbol, e.g. `BTCUSDT` or `BTC-USDT-SWAP`.
    pub fn symbol(&self) -> String {
        self.ticker.display_symbol_and_type().0
    }

    /// The exchange's display name, e.g. `OKX`.
    pub fn exchange_name(&self) -> String {
        self.ticker.exchange.venue().to_string()
    }

    /// What it waits for, e.g. `price crosses above 62000`.
    pub fn condition(&self) -> String {
        match &self.kind {
            AlertKind::Price { cross, level } => {
                let level = trim_float(*level);
                match cross {
                    PriceCross::Above => format!("price crosses above {level}"),
                    PriceCross::Below => format!("price crosses below {level}"),
                    PriceCross::Cross => format!("price crosses {level}"),
                }
            }
            AlertKind::Indicator {
                indicator,
                plot,
                inputs,
            } => {
                let mut text = match plot {
                    Some(plot) => format!("{indicator} {plot}"),
                    None => format!("any {indicator} alert"),
                };
                if !inputs.is_empty() {
                    let inputs = inputs
                        .iter()
                        .map(|(name, value)| format!("{name}={value}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    text.push_str(&format!(" ({inputs})"));
                }
                text
            }
        }
    }

    /// One line, e.g. `BTC-USDT-SWAP OKX 1h: price crosses above 62000 (once,
    /// desktop)`.
    pub fn summary(&self) -> String {
        let frequency = match self.frequency {
            Frequency::Once => "once",
            Frequency::EveryBar => "every bar",
        };
        let channel = match self.channel {
            Channel::Desktop => "desktop",
            Channel::Ntfy => "ntfy",
            Channel::Both => "desktop + ntfy",
        };
        let state = if self.is_done() {
            ", done"
        } else if !self.enabled {
            ", paused"
        } else {
            ""
        };
        format!(
            "{} {} {}: {} ({frequency}, {channel}{state})",
            self.symbol(),
            self.exchange_name(),
            self.timeframe,
            self.condition()
        )
    }
}

/// `62000` instead of `62000.0`, and no float noise.
pub fn trim_float(value: f64) -> String {
    let text = format!("{value:.10}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::adapter::Exchange;

    pub fn price_alert(id: &str) -> StoredAlert {
        StoredAlert {
            id: id.to_owned(),
            ticker: Ticker::new("BTC-USDT-SWAP", Exchange::OkexLinear),
            timeframe: Timeframe::H1,
            kind: AlertKind::Price {
                cross: PriceCross::Above,
                level: 62_000.0,
            },
            message: String::new(),
            frequency: Frequency::Once,
            channel: Channel::Desktop,
            enabled: true,
            created_by: Creator::Claude,
            created_at: 1,
            fired_at: None,
            error: None,
        }
    }

    #[test]
    fn json_is_readable_and_round_trips() {
        let mut store = AlertStore::default();
        store.add(price_alert("a1")).unwrap();
        store
            .add(StoredAlert {
                id: "b2".to_owned(),
                kind: AlertKind::Indicator {
                    indicator: "roman.roman-setup".to_owned(),
                    plot: None,
                    inputs: BTreeMap::from([("ctMode".to_owned(), "true".to_owned())]),
                },
                frequency: Frequency::EveryBar,
                channel: Channel::Both,
                ..price_alert("b2")
            })
            .unwrap();
        let json = serde_json::to_value(&store).unwrap();
        let first = &json["alerts"][0];
        assert_eq!(first["type"], "price");
        assert_eq!(first["cross"], "above");
        assert_eq!(first["ticker"], "OKXLinear:BTC-USDT-SWAP");
        assert_eq!(json["alerts"][1]["inputs"]["ctMode"], "true");
        assert_eq!(serde_json::from_value::<AlertStore>(json).unwrap(), store);
    }

    #[test]
    fn at_most_fifty_active() {
        let mut store = AlertStore::default();
        for n in 0..MAX_ACTIVE {
            store.add(price_alert(&format!("a{n}"))).unwrap();
        }
        assert!(store.add(price_alert("one-more")).is_err());
        // Paused alerts do not count.
        store
            .add(StoredAlert {
                enabled: false,
                ..price_alert("paused")
            })
            .unwrap();
        assert!(store.set_enabled("paused", true).is_err());
        store.set_enabled("a0", false).unwrap();
        store.set_enabled("paused", true).unwrap();
        assert_eq!(store.active_count(), MAX_ACTIVE);
    }

    #[test]
    fn summaries() {
        assert_eq!(
            price_alert("a").summary(),
            "BTC-USDT-SWAP OKX 1h: price crosses above 62000 (once, desktop)"
        );
        let done = StoredAlert {
            enabled: false,
            fired_at: Some(5),
            ..price_alert("a")
        };
        assert!(done.summary().ends_with("(once, desktop, done)"));
        assert_eq!(trim_float(0.000_123_4), "0.0001234");
        assert_eq!(trim_float(-3.5), "-3.5");
    }

    #[test]
    fn ntfy_url() {
        let mut ntfy = NtfySettings::default();
        assert_eq!(ntfy.url(), None);
        ntfy.topic = " my-topic ".to_owned();
        assert_eq!(ntfy.url().unwrap(), "https://ntfy.sh/my-topic");
        ntfy.server = "https://ntfy.example.org/".to_owned();
        assert_eq!(ntfy.url().unwrap(), "https://ntfy.example.org/my-topic");
    }
}
