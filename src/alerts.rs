//! Delivers indicator alerts: a desktop notification and a short chime.
//!
//! Chart panes report alert conditions that turned true on a closed bar
//! (`data::chart::kline::alerts`). This module drops duplicates (the same
//! chart can be open in several panes), skips muted conditions, and delivers
//! the rest the way the alert settings ask.

use data::chart::kline::alerts::{AlertHit, AlertSource};
use data::config::alert_history::FiredAlert;
use data::config::alerts::Alerts;
use rodio::Source;
use std::collections::VecDeque;
use std::time::Duration;

/// Recently delivered alerts remembered for de-duplication.
const RECENT_CAPACITY: usize = 256;

#[derive(Default)]
pub struct AlertCenter {
    recent: VecDeque<String>,
    chime: Option<rodio::MixerDeviceSink>,
}

impl AlertCenter {
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
            let key = format!(
                "{:?}|{}|{}|{}",
                hit.ticker_info.ticker,
                hit.basis,
                hit.condition_key(),
                hit.bar_time.as_u64()
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

        for hit in &fresh {
            log::info!(
                "alert: {} {} {} ({})",
                symbol(hit),
                hit.basis,
                hit.title,
                hit.condition_key()
            );
            if settings.notification {
                notify(summary(hit), body(hit));
            }
        }
        let fired = fresh.iter().map(fired_alert).collect();
        let error = if settings.sound {
            self.play_chime(settings.volume).err()
        } else {
            None
        };
        (fired, error)
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
fn fired_alert(hit: &AlertHit) -> FiredAlert {
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
    }
}

fn symbol(hit: &AlertHit) -> String {
    hit.ticker_info.ticker.display_symbol_and_type().0
}

fn summary(hit: &AlertHit) -> String {
    format!("{} {}: {}", symbol(hit), hit.basis, hit.title)
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
