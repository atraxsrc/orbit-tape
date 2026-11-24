//! The alert engine: evaluates the saved alerts (`alerts.json`) and delivers
//! the ones that fire.
//!
//! Exactly one program runs it at a time: whichever holds the engine lock
//! (`alert-engine.lock` in the data folder). `orbit-tape watch` waits for
//! the lock; the app takes it when nobody else has it. The others only edit
//! the alerts.
//!
//! - Price alerts follow live trades over the exchange websockets. After a
//!   reconnect, the one-minute candles of the gap are checked too, so a cross
//!   while the connection was down still fires.
//! - Indicator alerts run the indicator over the closed candles a few
//!   seconds after each bar of their timeframe closes.
//! - At start and after the computer wakes up, indicator alerts catch up on
//!   the newest closed bar if it closed at most one bar ago, labelled "missed
//!   while closed". Anything older is skipped.
//!
//! Every hit is claimed in the alert history first (see
//! `AlertHistory::claim`), so a restart or a second program never delivers
//! the same bar twice.

mod market;
mod price;

pub use market::{LiveMarkets, MarketData};

use crate::alert_channels::{self, Outgoing};
use crate::headless;
use data::chart::Basis;
use data::config::alert_history::{AlertHistory, FiredAlert};
use data::config::alert_store::{
    AlertKind, AlertStore, Channel, Frequency, StoredAlert, trim_float,
};
use data::config::shared_file;
use exchange::adapter::Exchange;
use exchange::unit::price::Price;
use exchange::{Event, Kline, Ticker, TickerInfo, Timeframe};
use iced::futures::StreamExt;
use indicator_engine::Value;
use price::{Fire, PriceRule, PriceState, Side};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Candles of history an indicator alert runs over, like `orbit-tape scan`.
const HISTORY_BARS: usize = 2_000;
/// Waits this long after a bar closes before asking for it, so the
/// exchange has it.
const CLOSE_DELAY_MS: u64 = 3_000;
/// Asks again this long after a bar was missing or the request failed.
const RETRY_MS: u64 = 10_000;
/// Waits this long before looking up a symbol again that failed.
const RESOLVE_RETRY_MS: u64 = 60_000;
/// A wall clock jump this much larger than the time that passed means the
/// computer was asleep.
const WAKE_JUMP_MS: u64 = 5_000;
/// Shown with alerts for bars that closed while nothing was running.
pub const MISSED_LABEL: &str = crate::alerts::MISSED_LABEL;

/// The files the engine reads and writes. Tests point these at a temp dir.
#[derive(Debug, Clone)]
pub struct Paths {
    pub store: PathBuf,
    pub history: PathBuf,
    /// Searched in order for indicators by ID.
    pub indicator_folders: Vec<PathBuf>,
}

impl Paths {
    pub fn installed() -> Self {
        Self {
            store: AlertStore::path(),
            history: AlertHistory::path(),
            indicator_folders: headless::indicator_search_folders(),
        }
    }
}

fn now_ms() -> u64 {
    exchange::UnixMs::now().as_u64()
}

/// An alert that fired, before it is claimed in the history.
#[derive(Debug, Clone, PartialEq)]
pub struct Firing {
    pub alert_id: String,
    pub channel: Channel,
    pub fired: FiredAlert,
    pub outgoing: Outgoing,
}

/// One market and timeframe that indicator alerts watch.
#[derive(Debug, Default)]
struct Series {
    klines: Vec<Kline>,
    /// Open time of the newest closed bar that was evaluated.
    checked: Option<u64>,
    /// Do not ask before this time (after a failure).
    next_try: u64,
}

/// What the engine needs to ask the exchange for next.
#[derive(Debug, Clone, PartialEq)]
pub struct CandleRequest {
    pub ticker_info: TickerInfo,
    pub timeframe: Timeframe,
    pub from: u64,
    pub to: u64,
}

/// The engine's state without any input or output, so tests can drive it
/// with recorded trades and candles.
pub struct Core {
    paths: Paths,
    store: AlertStore,
    /// Bars that closed before this were missed. Reset on wake.
    started_at: u64,
    infos: HashMap<Ticker, TickerInfo>,
    /// Last trade time and price per instrument.
    last_trade: HashMap<Ticker, (u64, f64)>,
    prices: HashMap<String, (PriceRule, PriceState)>,
    series: HashMap<(Ticker, Timeframe), Series>,
    /// Error text changes to save, by alert ID (`None` clears).
    errors: BTreeMap<String, Option<String>>,
}

impl Core {
    pub fn new(paths: Paths, started_at: u64) -> Self {
        Self {
            paths,
            store: AlertStore::default(),
            started_at,
            infos: HashMap::new(),
            last_trade: HashMap::new(),
            prices: HashMap::new(),
            series: HashMap::new(),
            errors: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub fn store(&self) -> &AlertStore {
        &self.store
    }

    /// Takes the alerts as saved now. Price alerts whose rule changed start
    /// over; markets nothing watches any more are forgotten.
    pub fn set_store(&mut self, store: AlertStore) {
        let mut rules = HashMap::new();
        let mut series = BTreeSet::new();
        for alert in store.alerts.iter().filter(|alert| alert.enabled) {
            match &alert.kind {
                AlertKind::Price { cross, level } => {
                    rules.insert(
                        alert.id.clone(),
                        PriceRule {
                            cross: *cross,
                            level: *level,
                            frequency: alert.frequency,
                        },
                    );
                }
                AlertKind::Indicator { .. } => {
                    series.insert((alert.ticker, alert.timeframe));
                }
            }
        }
        self.prices
            .retain(|id, (rule, _)| rules.get(id).is_some_and(|wanted| wanted == rule));
        for (id, rule) in rules {
            self.prices
                .entry(id)
                .or_insert_with(|| (rule, PriceState::default()));
        }
        self.series.retain(|key, _| series.contains(key));
        for key in series {
            self.series.entry(key).or_default();
        }
        self.store = store;
    }

    /// The computer woke up (or the engine started late): catch up like at
    /// start, and forget prices from before the sleep.
    pub fn wake(&mut self, now: u64) {
        self.started_at = now;
        self.last_trade.clear();
        for series in self.series.values_mut() {
            series.checked = None;
            series.next_try = 0;
        }
    }

    pub fn set_ticker_info(&mut self, ticker_info: TickerInfo) {
        self.infos.insert(ticker_info.ticker, ticker_info);
    }

    /// Instruments that need their details looked up, and the alerts that
    /// wait for each.
    pub fn unresolved(&self) -> BTreeMap<Ticker, Vec<String>> {
        let mut wanted: BTreeMap<Ticker, Vec<String>> = BTreeMap::new();
        for alert in self.store.alerts.iter().filter(|alert| alert.enabled) {
            if !self.infos.contains_key(&alert.ticker) {
                wanted
                    .entry(alert.ticker)
                    .or_default()
                    .push(alert.id.clone());
            }
        }
        wanted
    }

    /// Instruments with enabled price alerts, by exchange.
    pub fn price_streams(&self) -> BTreeMap<Exchange, Vec<TickerInfo>> {
        let mut streams: BTreeMap<Exchange, Vec<TickerInfo>> = BTreeMap::new();
        let tickers: BTreeSet<Ticker> = self
            .store
            .alerts
            .iter()
            .filter(|alert| alert.enabled && matches!(alert.kind, AlertKind::Price { .. }))
            .map(|alert| alert.ticker)
            .collect();
        for ticker in tickers {
            if let Some(info) = self.infos.get(&ticker) {
                streams.entry(ticker.exchange).or_default().push(*info);
            }
        }
        streams
    }

    /// Records an error for an alert (or clears it), to be saved.
    pub fn set_error(&mut self, id: &str, error: Option<String>) {
        let Some(alert) = self.store.get_mut(id) else {
            return;
        };
        if alert.error != error {
            alert.error.clone_from(&error);
            self.errors.insert(id.to_owned(), error);
        }
    }

    pub fn take_errors(&mut self) -> BTreeMap<String, Option<String>> {
        std::mem::take(&mut self.errors)
    }

    /// Live trades of one instrument, oldest first: `(time, price)`.
    pub fn on_trades(&mut self, ticker: Ticker, trades: &[(u64, f64)], now: u64) -> Vec<Firing> {
        let mut firings = Vec::new();
        for &(time, price) in trades {
            let previous = self.last_trade.insert(ticker, (time, price));
            let Some((_, previous)) = previous else {
                continue;
            };
            firings.extend(self.step_prices(
                ticker,
                |rule, state, bar_ms| {
                    rule.step(state, previous, price, time / bar_ms * bar_ms)
                        .map(|fire| vec![(fire, time, price)])
                        .unwrap_or_default()
                },
                now,
            ));
        }
        firings
    }

    /// One-minute candles that closed while the trade stream was down.
    pub fn on_gap(&mut self, ticker: Ticker, klines: &[Kline], now: u64) -> Vec<Firing> {
        let mut firings = Vec::new();
        for kline in klines {
            let Some(&(last_time, previous)) = self.last_trade.get(&ticker) else {
                return firings;
            };
            if kline.time.as_u64() + Timeframe::M1.to_milliseconds() <= last_time {
                continue;
            }
            let (high, low, close) = (
                kline.high.to_f64(),
                kline.low.to_f64(),
                kline.close.to_f64(),
            );
            let time = kline.time.as_u64();
            firings.extend(self.step_prices(
                ticker,
                |rule, state, bar_ms| {
                    rule.step_candle(state, previous, (high, low, close), time / bar_ms * bar_ms)
                        .into_iter()
                        .map(|fire| {
                            // A candle has no single trade price; report the level.
                            (fire, time, rule.level)
                        })
                        .collect()
                },
                now,
            ));
            self.last_trade.insert(ticker, (time, close));
        }
        firings
    }

    /// Runs `step` for every enabled price alert on `ticker`.
    fn step_prices(
        &mut self,
        ticker: Ticker,
        step: impl Fn(&PriceRule, &mut PriceState, u64) -> Vec<(Fire, u64, f64)>,
        now: u64,
    ) -> Vec<Firing> {
        let mut firings = Vec::new();
        let ids: Vec<String> = self
            .store
            .alerts
            .iter()
            .filter(|alert| alert.enabled && alert.ticker == ticker)
            .filter(|alert| matches!(alert.kind, AlertKind::Price { .. }))
            .map(|alert| alert.id.clone())
            .collect();
        for id in ids {
            let Some(alert) = self.store.get(&id).cloned() else {
                continue;
            };
            let Some((rule, state)) = self.prices.get_mut(&id) else {
                continue;
            };
            let bar_ms = alert.timeframe.to_milliseconds();
            for (fire, time, price) in step(rule, state, bar_ms) {
                let bar = time / bar_ms * bar_ms;
                firings.push(price_firing(
                    &alert,
                    self.infos.get(&ticker).copied(),
                    fire,
                    // `once` alerts are keyed by the trade, `every_bar` by the bar.
                    if alert.frequency == Frequency::Once {
                        time
                    } else {
                        bar
                    },
                    price,
                    now,
                ));
                if alert.frequency == Frequency::Once {
                    self.disable_fired(&id, now);
                    break;
                }
            }
        }
        firings
    }

    /// A `once` alert that fired stops at once, before the store is saved.
    fn disable_fired(&mut self, id: &str, now: u64) {
        if let Some(alert) = self.store.get_mut(id) {
            alert.enabled = false;
            alert.fired_at = Some(now);
        }
        self.prices.remove(id);
    }

    /// Candle requests that are due now: the bar of a watched series closed
    /// (plus a short delay) and was not evaluated yet.
    pub fn due(&self, now: u64) -> Vec<CandleRequest> {
        let mut due = Vec::new();
        for ((ticker, timeframe), series) in &self.series {
            let Some(ticker_info) = self.infos.get(ticker).copied() else {
                continue;
            };
            let bar_ms = timeframe.to_milliseconds();
            let forming = now / bar_ms * bar_ms;
            let newest_closed = forming - bar_ms;
            if series
                .checked
                .is_some_and(|checked| checked >= newest_closed)
                || now < forming + CLOSE_DELAY_MS
                || now < series.next_try
            {
                continue;
            }
            let from = series.klines.last().map_or_else(
                || forming.saturating_sub(bar_ms * HISTORY_BARS as u64),
                |last| last.time.as_u64() + bar_ms,
            );
            due.push(CandleRequest {
                ticker_info,
                timeframe: *timeframe,
                from,
                to: forming,
            });
        }
        due.sort_by_key(|request| request.ticker_info.ticker);
        due
    }

    /// The exchange could not deliver: ask again a little later.
    pub fn candles_failed(&mut self, ticker: Ticker, timeframe: Timeframe, now: u64) {
        if let Some(series) = self.series.get_mut(&(ticker, timeframe)) {
            series.next_try = now + RETRY_MS;
        }
    }

    /// New closed candles of a series: evaluates its indicator alerts on the
    /// bars that closed since the last look.
    pub fn on_candles(
        &mut self,
        ticker: Ticker,
        timeframe: Timeframe,
        klines: Vec<Kline>,
        now: u64,
    ) -> Vec<Firing> {
        let bar_ms = timeframe.to_milliseconds();
        let newest_closed = (now / bar_ms * bar_ms).saturating_sub(bar_ms);
        let Some(series) = self.series.get_mut(&(ticker, timeframe)) else {
            return Vec::new();
        };
        let last = series.klines.last().map(|kline| kline.time);
        series.klines.extend(
            klines
                .into_iter()
                .filter(|kline| last.is_none_or(|last| kline.time > last)),
        );
        let excess = series.klines.len().saturating_sub(HISTORY_BARS);
        series.klines.drain(..excess);
        if series
            .klines
            .last()
            .is_none_or(|kline| kline.time.as_u64() < newest_closed)
        {
            // The exchange does not have the bar yet.
            series.next_try = now + RETRY_MS;
            return Vec::new();
        }
        // Bars to report: closed since the last look (only the newest on a
        // first look), and at most one bar ago.
        let checked = series.checked;
        let wanted: BTreeSet<u64> = series
            .klines
            .iter()
            .map(|kline| kline.time.as_u64())
            .filter(|&time| match checked {
                Some(checked) => time > checked,
                None => time == newest_closed,
            })
            .filter(|&time| now.saturating_sub(time + bar_ms) <= bar_ms)
            .collect();
        series.checked = Some(newest_closed);
        series.next_try = 0;
        if wanted.is_empty() {
            return Vec::new();
        }
        let klines = series.klines.clone();

        let alerts: Vec<StoredAlert> = self
            .store
            .alerts
            .iter()
            .filter(|alert| {
                alert.enabled
                    && alert.ticker == ticker
                    && alert.timeframe == timeframe
                    && matches!(alert.kind, AlertKind::Indicator { .. })
            })
            .cloned()
            .collect();
        let Some(ticker_info) = self.infos.get(&ticker).copied() else {
            return Vec::new();
        };
        let mut firings = Vec::new();
        for alert in alerts {
            match self.evaluate(&alert, ticker_info, &klines, &wanted) {
                Ok(signals) => {
                    self.set_error(&alert.id, None);
                    let once = alert.frequency == Frequency::Once;
                    // A `once` alert reports its first signal only.
                    let take = if once { 1 } else { signals.len() };
                    for signal in signals.iter().take(take) {
                        firings.push(indicator_firing(
                            &alert,
                            ticker_info,
                            signal,
                            signal.bar_time + bar_ms < self.started_at,
                            now,
                        ));
                    }
                    if once && !signals.is_empty() {
                        self.disable_fired(&alert.id, now);
                    }
                }
                Err(error) => {
                    log::warn!("alert {}: {error}", alert.id);
                    self.set_error(&alert.id, Some(error));
                }
            }
        }
        firings
    }

    /// The alert plots of an indicator alert that are true on `wanted` bars.
    fn evaluate(
        &self,
        alert: &StoredAlert,
        ticker_info: TickerInfo,
        klines: &[Kline],
        wanted: &BTreeSet<u64>,
    ) -> Result<Vec<Signal>, String> {
        let AlertKind::Indicator {
            indicator,
            plot,
            inputs,
        } = &alert.kind
        else {
            return Ok(Vec::new());
        };
        let (_, compiled) =
            headless::find_indicator_by_id(&self.paths.indicator_folders, indicator)?;
        let inputs: Vec<(String, String)> = inputs
            .iter()
            .map(|(id, value)| (id.clone(), value.clone()))
            .collect();
        let config = headless::indicator_config(&compiled, &inputs)?;
        let mut plots = headless::alert_plots(&compiled);
        if let Some(plot) = plot {
            plots.retain(|(id, _)| id == plot);
            if plots.is_empty() {
                return Err(format!("{indicator} has no alert condition {plot}"));
            }
        }
        if plots.is_empty() {
            return Err(format!("{indicator} has no alert conditions"));
        }
        let title = compiled.descriptor.title.clone();
        let mut signals = Vec::new();
        headless::run_closed(&compiled, &config, ticker_info, klines, |index, output| {
            let time = output.time as u64;
            if !wanted.contains(&time) {
                return;
            }
            for (id, plot_title) in &plots {
                let fired = output
                    .plots
                    .iter()
                    .any(|value| value.id == *id && matches!(value.value, Value::Bool(true)));
                if fired {
                    signals.push(Signal {
                        indicator: title.clone(),
                        plot_id: id.clone(),
                        plot_title: plot_title.clone(),
                        bar_time: time,
                        close: klines[index].close,
                    });
                }
            }
        })?;
        Ok(signals)
    }
}

/// One alert plot true on one closed bar.
#[derive(Debug, Clone, PartialEq)]
struct Signal {
    indicator: String,
    plot_id: String,
    plot_title: String,
    bar_time: u64,
    close: Price,
}

fn exchange_name(ticker: &Ticker) -> String {
    ticker
        .symbol_and_exchange_string()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn local_hm(time_ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(time_ms as i64)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%a %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn with_message(mut body: String, alert: &StoredAlert) -> String {
    let message = alert.message.trim();
    if !message.is_empty() {
        body.push('\n');
        body.push_str(message);
    }
    body
}

fn price_text(price: f64, ticker_info: Option<TickerInfo>) -> String {
    match ticker_info {
        Some(info) => Price::from_f64(price)
            .round_to_min_tick(info.min_ticksize)
            .to_string(info.min_ticksize),
        None => trim_float(price),
    }
}

fn price_firing(
    alert: &StoredAlert,
    ticker_info: Option<TickerInfo>,
    fire: Fire,
    bar_time: u64,
    price: f64,
    now: u64,
) -> Firing {
    let AlertKind::Price { level, .. } = alert.kind else {
        unreachable!("price alerts only");
    };
    let level = price_text(level, ticker_info);
    let title = match fire {
        Fire::Crossed(Side::Above) => format!("crossed above {level}"),
        Fire::Crossed(Side::Below) => format!("crossed below {level}"),
        Fire::Stays(Side::Above) => format!("still above {level}"),
        Fire::Stays(Side::Below) => format!("still below {level}"),
    };
    let symbol = alert.symbol();
    let price = price_text(price, ticker_info);
    let exchange = exchange_name(&alert.ticker);
    Firing {
        alert_id: alert.id.clone(),
        channel: alert.channel,
        fired: FiredAlert {
            fired_at: now,
            ticker: alert.ticker,
            basis: Basis::Time(alert.timeframe),
            symbol: symbol.clone(),
            exchange: exchange.clone(),
            source: "Price alert".to_owned(),
            condition: title.clone(),
            price: price.clone(),
            bar_time: Some(bar_time),
            condition_key: Some(alert.condition_key()),
            missed: false,
        },
        outgoing: Outgoing {
            summary: format!("{symbol}: {title}"),
            body: with_message(format!("Price alert\nLast {price}\n{exchange}"), alert),
        },
    }
}

fn indicator_firing(
    alert: &StoredAlert,
    ticker_info: TickerInfo,
    signal: &Signal,
    missed: bool,
    now: u64,
) -> Firing {
    let symbol = alert.symbol();
    let exchange = exchange_name(&alert.ticker);
    let close = signal.close.to_string(ticker_info.min_ticksize);
    let mut summary = format!("{symbol} {}: {}", alert.timeframe, signal.plot_title);
    if missed {
        summary.push_str(&format!(" ({MISSED_LABEL})"));
    }
    Firing {
        alert_id: alert.id.clone(),
        channel: alert.channel,
        fired: FiredAlert {
            fired_at: now,
            ticker: alert.ticker,
            basis: Basis::Time(alert.timeframe),
            symbol,
            exchange: exchange.clone(),
            source: signal.indicator.clone(),
            condition: signal.plot_title.clone(),
            price: close.clone(),
            bar_time: Some(signal.bar_time),
            condition_key: Some(format!("{}/{}", alert.condition_key(), signal.plot_id)),
            missed,
        },
        outgoing: Outgoing {
            summary,
            body: with_message(
                format!(
                    "{}\nClose {close}, bar opened {}\n{exchange}",
                    signal.indicator,
                    local_hm(signal.bar_time)
                ),
                alert,
            ),
        },
    }
}

/// Claims `firings` in the history, sends the claimed ones and saves what
/// changed in the alerts (fired, turned off, errors). Returns the claimed
/// ones.
pub fn deliver(paths: &Paths, core: &mut Core, firings: Vec<Firing>) -> Vec<Firing> {
    let now = now_ms();
    let claimed = if firings.is_empty() {
        Vec::new()
    } else {
        let candidates = firings.iter().map(|firing| firing.fired.clone()).collect();
        match AlertHistory::claim_in(&paths.history, candidates) {
            Ok((claimed, _)) => firings
                .into_iter()
                .filter(|firing| claimed.contains(&firing.fired))
                .collect(),
            Err(error) => {
                // Better a possible duplicate than a lost alert.
                log::warn!("could not save the alert history: {error}");
                firings
            }
        }
    };
    let ntfy = core.store.channels.ntfy.clone();
    for firing in &claimed {
        log::info!("alert {}: {}", firing.alert_id, firing.outgoing.summary);
        alert_channels::send(firing.outgoing.clone(), firing.channel, &ntfy, |_| {});
    }

    let errors = core.take_errors();
    let fired: BTreeMap<String, bool> = claimed
        .iter()
        .filter_map(|firing| {
            let alert = core.store.get(&firing.alert_id)?;
            Some((firing.alert_id.clone(), alert.frequency == Frequency::Once))
        })
        .collect();
    if fired.is_empty() && errors.is_empty() {
        return claimed;
    }
    let saved = AlertStore::update_in(&paths.store, |store| {
        for (id, once) in &fired {
            if let Some(alert) = store.get_mut(id) {
                alert.fired_at = Some(now);
                if *once {
                    alert.enabled = false;
                }
            }
        }
        for (id, error) in &errors {
            if let Some(alert) = store.get_mut(id) {
                alert.error.clone_from(error);
            }
        }
    });
    if let Err(error) = saved {
        log::warn!("could not save the alerts: {error}");
    }
    claimed
}

/// Runs the engine until the process ends. Call it while holding the engine
/// lock.
pub async fn run(paths: Paths, market: Arc<dyn MarketData>) {
    let mut core = Core::new(paths.clone(), now_ms());
    let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let mut streams: HashMap<Exchange, (Vec<TickerInfo>, tokio::task::JoinHandle<()>)> =
        HashMap::new();
    let mut disconnected: BTreeSet<Ticker> = BTreeSet::new();
    let mut resolve_failed: HashMap<Ticker, u64> = HashMap::new();
    let mut stamp = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut wall = now_ms();
    let mut clock = Instant::now();
    log::info!("alert engine started");

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now = now_ms();
                // A wall clock that moved much more than the monotonic one: the
                // computer slept.
                let elapsed = clock.elapsed().as_millis() as u64;
                if now.saturating_sub(wall) > elapsed + WAKE_JUMP_MS {
                    log::info!("alert engine: woke up after {} s", (now - wall) / 1000);
                    core.wake(now);
                    for (_, (_, task)) in streams.drain() {
                        task.abort();
                    }
                }
                wall = now;
                clock = Instant::now();

                let current = shared_file::modified(&paths.store);
                if current != stamp {
                    stamp = current;
                    match shared_file::read::<AlertStore>(&paths.store) {
                        Ok(store) => core.set_store(store),
                        Err(error) => log::warn!("alert engine: {error}"),
                    }
                }

                for (ticker, ids) in core.unresolved() {
                    if resolve_failed.get(&ticker).is_some_and(|at| now < at + RESOLVE_RETRY_MS) {
                        continue;
                    }
                    match market.ticker_info(ticker).await {
                        Ok(info) => {
                            resolve_failed.remove(&ticker);
                            core.set_ticker_info(info);
                            for id in ids {
                                core.set_error(&id, None);
                            }
                        }
                        Err(error) => {
                            log::warn!("alert engine: {error}");
                            resolve_failed.insert(ticker, now);
                            for id in ids {
                                core.set_error(&id, Some(error.clone()));
                            }
                        }
                    }
                }

                let wanted = core.price_streams();
                streams.retain(|exchange, (tickers, task)| {
                    let keep = wanted.get(exchange) == Some(tickers);
                    if !keep {
                        task.abort();
                    }
                    keep
                });
                for (exchange, tickers) in wanted {
                    if streams.contains_key(&exchange) {
                        continue;
                    }
                    let mut stream = market.trades(exchange, tickers.clone());
                    let events_tx = events_tx.clone();
                    let task = tokio::spawn(async move {
                        while let Some(event) = stream.next().await {
                            if events_tx.send(event).is_err() {
                                break;
                            }
                        }
                    });
                    streams.insert(exchange, (tickers, task));
                }

                let mut firings = Vec::new();
                for request in core.due(now) {
                    let ticker = request.ticker_info.ticker;
                    match market
                        .klines(request.ticker_info, request.timeframe, request.from, request.to)
                        .await
                    {
                        Ok(klines) => {
                            firings.extend(core.on_candles(ticker, request.timeframe, klines, now_ms()));
                        }
                        Err(error) => {
                            log::warn!(
                                "alert engine: {} {} candles: {error}",
                                ticker.display_symbol_and_type().0,
                                request.timeframe
                            );
                            core.candles_failed(ticker, request.timeframe, now);
                        }
                    }
                }
                deliver(&paths, &mut core, firings);
            }
            Some(event) = events.recv() => {
                let firings = match event {
                    Event::TradesReceived(stream, _, trades) => {
                        let ticker = stream.ticker_info().ticker;
                        let trades: Vec<(u64, f64)> = trades
                            .iter()
                            .map(|trade| (trade.time.as_u64(), trade.price.to_f64()))
                            .collect();
                        core.on_trades(ticker, &trades, now_ms())
                    }
                    Event::Disconnected(streams, reason) => {
                        log::warn!("alert engine: trade stream down: {reason}");
                        disconnected.extend(streams.iter().map(|stream| stream.ticker_info().ticker));
                        Vec::new()
                    }
                    Event::Connected(streams) => {
                        let mut firings = Vec::new();
                        for stream in streams.iter() {
                            let info = stream.ticker_info();
                            if !disconnected.remove(&info.ticker) {
                                continue;
                            }
                            let Some((last_time, _)) = core.last_trade.get(&info.ticker).copied() else {
                                continue;
                            };
                            let minute = Timeframe::M1.to_milliseconds();
                            let from = last_time / minute * minute;
                            let to = now_ms() / minute * minute;
                            match market.klines(info, Timeframe::M1, from, to).await {
                                Ok(klines) => firings.extend(core.on_gap(info.ticker, &klines, now_ms())),
                                Err(error) => log::warn!("alert engine: gap candles: {error}"),
                            }
                        }
                        firings
                    }
                    _ => Vec::new(),
                };
                deliver(&paths, &mut core, firings);
            }
        }
    }
}

/// Runs the engine on this thread for the rest of the process. Call it
/// while holding the engine lock.
pub fn run_blocking(paths: Paths) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start the runtime: {error}"))?;
    runtime.block_on(async {
        let market = Arc::new(LiveMarkets::new()?);
        run(paths, market).await;
        Ok(())
    })
}

#[cfg(test)]
mod tests;
