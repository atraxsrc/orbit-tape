//! `orbit-tape mcp`: a Model Context Protocol server over stdio, so an AI
//! assistant (Claude Code, the Claude desktop app, any MCP client) can scan
//! markets, read candles, run indicators and manage saved alerts.
//!
//! No API keys and no orders. The only file it writes is the saved alerts
//! (`alerts.json`, see `mcp/alerts.rs`), which the app or `orbit-tape watch`
//! runs. It opens no window. Candles come from Binance or OKX USDT
//! perpetuals through the same adapters as the app and `orbit-tape scan`.
//! Logs go to stderr; stdout carries the protocol.

mod alerts;
mod routine;

use crate::headless::{
    self, MAX_BARS, Market, PerpExchange, Series, alert_plots, coin, evaluate_alerts,
    indicator_config, parse_timeframe,
};
use exchange::adapter::{MarketKind, Venue};
use exchange::{TickerInfo, Timeframe};
use indicator_engine::{
    Color, CompiledIndicator, InputValue, OutputPrecision, PlotDescriptor, Value, tooling,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub const USAGE: &str = "\
usage: orbit-tape mcp

Runs a Model Context Protocol server on stdin/stdout for AI assistants such as
Claude. It fetches public candles from Binance or OKX, runs your indicators and
manages your saved alerts; it needs no API keys and places no orders. See
docs/mcp.md.

  claude mcp add orbit-tape -- orbit-tape mcp";

const MAX_CANDLES: usize = 500;
const MAX_LAST: usize = 200;
const MAX_SCAN_SYMBOLS: usize = 100;
const MAX_LOOKBACK: usize = 50;
const MAX_ALERTS: usize = 200;
const DEFAULT_RUN_BARS: usize = 2000;
const KEY_LEVELS: &str = "key-levels.ind";
/// Decimals for quote (USDT) volume.
const VOLUME_DECIMALS: u32 = 2;
/// Significant digits for plot values with no precision of their own.
const PLOT_DIGITS: i32 = 10;

/// Runs the server until the client disconnects; returns the exit code.
pub fn run(arguments: &[String]) -> i32 {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return 0;
    }
    if let Some(argument) = arguments.first() {
        eprintln!("orbit-tape mcp: unknown argument '{argument}' (see --help)");
        return 1;
    }
    // Candle volume in the quote currency (USDT), the app's default size unit,
    // so volume-based indicators match the charts.
    exchange::unit::qty::set_preferred_currency(exchange::SizeUnit::Quote);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("orbit-tape mcp: could not start the runtime: {error}");
            return 1;
        }
    };
    let result = runtime.block_on(async {
        let service = OrbitTapeMcp::new(Paths::installed(), Arc::new(LiveCandles::default()))
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| error.to_string())?;
        service.waiting().await.map_err(|error| error.to_string())?;
        Ok::<(), String>(())
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("orbit-tape mcp: {error}");
            1
        }
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where closed candles come from: the exchanges, or files in tests.
pub trait CandleSource: Send + Sync + 'static {
    /// The listed USDT perpetual of a coin: its instrument name and details.
    fn resolve(
        &self,
        exchange: PerpExchange,
        coin: String,
    ) -> BoxFuture<'_, Result<(String, TickerInfo), String>>;

    fn closed_candles(
        &self,
        exchange: PerpExchange,
        coin: String,
        timeframe: Timeframe,
        bars: usize,
    ) -> BoxFuture<'_, Result<Series, String>>;
}

/// Exchange candles; each exchange's symbol list is loaded once.
#[derive(Default)]
pub struct LiveCandles {
    markets: Mutex<HashMap<PerpExchange, Arc<Market>>>,
}

impl LiveCandles {
    async fn market(&self, exchange: PerpExchange) -> Result<Arc<Market>, String> {
        if let Some(market) = self.lock().get(&exchange) {
            return Ok(market.clone());
        }
        let market = Arc::new(Market::connect(exchange).await?);
        self.lock().insert(exchange, market.clone());
        Ok(market)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PerpExchange, Arc<Market>>> {
        self.markets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl CandleSource for LiveCandles {
    fn resolve(
        &self,
        exchange: PerpExchange,
        coin: String,
    ) -> BoxFuture<'_, Result<(String, TickerInfo), String>> {
        Box::pin(async move {
            let market = self.market(exchange).await?;
            market
                .resolve(&coin)
                .ok_or_else(|| format!("no {} USDT perpetual for {coin}", exchange.name()))
        })
    }

    fn closed_candles(
        &self,
        exchange: PerpExchange,
        coin: String,
        timeframe: Timeframe,
        bars: usize,
    ) -> BoxFuture<'_, Result<Series, String>> {
        Box::pin(async move {
            let market = self.market(exchange).await?;
            market.closed_candles(&coin, timeframe, bars).await
        })
    }
}

/// Files the server reads. Tests point these at fixtures.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Searched in order for indicators by name.
    /// The first is the user's own folder, the rest ship with the app.
    pub indicator_folders: Vec<PathBuf>,
    /// `scan-symbols.txt`: the scan list and the default exchange. Read only;
    /// a missing file means the built-in list on OKX.
    pub symbols_file: PathBuf,
    pub alert_history: PathBuf,
    /// Saved alerts, the only file the server writes.
    pub alerts: PathBuf,
    /// Held by the program that runs the alerts.
    pub engine_lock: PathBuf,
    /// The user's trading plan (read only).
    pub plan: PathBuf,
}

impl Paths {
    fn installed() -> Self {
        Self {
            indicator_folders: headless::indicator_search_folders(),
            symbols_file: data::data_path(Some(crate::scan::SYMBOLS_FILE)),
            alert_history: data::data_path(Some(data::config::alert_history::ALERT_HISTORY_PATH)),
            alerts: data::config::alert_store::AlertStore::path(),
            engine_lock: data::config::alert_store::AlertStore::engine_lock_path(),
            plan: data::config::plan::path(),
        }
    }
}

#[derive(Clone)]
pub struct OrbitTapeMcp {
    paths: Arc<Paths>,
    candles: Arc<dyn CandleSource>,
    tool_router: ToolRouter<Self>,
}

// ---------------------------------------------------------------- parameters

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScanParams {
    /// Indicator name, ID or .ind path. Default: Roman Trade Setup
    /// (roman-setup).
    pub indicator: Option<String>,
    /// Chart timeframe: 1m 3m 5m 15m 30m 1h 2h 4h 12h 1d. Default 1h.
    pub timeframe: Option<String>,
    /// binance or okx. Default: the exchange line of the user's
    /// scan-symbols.txt, else okx.
    pub exchange: Option<String>,
    /// Coins such as BTC, ETH (BTCUSDT and BTC-USDT-SWAP work too). Default:
    /// the user's scan-symbols.txt list. At most 100.
    pub symbols: Option<Vec<String>>,
    /// Report alerts from the last n closed bars. Default 1, at most 50.
    pub lookback: Option<usize>,
    /// Indicator inputs by id or label, values as text, e.g. {"ctMode": "true"}.
    pub inputs: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CandlesParams {
    /// Coin or instrument, e.g. BTC, BTCUSDT, BTC-USDT-SWAP.
    pub symbol: String,
    /// 1m 3m 5m 15m 30m 1h 2h 4h 12h 1d. Default 1h.
    pub timeframe: Option<String>,
    /// binance or okx. Default: the exchange line of the user's
    /// scan-symbols.txt, else okx.
    pub exchange: Option<String>,
    /// Number of closed candles, newest last. Default 100, at most 500.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunParams {
    /// Installed indicator name, ID (namespace.name), title or .ind path.
    pub indicator: String,
    /// Coin or instrument, e.g. BTC.
    pub symbol: String,
    /// 1m 3m 5m 15m 30m 1h 2h 4h 12h 1d. Default 1h.
    pub timeframe: Option<String>,
    /// binance or okx. Default: the exchange line of the user's
    /// scan-symbols.txt, else okx.
    pub exchange: Option<String>,
    /// Indicator inputs by id or label, values as text.
    pub inputs: Option<BTreeMap<String, String>>,
    /// Closed bars to report, newest last. Default 20, at most 200.
    pub last: Option<usize>,
    /// Candles of history to run over. Default 2000, at most 20000.
    pub bars: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyLevelsParams {
    /// Coin or instrument, e.g. BTC.
    pub symbol: String,
    /// binance or okx. Default: the exchange line of the user's
    /// scan-symbols.txt, else okx.
    pub exchange: Option<String>,
    /// Levels to return, by group (4h, daily, monday, weekly, monthly,
    /// quarterly, yearly) or level ID (e.g. w_open, d_prev_high). Default: the
    /// levels the Key Levels indicator shows with its own settings (4H off,
    /// daily open, Monday range and mid, weekly and monthly open, high, low
    /// and mid, quarterly open and mid, yearly open and mid).
    pub levels: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckParams {
    /// A .ind file or a folder of them.
    pub path: String,
    /// Require every indicator ID to use this namespace.
    pub namespace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AlertHistoryParams {
    /// Newest alerts to return. Default 20, at most 200.
    pub last: Option<usize>,
}

// --------------------------------------------------------------------- tools

#[tool_router]
impl OrbitTapeMcp {
    pub fn new(paths: Paths, candles: Arc<dyn CandleSource>) -> Self {
        Self {
            paths: Arc::new(paths),
            candles,
            tool_router: Self::tool_router()
                + Self::alert_tool_router()
                + Self::routine_tool_router(),
        }
    }

    #[tool(
        description = "Scan USDT perpetuals with an indicator and list the alert signals that fired on the last closed bar(s). Default: Roman Trade Setup on the 1h chart over the user's scan list. Takes a few seconds per coin."
    )]
    pub async fn scan(&self, Parameters(params): Parameters<ScanParams>) -> Result<String, String> {
        let (path, indicator) = self.indicator(params.indicator.as_deref())?;
        let alert_plots = alert_plots(&indicator);
        if alert_plots.is_empty() {
            return Err(format!(
                "{} has no alert plots (plot(..., alert = true)), use run_indicator instead",
                indicator.descriptor.title
            ));
        }
        let config = indicator_config(&indicator, &inputs(params.inputs))?;
        let timeframe = timeframe(params.timeframe.as_deref())?;
        let lookback = params.lookback.unwrap_or(1);
        if !(1..=MAX_LOOKBACK).contains(&lookback) {
            return Err(format!("lookback must be 1 to {MAX_LOOKBACK}"));
        }
        let (file_exchange, symbols) = match params.symbols {
            Some(symbols) => (None, symbols.iter().map(|symbol| coin(symbol)).collect()),
            None => self.scan_list()?,
        };
        let mut coins: Vec<String> = Vec::new();
        for symbol in symbols {
            if !symbol.is_empty() && !coins.contains(&symbol) {
                coins.push(symbol);
            }
        }
        if coins.is_empty() {
            return Err("no symbols to scan".to_owned());
        }
        if coins.len() > MAX_SCAN_SYMBOLS {
            return Err(format!("at most {MAX_SCAN_SYMBOLS} symbols per scan"));
        }
        let exchange = match params.exchange.as_deref() {
            Some(text) => perp_exchange(text)?,
            None => file_exchange.unwrap_or(PerpExchange::Okx),
        };
        let bars = DEFAULT_RUN_BARS.max(lookback + 1);

        let mut rows = Vec::new();
        let mut signal_count = 0;
        let mut failed = 0;
        for coin in &coins {
            let fetched = self
                .candles
                .closed_candles(exchange, coin.clone(), timeframe, bars)
                .await;
            let result = fetched.and_then(|series| {
                evaluate_alerts(
                    &indicator,
                    &config,
                    &alert_plots,
                    coin,
                    series.ticker_info,
                    &series.klines,
                    lookback,
                )
                .map(|signals| (series, signals))
            });
            match result {
                Ok((series, signals)) => {
                    let last = series.klines.last().expect("candles are never empty");
                    let precision = Precision::of(&series.ticker_info);
                    signal_count += signals.len();
                    rows.push(json!({
                        "base": coin,
                        "symbol": series.instrument,
                        "last_closed_bar": utc(last.time.as_u64() as i64),
                        "close": precision.price(last.close.to_f64()),
                        "signals": signals
                            .iter()
                            .map(|signal| if lookback > 1 {
                                format!("{} ({})", signal.title, utc(signal.bar_time))
                            } else {
                                signal.title.clone()
                            })
                            .collect::<Vec<_>>(),
                    }));
                }
                Err(error) => {
                    failed += 1;
                    rows.push(json!({ "base": coin, "error": error }));
                }
            }
        }
        to_text(&json!({
            "indicator": indicator.descriptor.title,
            "path": path,
            "exchange": exchange_id(exchange),
            "timeframe": timeframe.to_string(),
            "lookback_bars": lookback,
            "signal_count": signal_count,
            "failed_symbols": failed,
            "rows": rows,
        }))
    }

    #[tool(
        description = "Closed OHLCV candles of a USDT perpetual on Binance or OKX, oldest first. Times are bar open times in UTC. Prices are rounded to the instrument's tick size. volume is quote volume in USDT (base volume times the bar's close, like the app's default size unit), rounded to cents."
    )]
    pub async fn get_candles(
        &self,
        Parameters(params): Parameters<CandlesParams>,
    ) -> Result<String, String> {
        let exchange = self.exchange(params.exchange.as_deref())?;
        let timeframe = timeframe(params.timeframe.as_deref())?;
        let limit = params.limit.unwrap_or(100);
        if !(1..=MAX_CANDLES).contains(&limit) {
            return Err(format!("limit must be 1 to {MAX_CANDLES}"));
        }
        let base = symbol_coin(&params.symbol)?;
        let series = self
            .candles
            .closed_candles(exchange, base.clone(), timeframe, limit)
            .await?;
        let precision = Precision::of(&series.ticker_info);
        let first = series.klines.len().saturating_sub(limit);
        let rows = series.klines[first..]
            .iter()
            .map(|kline| {
                json!([
                    utc(kline.time.as_u64() as i64),
                    precision.price(kline.open.to_f64()),
                    precision.price(kline.high.to_f64()),
                    precision.price(kline.low.to_f64()),
                    precision.price(kline.close.to_f64()),
                    round_decimals(kline.volume.total().to_f64(), VOLUME_DECIMALS),
                ])
            })
            .collect::<Vec<_>>();
        to_text(&json!({
            "base": base,
            "symbol": series.instrument,
            "exchange": exchange_id(exchange),
            "timeframe": timeframe.to_string(),
            "volume_unit": "USDT",
            "columns": ["time_utc", "open", "high", "low", "close", "volume"],
            "candles": rows,
        }))
    }

    #[tool(
        description = "Run an indicator over closed candles and return its plot values, markers, alerts and info panel for the last N bars."
    )]
    pub async fn run_indicator(
        &self,
        Parameters(params): Parameters<RunParams>,
    ) -> Result<String, String> {
        let (path, indicator) = self.indicator(Some(&params.indicator))?;
        let config = indicator_config(&indicator, &inputs(params.inputs))?;
        let exchange = self.exchange(params.exchange.as_deref())?;
        let timeframe = timeframe(params.timeframe.as_deref())?;
        let last = params.last.unwrap_or(20);
        if !(1..=MAX_LAST).contains(&last) {
            return Err(format!("last must be 1 to {MAX_LAST}"));
        }
        let bars = params.bars.unwrap_or(DEFAULT_RUN_BARS);
        if !(last..=MAX_BARS).contains(&bars) {
            return Err(format!("bars must be {last} to {MAX_BARS}"));
        }
        let base = symbol_coin(&params.symbol)?;
        let series = self
            .candles
            .closed_candles(exchange, base.clone(), timeframe, bars)
            .await?;
        let report = indicator_report(&indicator, &config, &series, last)?;
        to_text(&json!({
            "indicator": indicator.descriptor.title,
            "id": indicator.descriptor.id.as_str(),
            "path": path,
            "base": base,
            "symbol": series.instrument,
            "exchange": exchange_id(exchange),
            "timeframe": timeframe.to_string(),
            "bars_run": series.klines.len(),
            "report": report,
        }))
    }

    #[tool(
        description = "Current Key Levels of a USDT perpetual, sorted by price, with the last close and whether each level is above or below it. By default the levels the Key Levels indicator shows with its own settings; pass levels to ask for others (4H, daily, Monday, weekly, monthly, quarterly, yearly opens, previous highs, lows and mids)."
    )]
    pub async fn key_levels(
        &self,
        Parameters(params): Parameters<KeyLevelsParams>,
    ) -> Result<String, String> {
        let (_, indicator) = self.indicator(Some(KEY_LEVELS))?;
        let config = indicator_config(
            &indicator,
            &[("exportValues".to_owned(), "true".to_owned())],
        )?;
        let wanted = key_level_selection(&indicator, params.levels.as_deref())?;
        let exchange = self.exchange(params.exchange.as_deref())?;
        let base = symbol_coin(&params.symbol)?;
        let series = self
            .candles
            .closed_candles(
                exchange,
                base.clone(),
                Timeframe::H1,
                key_levels_bars(chrono::Utc::now()),
            )
            .await?;
        let precision = Precision::of(&series.ticker_info);
        let mut last_output = None;
        headless::run_closed(
            &indicator,
            &config,
            series.ticker_info,
            &series.klines,
            |_, output| last_output = Some(output.clone()),
        )?;
        let output = last_output.ok_or("no candles")?;
        let close = series
            .klines
            .last()
            .map(|kline| precision.price(kline.close.to_f64()))
            .unwrap_or_default();
        let mut levels = indicator
            .descriptor
            .plots
            .iter()
            .filter(|plot| wanted.contains(&plot.id))
            .filter_map(|plot| {
                let value = output
                    .plots
                    .iter()
                    .find(|out| out.id == plot.id)
                    .and_then(|out| number(&out.value))?;
                Some((plot.label.clone(), plot.id.clone(), precision.price(value)))
            })
            .collect::<Vec<_>>();
        if levels.is_empty() {
            return Err("Key Levels returned no levels for this history".to_owned());
        }
        levels.sort_by(|a, b| b.2.total_cmp(&a.2));
        to_text(&json!({
            "base": base,
            "symbol": series.instrument,
            "exchange": exchange_id(exchange),
            "selection": if params.levels.is_some() { "requested" } else { "indicator defaults" },
            "as_of_bar_utc": utc(output.time),
            "close": close,
            "levels": levels
                .iter()
                .map(|(label, id, value)| json!({
                    "label": label,
                    "id": id,
                    "price": value,
                    "side": if *value > close { "above" } else { "below" },
                }))
                .collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "Compile .ind indicator files like `orbit-tape-indicator check`: syntax, types, IDs, filenames and duplicate IDs."
    )]
    pub async fn check_indicator(
        &self,
        Parameters(params): Parameters<CheckParams>,
    ) -> Result<String, String> {
        let (report, _) = check_sources(&params)?;
        if report.diagnostics.is_empty() {
            Ok(format!("check passed: {} source(s)", report.sources.len()))
        } else {
            Err(format!(
                "check failed: {} diagnostic(s)\n{}",
                report.diagnostics.len(),
                report.diagnostics.join("\n")
            ))
        }
    }

    #[tool(
        description = "Check .ind indicator files and run each on synthetic bars, like `orbit-tape-indicator test`."
    )]
    pub async fn test_indicator(
        &self,
        Parameters(params): Parameters<CheckParams>,
    ) -> Result<String, String> {
        let (report, _) = check_sources(&params)?;
        let mut lines = report.diagnostics.clone();
        let mut passed = Vec::new();
        for source in &report.sources {
            match tooling::smoke_test(source) {
                Ok(()) => passed.push(format!("test passed: {}", source.path.display())),
                Err(error) => lines.push(format!("{}:1:1: error: {error}", source.path.display())),
            }
        }
        if lines.is_empty() {
            passed.push(format!("test passed: {} source(s)", report.sources.len()));
            Ok(passed.join("\n"))
        } else {
            Err(format!(
                "test failed: {} diagnostic(s)\n{}",
                lines.len(),
                lines.join("\n")
            ))
        }
    }

    #[tool(
        description = "List the user's installed indicators: name, ID, path (relative to its indicators folder, with source user or bundled), inputs (with defaults and options) and alert conditions. Colors are #RRGGBB, #RRGGBBAA, or theme.<role> (theme.<role>@AA with alpha)."
    )]
    pub async fn list_indicators(&self) -> Result<String, String> {
        let indicators = self
            .installed()
            .into_iter()
            .map(|found| {
                let descriptor = &found.indicator.descriptor;
                let indicator = &found.indicator;
                json!({
                    "name": descriptor.title,
                    "id": descriptor.id.as_str(),
                    "path": found.path,
                    "source": found.source,
                    "description": descriptor.description,
                    "inputs": descriptor.inputs.iter().map(|input| {
                        let mut entry = json!({
                            "id": input.id,
                            "label": input.label,
                            "type": format!("{:?}", input.value_type).to_lowercase(),
                            "default": input_json(&input.default),
                        });
                        if !input.options.is_empty() {
                            entry["options"] = json!(input
                                .options
                                .iter()
                                .map(|option| option.id.clone())
                                .collect::<Vec<_>>());
                        }
                        if let Some(group) = &input.group {
                            entry["group"] = json!(group);
                        }
                        entry
                    }).collect::<Vec<_>>(),
                    "alerts": alert_plots(indicator)
                        .into_iter()
                        .map(|(_, title)| title)
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        to_text(&json!({ "indicators": indicators }))
    }

    #[tool(
        description = "The newest alerts Orbit Tape fired (indicator and price alerts), newest first, from its saved alert history: when it fired, the chart's exchange, market and timeframe, the bar it fired on (null for entries saved by older versions), the condition and price."
    )]
    pub async fn alert_history(
        &self,
        Parameters(params): Parameters<AlertHistoryParams>,
    ) -> Result<String, String> {
        let last = params.last.unwrap_or(20);
        if !(1..=MAX_ALERTS).contains(&last) {
            return Err(format!("last must be 1 to {MAX_ALERTS}"));
        }
        let path = &self.paths.alert_history;
        let history = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<data::config::alert_history::AlertHistory>(&text)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return to_text(&json!({ "alerts": [], "note": "no alerts fired yet" }));
            }
            Err(error) => return Err(format!("could not read {}: {error}", path.display())),
        };
        let alerts = history
            .entries()
            .take(last)
            .map(|alert| {
                let exchange = alert.ticker.exchange;
                json!({
                    "fired_utc": utc(alert.fired_at as i64),
                    "bar_utc": alert.bar_time.map(|time| utc(time as i64)),
                    "symbol": alert.symbol,
                    "exchange": venue_id(exchange.venue()),
                    "market": market_id(exchange.market_type()),
                    "timeframe": alert.basis.to_string(),
                    "source": alert.source,
                    "condition": alert.condition,
                    "missed_while_closed": alert.missed,
                    "price": alert.price.parse::<f64>().ok(),
                })
            })
            .collect::<Vec<_>>();
        to_text(&json!({ "total_saved": history.len(), "alerts": alerts }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for OrbitTapeMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("orbit-tape", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Orbit Tape: crypto market tools. Candles are closed bars of USDT \
                 perpetuals on Binance or OKX; times are UTC. Use list_indicators to see the \
                 user's indicators and their inputs, scan for alert signals across many coins, \
                 run_indicator for one coin's values, key_levels for support and resistance \
                 levels, and alert_history for what fired. create_alert, list_alerts, \
                 update_alert, delete_alert, pause_alerts and resume_alerts manage saved \
                 alerts (price crosses and indicator conditions) that the app or orbit-tape \
                 watch runs and sends to the desktop or the user's phone (ntfy). Nothing here \
                 places orders or is financial advice.",
            )
    }
}

// ------------------------------------------------------------------- helpers

impl OrbitTapeMcp {
    /// Every installed indicator that compiles, first folder wins per ID.
    fn installed(&self) -> Vec<Installed> {
        let mut found: Vec<Installed> = Vec::new();
        for (index, folder) in self.paths.indicator_folders.iter().enumerate() {
            let Ok((paths, _)) = indicator_engine::source_files_in_directory(folder) else {
                continue;
            };
            for path in paths {
                let Ok(indicator) = headless::compile(&path) else {
                    continue;
                };
                let id = indicator.descriptor.id.as_str().to_owned();
                if found
                    .iter()
                    .any(|known| known.indicator.descriptor.id.as_str() == id)
                {
                    continue;
                }
                found.push(Installed {
                    stem: path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or_default()
                        .to_lowercase(),
                    path: path
                        .strip_prefix(folder)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                    source: if index == 0 { "user" } else { "bundled" },
                    indicator,
                });
            }
        }
        found
    }

    /// The exchange asked for, else the scan list's, else OKX.
    fn exchange(&self, text: Option<&str>) -> Result<PerpExchange, String> {
        match text {
            Some(text) => perp_exchange(text),
            None => Ok(self
                .scan_list()
                .ok()
                .and_then(|(exchange, _)| exchange)
                .unwrap_or(PerpExchange::Okx)),
        }
    }

    /// The user's scan-symbols.txt (never created), or the built-in list.
    fn scan_list(&self) -> Result<(Option<PerpExchange>, Vec<String>), String> {
        let path = &self.paths.symbols_file;
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let (exchange, symbols) = crate::scan::parse_symbols(&text)
                    .map_err(|error| format!("scan-symbols.txt: {error}"))?;
                if symbols.is_empty() {
                    return Err("scan-symbols.txt lists no coins".to_owned());
                }
                Ok((exchange, symbols))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
                None,
                crate::scan::DEFAULT_SYMBOLS
                    .iter()
                    .map(|&symbol| symbol.to_owned())
                    .collect(),
            )),
            Err(error) => Err(format!("scan-symbols.txt: {error}")),
        }
    }

    /// An indicator by .ind path, ID, file name, short name or title, with the
    /// path to show: relative to its indicators folder, or as the client gave
    /// it.
    fn indicator(&self, wanted: Option<&str>) -> Result<(String, CompiledIndicator), String> {
        let wanted = wanted
            .map(str::trim)
            .filter(|wanted| !wanted.is_empty())
            .unwrap_or(headless::DEFAULT_INDICATOR);
        let as_path = Path::new(wanted);
        if wanted.ends_with(".ind") && as_path.components().count() > 1 {
            let path = source_path(wanted)?;
            if path.is_dir() {
                return Err(format!("{wanted} is a folder, not a .ind file"));
            }
            let indicator = headless::compile(&path)?;
            return Ok((wanted.to_owned(), indicator));
        }
        let key = wanted.trim_end_matches(".ind").to_lowercase();
        let matches = |found: &Installed| {
            let id = found.indicator.descriptor.id.as_str().to_lowercase();
            id == key
                || id.rsplit('.').next() == Some(key.as_str())
                || found.stem == key
                || found.indicator.descriptor.title.to_lowercase() == key
        };
        self.installed()
            .into_iter()
            .find(matches)
            .map(|found| (found.path, found.indicator))
            .ok_or_else(|| {
                format!(
                    "unknown indicator '{wanted}'; use list_indicators to see the installed ones, or pass a .ind path"
                )
            })
    }
}

/// An installed indicator, with its path relative to its folder.
struct Installed {
    path: String,
    stem: String,
    /// "user" for the user's own folder, "bundled" for the app's copies.
    source: &'static str,
    indicator: CompiledIndicator,
}

/// A path the client asked for, resolved: an existing folder, or a regular
/// `.ind` file. A link to anything else (`x.ind -> ~/.ssh/id_ed25519`) is
/// refused, so only indicator sources are ever read; folder scans skip links.
fn source_path(text: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(text.trim());
    let resolved = requested
        .canonicalize()
        .map_err(|error| format!("{}: {error}", requested.display()))?;
    if resolved.is_dir() {
        return Ok(resolved);
    }
    if resolved.is_file()
        && resolved
            .extension()
            .is_some_and(|extension| extension == "ind")
    {
        return Ok(resolved);
    }
    Err(format!("{} is not a .ind file", requested.display()))
}

fn check_sources(params: &CheckParams) -> Result<(tooling::SourceReport, PathBuf), String> {
    let path = source_path(&params.path)?;
    let paths = tooling::source_paths(&path)?;
    Ok((
        tooling::load_sources(&paths, params.namespace.as_deref()),
        path,
    ))
}

/// Plot values, markers, alerts and the info panel for the last `last` bars.
fn indicator_report(
    indicator: &CompiledIndicator,
    config: &indicator_engine::IndicatorConfig,
    series: &Series,
    last: usize,
) -> Result<Json, String> {
    let plots = &indicator.descriptor.plots;
    let precision = Precision::of(&series.ticker_info);
    let first = series.klines.len().saturating_sub(last);
    let mut rows = Vec::new();
    let mut markers = Vec::new();
    let mut alerts = Vec::new();
    let mut info = Vec::new();
    headless::run_closed(
        indicator,
        config,
        series.ticker_info,
        &series.klines,
        |index, output| {
            if index < first {
                return;
            }
            let kline = &series.klines[index];
            let mut row = vec![
                json!(utc(output.time)),
                json!(precision.price(kline.close.to_f64())),
            ];
            for plot in plots {
                let value = output
                    .plots
                    .iter()
                    .find(|out| out.id == plot.id)
                    .map_or(Json::Null, |out| {
                        value_json(&out.value, |value| precision.plot(plot, value))
                    });
                if plot.alert && value == json!(true) {
                    alerts.push(json!({ "time_utc": utc(output.time), "alert": plot.label }));
                }
                row.push(value);
            }
            rows.push(Json::Array(row));
            // A marker without a price is one the script did not show on
            // this bar (`visible = false`).
            for marker in output
                .markers
                .iter()
                .filter(|marker| marker.value.is_some())
            {
                markers.push(json!({
                    "time_utc": utc(output.time),
                    "id": marker.id,
                    "text": marker.text,
                    "price": marker.value.map(|value| precision.price(value)),
                }));
            }
            if index + 1 == series.klines.len() {
                info = output
                    .info_rows
                    .iter()
                    .map(|row| json!([row.left, row.right]))
                    .collect();
            }
        },
    )?;
    let mut columns = vec!["time_utc".to_owned(), "close".to_owned()];
    columns.extend(plots.iter().map(|plot| plot.id.clone()));
    Ok(json!({
        "columns": columns,
        "plot_titles": plots
            .iter()
            .map(|plot| (plot.id.clone(), plot.label.clone()))
            .collect::<BTreeMap<_, _>>(),
        "rows": rows,
        "markers": markers,
        "alerts": alerts,
        "info_panel": info,
    }))
}

/// 1H bars back to the start of the previous quarter or the year, whichever
/// is earlier, so every Key Levels period is complete.
fn key_levels_bars(now: chrono::DateTime<chrono::Utc>) -> usize {
    use chrono::Datelike;
    let year_start = chrono::NaiveDate::from_ymd_opt(now.year(), 1, 1).expect("valid date");
    let quarter_month = (now.month0() / 3) * 3 + 1;
    let (prev_year, prev_month) = if quarter_month <= 3 {
        (now.year() - 1, quarter_month + 9)
    } else {
        (now.year(), quarter_month - 3)
    };
    let prev_quarter =
        chrono::NaiveDate::from_ymd_opt(prev_year, prev_month, 1).expect("valid date");
    let start = year_start.min(prev_quarter);
    let hours = (now.date_naive() - start).num_hours().max(0) as usize;
    // A week of margin for the Monday range and previous periods.
    (hours + 24 * 8).min(MAX_BARS)
}

fn perp_exchange(text: &str) -> Result<PerpExchange, String> {
    PerpExchange::parse(text).ok_or_else(|| format!("unknown exchange '{text}' (binance or okx)"))
}

/// Exchange names in every output: the same words the tools take.
fn exchange_id(exchange: PerpExchange) -> &'static str {
    match exchange {
        PerpExchange::Binance => "binance",
        PerpExchange::Okx => "okx",
    }
}

fn venue_id(venue: Venue) -> &'static str {
    match venue {
        Venue::Binance => "binance",
        Venue::Okex => "okx",
        Venue::Bybit => "bybit",
        Venue::Hyperliquid => "hyperliquid",
        Venue::Mexc => "mexc",
    }
}

fn market_id(market: MarketKind) -> &'static str {
    match market {
        MarketKind::Spot => "spot",
        MarketKind::LinearPerps => "perp",
        MarketKind::InversePerps => "inverse_perp",
    }
}

/// Rounding for one instrument: prices to its tick size.
#[derive(Clone, Copy)]
struct Precision {
    price_decimals: u32,
}

impl Precision {
    fn of(info: &TickerInfo) -> Self {
        Self {
            price_decimals: (-i32::from(info.min_ticksize.power)).max(0) as u32,
        }
    }

    fn price(self, value: f64) -> f64 {
        round_decimals(value, self.price_decimals)
    }

    /// A plot value: with the plot's own precision, as a price for
    /// `precision = "instrument"`, else to `PLOT_DIGITS` significant digits.
    fn plot(self, plot: &PlotDescriptor, value: f64) -> f64 {
        match plot.output_precision {
            Some(OutputPrecision::Instrument) => self.price(value),
            Some(OutputPrecision::Decimals(decimals)) => round_decimals(value, decimals.into()),
            Some(OutputPrecision::Step(step)) if step > 0.0 => {
                significant((value / step).round() * step, PLOT_DIGITS)
            }
            _ => significant(value, PLOT_DIGITS),
        }
    }
}

fn round_decimals(value: f64, decimals: u32) -> f64 {
    let scale = 10f64.powi(decimals.min(15) as i32);
    let rounded = (value * scale).round() / scale;
    if rounded.is_finite() { rounded } else { value }
}

/// `value` to `digits` significant digits (only ever drops float noise).
fn significant(value: f64, digits: i32) -> f64 {
    if value == 0.0 || !value.is_finite() {
        return value;
    }
    let decimals = digits - 1 - value.abs().log10().floor() as i32;
    if decimals <= 0 {
        return value.round();
    }
    round_decimals(value, decimals as u32)
}

/// A color as text: `#RRGGBB`, `#RRGGBBAA` when not opaque, or a theme role
/// (`theme.danger`, `theme.danger@80` with alpha) that the app resolves from
/// the desktop theme.
fn color_text(bits: u64) -> String {
    let color = Color(bits);
    if color.theme_role().is_some() {
        return color.to_string();
    }
    let argb = color.argb_bits();
    let (alpha, rgb) = (argb >> 24, argb & 0x00ff_ffff);
    if alpha == 0xff {
        format!("#{rgb:06x}")
    } else {
        format!("#{rgb:06x}{alpha:02x}")
    }
}

/// Plot IDs of the Key Levels indicator to report: the requested groups and
/// IDs, or the ones its own settings show.
fn key_level_selection(
    indicator: &CompiledIndicator,
    requested: Option<&[String]>,
) -> Result<Vec<String>, String> {
    const GROUPS: [(&str, &str); 7] = [
        ("4h", "h4"),
        ("daily", "d"),
        ("monday", "mon"),
        ("weekly", "w"),
        ("monthly", "m"),
        ("quarterly", "q"),
        ("yearly", "y"),
    ];
    let plots = indicator
        .descriptor
        .plots
        .iter()
        .filter_map(|plot| Some((plot.id.clone(), key_level_toggle(&plot.id)?)))
        .collect::<Vec<_>>();
    let Some(requested) = requested else {
        let shown =
            |toggle: &str| {
                indicator.descriptor.inputs.iter().any(|input| {
                    input.id == toggle && matches!(input.default, InputValue::Bool(true))
                })
            };
        return Ok(plots
            .into_iter()
            .filter(|(_, toggle)| shown(toggle))
            .map(|(id, _)| id)
            .collect());
    };
    let mut wanted = Vec::new();
    for entry in requested {
        let entry = entry.trim().to_lowercase();
        let prefix = GROUPS
            .iter()
            .find(|(group, _)| *group == entry)
            .map(|(_, prefix)| format!("{prefix}_"));
        let found = plots
            .iter()
            .filter(|(id, _)| match &prefix {
                Some(prefix) => id.starts_with(prefix.as_str()),
                None => *id == entry,
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if found.is_empty() {
            let groups = GROUPS.map(|(group, _)| group).join(", ");
            return Err(format!(
                "unknown level '{entry}' (groups: {groups}; or level IDs such as w_open, d_prev_high)"
            ));
        }
        wanted.extend(found);
    }
    Ok(wanted)
}

/// The Key Levels input that shows a level: `w_prev_high` is shown by
/// `wRange`, `y_mid` by `yMid`.
fn key_level_toggle(plot_id: &str) -> Option<String> {
    let (period, kind) = plot_id.split_once('_')?;
    if !["h4", "d", "mon", "w", "m", "q", "y"].contains(&period) {
        return None;
    }
    let toggle = match kind {
        "open" => "Open",
        "prev_high" | "prev_low" | "high" | "low" => "Range",
        "prev_mid" | "mid" => "Mid",
        _ => return None,
    };
    Some(format!("{period}{toggle}"))
}

fn timeframe(text: Option<&str>) -> Result<Timeframe, String> {
    text.map_or(Ok(Timeframe::H1), parse_timeframe)
}

fn symbol_coin(symbol: &str) -> Result<String, String> {
    let coin = coin(symbol);
    if coin.is_empty() || !coin.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("invalid symbol '{symbol}'"));
    }
    Ok(coin)
}

fn inputs(inputs: Option<BTreeMap<String, String>>) -> Vec<(String, String)> {
    inputs.unwrap_or_default().into_iter().collect()
}

fn utc(time_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(time_ms)
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Float(value) if value.is_finite() => Some(*value),
        Value::Int(value) => Some(*value as f64),
        _ => None,
    }
}

fn value_json(value: &Value, round: impl Fn(f64) -> f64) -> Json {
    match value {
        Value::Int(value) => json!(value),
        Value::Float(value) if value.is_finite() => json!(round(*value)),
        Value::Float(_) | Value::Na => Json::Null,
        Value::Bool(value) => json!(value),
        Value::String(value) => json!(value),
        Value::Color(value) => json!(color_text(*value)),
        other => json!(format!("{other:?}")),
    }
}

fn input_json(value: &InputValue) -> Json {
    match value {
        InputValue::Int(value) | InputValue::Time(value) => json!(value),
        InputValue::Float(value) => json!(value),
        InputValue::Bool(value) => json!(value),
        InputValue::String(value) => json!(value),
        InputValue::Color(value) => json!(color_text(*value)),
    }
}

fn to_text(value: &Json) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::alerts::{AlertIdParams, AlertIdsParams};
    use super::*;
    use exchange::adapter::Exchange;
    use exchange::{Kline, Ticker, TickerInfo};

    const FIXTURE: &str = "indicators/market/tests/binance-btcusdt-perp-1h-20250601-20260115.csv";

    /// BTC 1h candles from a CSV; every other coin or timeframe is unlisted.
    struct FixtureCandles;

    fn fixture_info() -> TickerInfo {
        TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            0.1,
            0.001,
            None,
        )
    }

    impl CandleSource for FixtureCandles {
        fn resolve(
            &self,
            exchange: PerpExchange,
            coin: String,
        ) -> BoxFuture<'_, Result<(String, TickerInfo), String>> {
            Box::pin(async move {
                match (exchange, coin.as_str()) {
                    (PerpExchange::Binance, "BTC") => Ok(("BTCUSDT".to_owned(), fixture_info())),
                    (PerpExchange::Okx, "BTC") => Ok((
                        "BTC-USDT-SWAP".to_owned(),
                        TickerInfo::new(
                            Ticker::new("BTC-USDT-SWAP", Exchange::OkexLinear),
                            0.1,
                            0.01,
                            None,
                        ),
                    )),
                    _ => Err(format!("no {} USDT perpetual for {coin}", exchange.name())),
                }
            })
        }

        fn closed_candles(
            &self,
            exchange: PerpExchange,
            coin: String,
            timeframe: Timeframe,
            bars: usize,
        ) -> BoxFuture<'_, Result<Series, String>> {
            Box::pin(async move {
                if coin != "BTC" || timeframe != Timeframe::H1 {
                    return Err(format!("no {} USDT perpetual for {coin}", exchange.name()));
                }
                let ticker_info = TickerInfo::new(
                    Ticker::new("BTCUSDT", Exchange::BinanceLinear),
                    0.1,
                    0.001,
                    None,
                );
                let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
                let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
                let mut klines = text
                    .lines()
                    .skip(1)
                    .map(|line| {
                        let field = line
                            .split(',')
                            .map(|value| value.parse::<f64>().unwrap())
                            .collect::<Vec<_>>();
                        Kline::new(
                            field[0] as u64,
                            field[1],
                            field[2],
                            field[3],
                            field[4],
                            exchange::Volume::TotalOnly(exchange::unit::Qty::from_f64(field[5])),
                            ticker_info.min_ticksize,
                        )
                    })
                    .collect::<Vec<_>>();
                let first = klines.len().saturating_sub(bars);
                klines.drain(..first);
                Ok(Series {
                    instrument: "BTCUSDT".to_owned(),
                    ticker_info,
                    klines,
                })
            })
        }
    }

    /// Every request fails, like an exchange that is down.
    struct DownCandles;

    impl CandleSource for DownCandles {
        fn resolve(
            &self,
            _: PerpExchange,
            _: String,
        ) -> BoxFuture<'_, Result<(String, TickerInfo), String>> {
            Box::pin(async { Err("symbols: connection refused".to_owned()) })
        }

        fn closed_candles(
            &self,
            _: PerpExchange,
            _: String,
            _: Timeframe,
            _: usize,
        ) -> BoxFuture<'_, Result<Series, String>> {
            Box::pin(async { Err("candles: connection refused".to_owned()) })
        }
    }

    fn repo(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    fn server_with(candles: Arc<dyn CandleSource>, paths: Paths) -> OrbitTapeMcp {
        OrbitTapeMcp::new(paths, candles)
    }

    fn paths() -> Paths {
        Paths {
            indicator_folders: vec![repo("indicators")],
            symbols_file: repo("target/no-such-scan-symbols.txt"),
            alert_history: repo("target/no-such-alert-history.json"),
            alerts: repo("target/no-such-alerts.json"),
            engine_lock: repo("target/no-such-alert-engine"),
            plan: repo("docs/plan.example.toml"),
        }
    }

    fn server() -> OrbitTapeMcp {
        server_with(Arc::new(FixtureCandles), paths())
    }

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn parse(result: Result<String, String>) -> Json {
        serde_json::from_str(&result.expect("tool succeeds")).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("orbit-tape-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_reports_rows_and_unlisted_symbols() {
        let out = parse(block_on(server().scan(Parameters(ScanParams {
            indicator: None,
            timeframe: None,
            exchange: Some("binance".into()),
            symbols: Some(vec!["BTCUSDT".into(), "DOGE".into(), "btc".into()]),
            lookback: Some(24),
            inputs: Some(BTreeMap::from([("ctMode".into(), "true".into())])),
        }))));
        assert_eq!(out["indicator"], "Roman Trade Setup (H12 to H1)");
        assert_eq!(out["exchange"], "binance");
        assert_eq!(out["path"], "roman/roman-setup.ind");
        let rows = out["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "duplicates are dropped: {rows:?}");
        assert_eq!(rows[0]["base"], "BTC");
        assert_eq!(rows[0]["symbol"], "BTCUSDT");
        assert_eq!(rows[0]["last_closed_bar"], "2026-01-15 23:00");
        assert_eq!(rows[0]["close"], 95567.3);
        assert!(rows[0]["signals"].is_array());
        assert_eq!(rows[1]["base"], "DOGE");
        assert!(
            rows[1]["error"]
                .as_str()
                .unwrap()
                .contains("no Binance USDT perpetual")
        );
        assert_eq!(out["failed_symbols"], 1);

        let bad_input = block_on(server().scan(Parameters(ScanParams {
            indicator: None,
            timeframe: None,
            exchange: None,
            symbols: Some(vec!["BTC".into()]),
            lookback: None,
            inputs: Some(BTreeMap::from([("nope".into(), "1".into())])),
        })));
        assert_eq!(bad_input.unwrap_err(), "unknown input 'nope'");
        let no_alerts = block_on(server().scan(Parameters(ScanParams {
            indicator: Some("market.sma".into()),
            timeframe: None,
            exchange: None,
            symbols: Some(vec!["BTC".into()]),
            lookback: None,
            inputs: None,
        })));
        assert!(no_alerts.unwrap_err().contains("no alert plots"));
    }

    #[test]
    fn scan_reads_the_symbols_file_without_writing_one() {
        let dir = temp_dir("symbols");
        let file = dir.join("scan-symbols.txt");
        std::fs::write(&file, "exchange = okx\nBTC-USDT-SWAP\n").unwrap();
        let mut with_file = paths();
        with_file.symbols_file = file.clone();
        let server = server_with(Arc::new(FixtureCandles), with_file);
        let out = parse(block_on(server.scan(Parameters(ScanParams {
            indicator: Some("roman-setup".into()),
            timeframe: Some("1h".into()),
            exchange: None,
            symbols: None,
            lookback: None,
            inputs: None,
        }))));
        assert_eq!(out["exchange"], "okx");
        assert_eq!(out["rows"].as_array().unwrap().len(), 1);

        // Every tool takes its default exchange from the same line.
        std::fs::write(&file, "exchange = binance\nBTC\n").unwrap();
        let candles = parse(block_on(server.get_candles(Parameters(CandlesParams {
            symbol: "BTC".into(),
            timeframe: None,
            exchange: None,
            limit: Some(1),
        }))));
        assert_eq!(candles["exchange"], "binance");
        let levels = parse(block_on(server.key_levels(Parameters(KeyLevelsParams {
            symbol: "BTC".into(),
            exchange: None,
            levels: Some(vec!["daily".into()]),
        }))));
        assert_eq!(levels["exchange"], "binance");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "exchange = binance\nBTC\n",
            "the scan list is never written"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_candles_returns_the_newest_closed_bars() {
        let out = parse(block_on(server().get_candles(Parameters(CandlesParams {
            symbol: "BTC".into(),
            timeframe: Some("1h".into()),
            exchange: None,
            limit: Some(3),
        }))));
        assert_eq!(out["base"], "BTC");
        assert_eq!(out["symbol"], "BTCUSDT");
        // No scan list: OKX, like scan.
        assert_eq!(out["exchange"], "okx");
        assert_eq!(out["volume_unit"], "USDT");
        let candles = out["candles"].as_array().unwrap();
        assert_eq!(candles.len(), 3);
        assert_eq!(candles[2][0], "2026-01-15 23:00");
        assert_eq!(candles[2][4], 95567.3);
        for candle in candles {
            for (column, decimals) in [(1, 1), (2, 1), (3, 1), (4, 1), (5, 2)] {
                let text = candle[column].to_string();
                let places = text
                    .split_once('.')
                    .map_or(0, |(_, fraction)| fraction.len());
                assert!(places <= decimals, "column {column}: {text}");
            }
        }

        for (limit, timeframe, exchange, expected) in [
            (501, None, None, "limit must be 1 to 500"),
            (5, Some("7h"), None, "unknown timeframe '7h'"),
            (
                5,
                None,
                Some("kraken"),
                "unknown exchange 'kraken' (binance or okx)",
            ),
        ] {
            let error = block_on(server().get_candles(Parameters(CandlesParams {
                symbol: "BTC".into(),
                timeframe: timeframe.map(str::to_owned),
                exchange: exchange.map(str::to_owned),
                limit: Some(limit),
            })))
            .unwrap_err();
            assert!(error.starts_with(expected), "{error}");
        }
        let unknown = block_on(server().get_candles(Parameters(CandlesParams {
            symbol: "NOPE".into(),
            timeframe: None,
            exchange: None,
            limit: None,
        })));
        assert!(
            unknown
                .unwrap_err()
                .contains("no OKX USDT perpetual for NOPE")
        );
        let invalid = block_on(server().get_candles(Parameters(CandlesParams {
            symbol: "../etc".into(),
            timeframe: None,
            exchange: None,
            limit: None,
        })));
        assert!(invalid.unwrap_err().starts_with("invalid symbol"));
    }

    #[test]
    fn run_indicator_reports_the_last_bars() {
        let out = parse(block_on(server().run_indicator(Parameters(RunParams {
            indicator: "SMA x4".into(),
            symbol: "BTC".into(),
            timeframe: None,
            exchange: None,
            inputs: Some(BTreeMap::from([("len1".into(), "10".into())])),
            last: Some(3),
            bars: Some(500),
        }))));
        assert_eq!(out["id"], "market.sma");
        assert_eq!(out["path"], "market/sma.ind");
        assert_eq!(out["base"], "BTC");
        assert_eq!(out["exchange"], "okx");
        assert_eq!(out["bars_run"], 500);
        let report = &out["report"];
        let columns = report["columns"].as_array().unwrap();
        assert_eq!(
            columns[..3],
            [json!("time_utc"), json!("close"), json!("sma1")]
        );
        let rows = report["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows[2][2].is_f64(), "{:?}", rows[2]);

        let roman = parse(block_on(server().run_indicator(Parameters(RunParams {
            indicator: "roman.roman-setup".into(),
            symbol: "BTC".into(),
            timeframe: None,
            exchange: None,
            inputs: None,
            last: Some(200),
            bars: None,
        }))));
        assert_eq!(roman["report"]["rows"].as_array().unwrap().len(), 200);
        assert!(roman["report"]["alerts"].is_array());
        // Only markers the script showed, each with a price.
        let markers = roman["report"]["markers"].as_array().unwrap();
        assert!(markers.len() < 200, "{} markers", markers.len());
        assert!(markers.iter().all(|marker| marker["price"].is_f64()));
        // The setup values carry the TradingView titles.
        let titles = &roman["report"]["plot_titles"];
        for title in [
            "x_bias",
            "x_tapUpAgo",
            "x_mssDir",
            "x_b1St",
            "x_ctArm",
            "x_bullFlip",
        ] {
            assert_eq!(titles[title], title);
        }

        let error = block_on(server().run_indicator(Parameters(RunParams {
            indicator: "market.sma".into(),
            symbol: "BTC".into(),
            timeframe: None,
            exchange: None,
            inputs: None,
            last: Some(201),
            bars: None,
        })));
        assert_eq!(error.unwrap_err(), "last must be 1 to 200");
    }

    #[test]
    fn key_levels_lists_levels_sorted_by_price() {
        let out = parse(block_on(server().key_levels(Parameters(KeyLevelsParams {
            symbol: "BTC".into(),
            exchange: None,
            levels: None,
        }))));
        assert_eq!(out["as_of_bar_utc"], "2026-01-15 23:00");
        assert_eq!(out["base"], "BTC");
        assert_eq!(out["selection"], "indicator defaults");
        let levels = out["levels"].as_array().unwrap();
        let ids = levels
            .iter()
            .map(|level| level["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        // The indicator's own settings: 4H off, daily open only, ...
        assert!(ids.iter().all(|id| !id.starts_with("h4_")), "{ids:?}");
        for id in [
            "d_open",
            "mon_high",
            "w_prev_mid",
            "m_open",
            "q_open",
            "y_mid",
        ] {
            assert!(ids.contains(&id), "{id} missing from {ids:?}");
        }
        for id in ["d_prev_high", "q_prev_high", "y_high"] {
            assert!(!ids.contains(&id), "{id} is off by default: {ids:?}");
        }
        let prices = levels
            .iter()
            .map(|level| level["price"].as_f64().unwrap())
            .collect::<Vec<_>>();
        assert!(prices.windows(2).all(|pair| pair[0] >= pair[1]));
        let labels = levels
            .iter()
            .map(|level| level["label"].as_str().unwrap())
            .collect::<Vec<_>>();
        for label in ["Weekly open", "Monthly open", "Yearly open"] {
            assert!(labels.contains(&label), "{label} missing from {labels:?}");
        }
        for level in levels {
            let text = level["price"].to_string();
            assert!(
                text.split_once('.')
                    .is_none_or(|(_, fraction)| fraction.len() <= 1),
                "{text}"
            );
        }

        let requested = parse(block_on(server().key_levels(Parameters(KeyLevelsParams {
            symbol: "BTC".into(),
            exchange: Some("binance".into()),
            levels: Some(vec!["4h".into(), "D_PREV_HIGH".into()]),
        }))));
        assert_eq!(requested["selection"], "requested");
        let mut ids = requested["levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|level| level["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            [
                "d_prev_high",
                "h4_open",
                "h4_prev_high",
                "h4_prev_low",
                "h4_prev_mid"
            ]
        );
        let unknown = block_on(server().key_levels(Parameters(KeyLevelsParams {
            symbol: "BTC".into(),
            exchange: None,
            levels: Some(vec!["hourly".into()]),
        })));
        assert!(unknown.unwrap_err().starts_with("unknown level 'hourly'"));
    }

    #[test]
    fn colors_are_hex_or_theme_roles() {
        assert_eq!(color_text(0xff11_2233), "#112233");
        assert_eq!(color_text(0x8011_2233), "#11223380");
        assert_eq!(color_text(0xd_f000_0000), "theme.text@f0");
        assert_eq!(
            color_text(Color::theme(indicator_engine::ThemeColor::Danger).0),
            "theme.danger"
        );
    }

    #[test]
    fn rounds_to_the_instrument() {
        let info = TickerInfo::new(
            Ticker::new("PEPEUSDT", Exchange::BinanceLinear),
            0.0000001,
            1.0,
            None,
        );
        let precision = Precision::of(&info);
        assert_eq!(precision.price(0.000_012_345_67), 0.000_012_3);
        assert_eq!(
            round_decimals(132_861_314.768_700_02, VOLUME_DECIMALS),
            132_861_314.77
        );
        assert_eq!(significant(0.1 + 0.2, PLOT_DIGITS), 0.3);
        assert_eq!(
            significant(132_861_314.768_700_02, PLOT_DIGITS),
            132_861_314.8
        );
        assert_eq!(
            significant(-0.000_123_456_789_012_3, PLOT_DIGITS),
            -0.000_123_456_789_0
        );
    }

    #[test]
    fn key_levels_history_covers_the_year_and_previous_quarter() {
        use chrono::TimeZone;
        let at = |y, m, d| chrono::Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap();
        // 15 Feb: previous quarter (Oct-Dec) starts before the year.
        assert_eq!(
            key_levels_bars(at(2026, 2, 15)),
            (31 + 30 + 31 + 31 + 14) * 24 + 192
        );
        // 24 Nov: the year starts before the previous quarter (Jul-Sep).
        assert_eq!(key_levels_bars(at(2026, 11, 24)), 327 * 24 + 192);
    }

    #[test]
    fn check_and_test_match_the_cli() {
        let ok = block_on(server().check_indicator(Parameters(CheckParams {
            path: repo("examples/indicators").display().to_string(),
            namespace: None,
        })));
        assert_eq!(ok.unwrap(), "check passed: 6 source(s)");
        let tested = block_on(server().test_indicator(Parameters(CheckParams {
            path: repo("indicators/market").display().to_string(),
            namespace: Some("market".into()),
        })))
        .unwrap();
        assert!(tested.ends_with("test passed: 3 source(s)"), "{tested}");

        let dir = temp_dir("check");
        let bad = dir.join("broken.ind");
        std::fs::write(
            &bad,
            "language(1)\nindicator(\"B\", id = \"x.broken\")\nplot(nope)\n",
        )
        .unwrap();
        let failed = block_on(server().check_indicator(Parameters(CheckParams {
            path: bad.display().to_string(),
            namespace: None,
        })))
        .unwrap_err();
        assert!(failed.starts_with("check failed:"), "{failed}");
        assert!(failed.contains("broken.ind:"), "{failed}");
        let not_ind = dir.join("notes.txt");
        std::fs::write(&not_ind, "hello").unwrap();
        let refused = block_on(server().check_indicator(Parameters(CheckParams {
            path: not_ind.display().to_string(),
            namespace: None,
        })));
        assert!(refused.unwrap_err().ends_with("is not a .ind file"));
        let disguised = dir.join("secret.ind");
        std::os::unix::fs::symlink(&not_ind, &disguised).unwrap();
        for result in [
            block_on(server().check_indicator(Parameters(CheckParams {
                path: disguised.display().to_string(),
                namespace: None,
            }))),
            block_on(server().run_indicator(Parameters(RunParams {
                indicator: disguised.display().to_string(),
                symbol: "BTC".into(),
                timeframe: None,
                exchange: None,
                inputs: None,
                last: None,
                bars: None,
            }))),
        ] {
            let error = result.unwrap_err();
            assert!(error.ends_with("secret.ind is not a .ind file"), "{error}");
            assert!(!error.contains("hello"), "{error}");
        }
        let missing = block_on(server().check_indicator(Parameters(CheckParams {
            path: dir.join("missing.ind").display().to_string(),
            namespace: None,
        })));
        assert!(missing.unwrap_err().contains("missing.ind"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn list_indicators_shows_inputs_and_alerts() {
        let out = parse(block_on(server().list_indicators()));
        let indicators = out["indicators"].as_array().unwrap();
        let roman = indicators
            .iter()
            .find(|indicator| indicator["id"] == "roman.roman-setup")
            .unwrap();
        assert!(!roman["alerts"].as_array().unwrap().is_empty());
        let ct = roman["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|input| input["id"] == "ctMode")
            .unwrap();
        assert_eq!(ct["type"], "bool");
        assert_eq!(ct["default"], false);
        assert_eq!(roman["path"], "roman/roman-setup.ind");
        assert_eq!(roman["source"], "user");

        let mut colors = 0;
        for indicator in indicators {
            let path = indicator["path"].as_str().unwrap();
            assert!(!path.starts_with('/'), "{path}");
            for input in indicator["inputs"].as_array().unwrap() {
                if input["type"] != "color" {
                    continue;
                }
                colors += 1;
                let text = input["default"].as_str().unwrap();
                let hex = text.strip_prefix('#').is_some_and(|digits| {
                    matches!(digits.len(), 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit())
                });
                let theme = text.strip_prefix("theme.").is_some_and(|role| {
                    let (name, alpha) = role.split_once('@').unwrap_or((role, "ff"));
                    name.chars().all(|c| c.is_ascii_lowercase())
                        && alpha.len() == 2
                        && alpha.chars().all(|c| c.is_ascii_hexdigit())
                });
                assert!(hex || theme, "{}: {text}", input["id"]);
            }
        }
        assert!(colors > 10, "{colors} color inputs");
    }

    #[test]
    fn alert_history_reads_the_newest_entries() {
        let empty = parse(block_on(
            server().alert_history(Parameters(AlertHistoryParams { last: None })),
        ));
        assert_eq!(empty["alerts"], json!([]));

        let dir = temp_dir("alerts");
        let file = dir.join("alert-history.json");
        let mut history = data::config::alert_history::AlertHistory::default();
        for (index, condition) in ["LTF MSS up", "HTF Bullish SFP", "OB formed"]
            .iter()
            .enumerate()
        {
            history.push(data::config::alert_history::FiredAlert {
                fired_at: 1_768_518_000_000 + index as u64 * 3_600_000,
                ticker: Ticker::new("BTCUSDT", Exchange::BinanceLinear),
                basis: data::chart::Basis::Time(Timeframe::H1),
                symbol: "BTCUSDT".into(),
                exchange: "BinanceLinear".into(),
                source: "Roman Trade Setup".into(),
                condition: (*condition).into(),
                price: "95567.3".into(),
                bar_time: (index > 0).then_some(1_768_514_400_000 + index as u64 * 3_600_000),
                condition_key: Some("roman.roman-setup/signal".into()),
                missed: index == 2,
            });
        }
        std::fs::write(&file, serde_json::to_string(&history).unwrap()).unwrap();
        let mut with_history = paths();
        with_history.alert_history = file.clone();
        let out = parse(block_on(
            server_with(Arc::new(FixtureCandles), with_history)
                .alert_history(Parameters(AlertHistoryParams { last: Some(2) })),
        ));
        assert_eq!(out["total_saved"], 3);
        let alerts = out["alerts"].as_array().unwrap();
        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[0]["condition"], "OB formed");
        assert_eq!(alerts[0]["fired_utc"], "2026-01-16 01:00");
        assert_eq!(alerts[0]["bar_utc"], "2026-01-16 00:00");
        assert_eq!(alerts[0]["exchange"], "binance");
        assert_eq!(alerts[0]["market"], "perp");
        assert_eq!(alerts[0]["timeframe"], "1h");
        assert_eq!(alerts[0]["price"], 95567.3);
        assert_eq!(alerts[0]["missed_while_closed"], true);
        let older = parse(block_on(
            server_with(Arc::new(FixtureCandles), {
                let mut paths = paths();
                paths.alert_history = file.clone();
                paths
            })
            .alert_history(Parameters(AlertHistoryParams { last: Some(3) })),
        ));
        assert_eq!(older["alerts"][2]["bar_utc"], Json::Null);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            serde_json::to_string(&history).unwrap(),
            "the history file is never written"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn network_errors_are_tool_errors() {
        let server = server_with(Arc::new(DownCandles), paths());
        let error = block_on(server.get_candles(Parameters(CandlesParams {
            symbol: "BTC".into(),
            timeframe: None,
            exchange: None,
            limit: None,
        })));
        assert_eq!(error.unwrap_err(), "candles: connection refused");
        let scan = parse(block_on(server.scan(Parameters(ScanParams {
            indicator: None,
            timeframe: None,
            exchange: None,
            symbols: Some(vec!["BTC".into(), "ETH".into()]),
            lookback: None,
            inputs: None,
        }))));
        assert_eq!(scan["failed_symbols"], 2);
        // The same server keeps answering.
        assert!(block_on(server.list_indicators()).is_ok());
    }

    // ------------------------------------------------------------- alerts

    fn alert_server(name: &str) -> (OrbitTapeMcp, PathBuf) {
        let dir = temp_dir(&format!("alerts-{name}"));
        let paths = Paths {
            alerts: dir.join("alerts.json"),
            engine_lock: dir.join("alert-engine"),
            ..paths()
        };
        (server_with(Arc::new(FixtureCandles), paths), dir)
    }

    fn create(server: &OrbitTapeMcp, params: Json) -> Result<String, String> {
        block_on(server.create_alert(Parameters(serde_json::from_value(params).unwrap())))
    }

    #[test]
    fn create_list_update_pause_resume_and_delete_a_price_alert() {
        let (server, dir) = alert_server("price");
        let out = parse(create(
            &server,
            json!({"symbol": "BTC", "exchange": "okx", "condition": "crosses_above", "price": 62000}),
        ));
        let id = out["id"].as_str().unwrap().to_owned();
        assert_eq!(
            out["summary"],
            "BTC-USDT-SWAP OKX 1h: price crosses above 62000 (once, desktop)"
        );
        assert_eq!(out["alert"]["created_by"], "claude");
        assert_eq!(out["alert"]["exchange"], "okx");
        assert!(out["engine"].as_str().unwrap().starts_with("not running"));

        let list = parse(block_on(server.list_alerts()));
        assert_eq!(list["active"], 1);
        assert_eq!(list["max_active"], 50);
        assert_eq!(list["alerts"][0]["id"], id.as_str());
        assert_eq!(list["alerts"][0]["status"], "active");

        let updated = parse(block_on(server.update_alert(Parameters(
            serde_json::from_value(json!({"id": id, "price": 61500.5, "condition": "crosses", "frequency": "every_bar", "message": "check the 4h"})).unwrap(),
        ))));
        assert_eq!(
            updated["summary"],
            "BTC-USDT-SWAP OKX 1h: price crosses 61500.5 (every bar, desktop)"
        );
        assert_eq!(updated["alert"]["message"], "check the 4h");

        let paused = parse(block_on(
            server.pause_alerts(Parameters(AlertIdsParams { ids: None })),
        ));
        assert_eq!(paused["paused"], json!([id]));
        assert_eq!(paused["active"], 0);
        let resumed = parse(block_on(server.resume_alerts(Parameters(AlertIdsParams {
            ids: Some(vec![id.clone()]),
        }))));
        assert_eq!(resumed["resumed"], json!([id]));

        let deleted = parse(block_on(
            server.delete_alert(Parameters(AlertIdParams { id: id.clone() })),
        ));
        assert_eq!(deleted["deleted"], id.as_str());
        assert!(block_on(server.delete_alert(Parameters(AlertIdParams { id }))).is_err());
        // The app and the engine read the same file.
        let saved: data::config::alert_store::AlertStore =
            data::config::shared_file::read(&dir.join("alerts.json")).unwrap();
        assert!(saved.alerts.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_indicator_alert_on_any_roman_setup_signal() {
        let (server, dir) = alert_server("indicator");
        let out = parse(create(
            &server,
            json!({"symbol": "BTCUSDT", "exchange": "binance", "condition": "indicator", "indicator": "roman-setup", "inputs": {"ctMode": "true"}}),
        ));
        let alert = &out["alert"];
        assert_eq!(alert["indicator"], "roman.roman-setup");
        assert_eq!(alert["alert"], Json::Null);
        assert_eq!(alert["frequency"], "every_bar");
        assert_eq!(alert["inputs"]["ctMode"], "true");
        assert_eq!(
            out["summary"],
            "BTCUSDT Binance 1h: any roman.roman-setup alert (ctMode=true) (every bar, desktop)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_alert_checks_everything_before_saving() {
        let (server, dir) = alert_server("invalid");
        let fails = |params: Json, wanted: &str| {
            let error = create(&server, params).unwrap_err();
            assert!(error.contains(wanted), "{error}");
        };
        fails(
            json!({"symbol": "NOPE", "exchange": "binance", "condition": "crosses_above", "price": 1}),
            "no Binance USDT perpetual for NOPE",
        );
        fails(
            json!({"symbol": "BTC", "exchange": "kraken", "condition": "crosses_above", "price": 1}),
            "unknown exchange",
        );
        fails(
            json!({"symbol": "BTC", "condition": "crosses_above", "price": 1, "timeframe": "7m"}),
            "unknown timeframe",
        );
        fails(
            json!({"symbol": "BTC", "condition": "crosses_above", "price": -5}),
            "positive number",
        );
        fails(
            json!({"symbol": "BTC", "condition": "indicator", "indicator": "no-such-indicator"}),
            "unknown indicator",
        );
        fails(
            json!({"symbol": "BTC", "condition": "indicator", "indicator": "roman-setup", "alert": "Moon"}),
            "has no alert condition 'Moon'",
        );
        fails(
            json!({"symbol": "BTC", "condition": "indicator", "indicator": "roman-setup", "inputs": {"nope": "1"}}),
            "unknown input",
        );
        fails(
            json!({"symbol": "BTC", "condition": "crosses_above", "price": 1, "channel": "ntfy"}),
            "no ntfy topic is set",
        );
        assert!(!dir.join("alerts.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn at_most_fifty_active_alerts() {
        let (server, dir) = alert_server("cap");
        for n in 0..50 {
            create(
                &server,
                json!({"symbol": "BTC", "exchange": "binance", "condition": "crosses_below", "price": 1000 + n}),
            )
            .unwrap();
        }
        let error = create(
            &server,
            json!({"symbol": "BTC", "exchange": "binance", "condition": "crosses_below", "price": 999}),
        )
        .unwrap_err();
        assert!(error.contains("at most 50"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------ routine

    #[test]
    fn get_plan_returns_the_plan_and_names_a_missing_one() {
        let out = parse(block_on(server().get_plan()));
        assert!(out["path"].as_str().unwrap().ends_with("plan.example.toml"));
        assert_eq!(out["plan"]["universe"]["exchange"], "okx");
        assert!(out["plan"]["prop"]["leverage"]["BTC"].is_number());
        assert!(out["plan"]["spot"]["monthly"]["dates"][0].is_string());

        let missing = server_with(
            Arc::new(FixtureCandles),
            Paths {
                plan: repo("target/no-such-plan.toml"),
                ..paths()
            },
        );
        let error = block_on(missing.get_plan()).unwrap_err();
        assert!(error.contains("plan.example.toml"), "{error}");
    }
}
