//! Delivers indicator alerts: a desktop notification and a short chime.
//!
//! Chart panes report alert conditions that turned true on a closed bar
//! (`data::chart::kline::alerts`). This module drops duplicates (the same
//! chart can be open in several panes), skips muted conditions, and delivers
//! the rest the way the alert settings ask.

use data::chart::Basis;
use data::chart::kline::alerts::{AlertHit, AlertSource};
use data::config::alert_history::{AlertHistory, FiredAlert};
use data::config::alerts::Alerts;
use exchange::{Ticker, UnixMs};
use rodio::Source;
use std::collections::VecDeque;
use std::time::Duration;

/// Recently delivered alerts remembered for de-duplication.
const RECENT_CAPACITY: usize = 256;

/// Shown with alerts for bars that closed while the app was not running.
pub const MISSED_LABEL: &str = "missed while closed";

pub struct AlertCenter {
    recent: VecDeque<String>,
    /// When this run started: bars that closed before it were missed.
    launched_at: UnixMs,
    chime: Option<rodio::MixerDeviceSink>,
}

/// One delivered condition on one bar of one chart.
fn recent_key(ticker: &Ticker, basis: &Basis, condition_key: &str, bar_time: u64) -> String {
    format!("{ticker:?}|{basis}|{condition_key}|{bar_time}")
}

impl AlertCenter {
    /// Starts with the newest saved alerts as already delivered, so
    /// restarting before the next bar closes does not deliver one again.
    /// Entries saved by older versions (no bar time or key) are ignored.
    pub fn new(history: &AlertHistory, launched_at: UnixMs) -> Self {
        let mut recent = history
            .entries()
            .filter_map(|alert| {
                Some(recent_key(
                    &alert.ticker,
                    &alert.basis,
                    alert.condition_key.as_deref()?,
                    alert.bar_time?,
                ))
            })
            .take(RECENT_CAPACITY)
            .collect::<VecDeque<_>>();
        // History is newest first; `recent` drops from the front.
        recent.make_contiguous().reverse();
        Self {
            recent,
            launched_at,
            chime: None,
        }
    }

    /// Delivers new hits. Returns the hits that were delivered (for the
    /// alert history) and an error message for the user when the chime could
    /// not be played.
    pub fn deliver(
        &mut self,
        hits: Vec<AlertHit>,
        settings: &Alerts,
    ) -> (Vec<FiredAlert>, Option<String>) {
        if !settings.enabled {
            return (Vec::new(), None);
        }
        let mut fresh = Vec::new();
        for hit in hits {
            if settings.is_muted(&hit.condition_key()) {
                continue;
            }
            let key = recent_key(
                &hit.ticker_info.ticker,
                &hit.basis,
                &hit.condition_key(),
                hit.bar_time.as_u64(),
            );
            if self.recent.contains(&key) {
                continue;
            }
            if self.recent.len() == RECENT_CAPACITY {
                self.recent.pop_front();
            }
            self.recent.push_back(key);
            fresh.push(hit);
        }
        if fresh.is_empty() {
            return (Vec::new(), None);
        }

        let mut fired = Vec::with_capacity(fresh.len());
        for hit in &fresh {
            let missed = self.missed(hit);
            log::info!(
                "alert: {} {} {} ({}){}",
                symbol(hit),
                hit.basis,
                hit.title,
                hit.condition_key(),
                if missed { ", missed while closed" } else { "" }
            );
            if settings.notification {
                notify(summary(hit, missed), body(hit));
            }
            fired.push(fired_alert(hit, missed));
        }
        let error = if settings.sound {
            self.play_chime(settings.volume).err()
        } else {
            None
        };
        (fired, error)
    }

    /// Whether the hit's bar closed before this run started. Price alerts
    /// are live, never missed.
    fn missed(&self, hit: &AlertHit) -> bool {
        let (AlertSource::Indicator { .. }, Basis::Time(timeframe)) = (&hit.source, hit.basis)
        else {
            return false;
        };
        let closed_at = hit
            .bar_time
            .as_u64()
            .saturating_add(timeframe.to_milliseconds());
        closed_at < self.launched_at.as_u64()
    }

    /// Sends a sample notification and chime so the user can check both.
    pub fn test(&mut self, settings: &Alerts) -> Option<String> {
        if settings.notification {
            notify(
                "Orbit Tape test alert".to_owned(),
                "Alerts will look like this.".to_owned(),
            );
        }
        if settings.sound {
            return self.play_chime(settings.volume).err();
        }
        None
    }

    fn play_chime(&mut self, volume: f32) -> Result<(), String> {
        if volume <= 0.0 {
            return Ok(());
        }
        if self.chime.is_none() {
            let mut sink = rodio::DeviceSinkBuilder::open_default_sink()
                .map_err(|err| format!("Alert sound disabled: {err}"))?;
            sink.log_on_drop(false);
            self.chime = Some(sink);
        }
        if let Some(sink) = &self.chime {
            sink.mixer().add(chime(volume.clamp(0.0, 100.0) / 100.0));
        }
        Ok(())
    }
}

/// Two rising notes, about half a second long.
fn chime(gain: f32) -> impl Source + Send + 'static {
    let note = |freq: f32, start_ms: u64, len_ms: u64| {
        let len = Duration::from_millis(len_ms);
        rodio::source::SineWave::new(freq)
            .fade_out(len)
            .take_duration(len)
            .fade_in(Duration::from_millis(6))
            .delay(Duration::from_millis(start_ms))
    };
    note(784.0, 0, 260)
        .mix(note(1175.0, 140, 380))
        .amplify(0.35 * gain)
}

/// The history entry for a delivered hit.
fn fired_alert(hit: &AlertHit, missed: bool) -> FiredAlert {
    FiredAlert {
        fired_at: exchange::UnixMs::now().as_u64(),
        ticker: hit.ticker_info.ticker,
        basis: hit.basis,
        symbol: symbol(hit),
        exchange: exchange_name(hit),
        source: hit.source_name.clone(),
        condition: hit.title.clone(),
        price: hit.close.to_string(hit.ticker_info.min_ticksize),
        bar_time: Some(hit.bar_time.as_u64()),
        condition_key: Some(hit.condition_key()),
        missed,
    }
}

fn symbol(hit: &AlertHit) -> String {
    hit.ticker_info.ticker.display_symbol_and_type().0
}

fn summary(hit: &AlertHit, missed: bool) -> String {
    let summary = format!("{} {}: {}", symbol(hit), hit.basis, hit.title);
    if missed {
        format!("{summary} ({MISSED_LABEL})")
    } else {
        summary
    }
}

fn body(hit: &AlertHit) -> String {
    let close = hit.close.to_string(hit.ticker_info.min_ticksize);
    let bar = chrono::DateTime::from_timestamp_millis(hit.bar_time.as_u64() as i64)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%a %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    match hit.source {
        AlertSource::Indicator { .. } => format!(
            "{}\nClose {close}, bar opened {bar}\n{}",
            hit.source_name,
            exchange_name(hit)
        ),
        AlertSource::PriceLevel { .. } => format!(
            "{}\nLast {close} at {bar}\n{}",
            hit.source_name,
            exchange_name(hit)
        ),
    }
}

fn exchange_name(hit: &AlertHit) -> String {
    hit.ticker_info
        .ticker
        .symbol_and_exchange_string()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Shows a desktop notification without blocking the UI thread.
fn notify(summary: String, body: String) {
    let spawned = std::thread::Builder::new()
        .name("alert-notification".to_owned())
        .spawn(move || {
            let result = notify_rust::Notification::new()
                .appname("Orbit Tape")
                .summary(&summary)
                .body(&body)
                .icon("appointment-soon")
                .timeout(notify_rust::Timeout::Milliseconds(15_000))
                .show();
            if let Err(err) = result {
                log::warn!("alert notification failed: {err}");
            }
        });
    if let Err(err) = spawned {
        log::warn!("alert notification thread failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::chart::kline::IndicatorId;
    use exchange::adapter::Exchange;
    use exchange::unit::price::Price;
    use exchange::{TickerInfo, Timeframe};

    const HOUR: u64 = 3_600_000;
    const BAR: u64 = 1_768_514_400_000;

    fn hit(bar_time: u64) -> AlertHit {
        AlertHit {
            ticker_info: TickerInfo::new(
                Ticker::new("BTCUSDT", Exchange::BinanceLinear),
                0.1,
                0.001,
                None,
            ),
            basis: Basis::Time(Timeframe::H1),
            source: AlertSource::Indicator {
                indicator: IndicatorId::new("roman.roman-setup").unwrap(),
                plot_id: "ltfMssDown".to_owned(),
            },
            source_name: "Roman Trade Setup".to_owned(),
            title: "LTF MSS down".to_owned(),
            bar_time: UnixMs(bar_time),
            close: Price::from_f64(95_567.3),
        }
    }

    /// Delivery with no notification or chime, so tests stay silent.
    fn quiet() -> Alerts {
        Alerts {
            notification: false,
            sound: false,
            ..Alerts::default()
        }
    }

    /// Saves and loads the history, as quitting and starting again does.
    fn restart(history: &AlertHistory) -> AlertHistory {
        serde_json::from_str(&serde_json::to_string(history).unwrap()).unwrap()
    }

    #[test]
    fn a_restart_does_not_deliver_the_same_bar_again() {
        let mut center = AlertCenter::new(&AlertHistory::default(), UnixMs(BAR));
        let (fired, _) = center.deliver(vec![hit(BAR)], &quiet());
        assert_eq!(fired.len(), 1);
        assert!(!fired[0].missed);
        let mut history = AlertHistory::default();
        for alert in fired {
            history.push(alert);
        }

        // Restarted before the next bar closed: the chart catches up on the
        // same bar, which was already delivered.
        let history = restart(&history);
        let mut center = AlertCenter::new(&history, UnixMs(BAR + HOUR + 60_000));
        let (fired, _) = center.deliver(vec![hit(BAR)], &quiet());
        assert!(fired.is_empty(), "{fired:?}");
        // The next bar still fires.
        let (fired, _) = center.deliver(vec![hit(BAR + HOUR)], &quiet());
        assert_eq!(fired.len(), 1);
    }

    #[test]
    fn entries_without_a_key_do_not_block_alerts() {
        let mut history = AlertHistory::default();
        history.push(FiredAlert {
            condition_key: None,
            ..fired_alert(&hit(BAR), false)
        });
        let mut center = AlertCenter::new(&restart(&history), UnixMs(BAR + 2 * HOUR));
        assert_eq!(center.deliver(vec![hit(BAR)], &quiet()).0.len(), 1);
    }

    #[test]
    fn bars_that_closed_before_launch_are_marked_missed() {
        // Launched 30 minutes after the bar closed.
        let launched = UnixMs(BAR + HOUR + HOUR / 2);
        let mut center = AlertCenter::new(&AlertHistory::default(), launched);
        let (fired, _) = center.deliver(vec![hit(BAR), hit(BAR + HOUR)], &quiet());
        assert_eq!(
            fired.iter().map(|alert| alert.missed).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(
            summary(&hit(BAR), true),
            "BTCUSDT 1h: LTF MSS down (missed while closed)"
        );
    }
}
