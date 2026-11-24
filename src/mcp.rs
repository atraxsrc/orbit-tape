//! `orbit-tape mcp`: a Model Context Protocol server over stdio, so an AI
//! assistant (Claude Code, the Claude desktop app, any MCP client) can scan
//! markets, read candles and run indicators.
//!
//! Read-only: no API keys, no orders, and no file writes. It opens no window
//! and does not talk to a running Orbit Tape. Candles come from Binance or OKX
//! USDT perpetuals through the same adapters as the app and `orbit-tape scan`.
//! Logs go to stderr; stdout carries the protocol.

use crate::headless::{
    self, MAX_BARS, Market, PerpExchange, Series, alert_plots, coin, evaluate_alerts,
    indicator_config, parse_timeframe,
};
use exchange::Timeframe;
use indicator_engine::{CompiledIndicator, InputValue, Value, tooling};
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
Claude. Read-only: it fetches public candles from Binance or OKX and runs your
indicators; it needs no API keys and places no orders. See docs/mcp.md.

  claude mcp add orbit-tape -- orbit-tape mcp";

const MAX_CANDLES: usize = 500;
const MAX_LAST: usize = 200;
const MAX_SCAN_SYMBOLS: usize = 100;
const MAX_LOOKBACK: usize = 50;
const MAX_ALERTS: usize = 200;
const DEFAULT_RUN_BARS: usize = 2000;
const KEY_LEVELS: &str = "key-levels.ind";

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
    pub indicator_folders: Vec<PathBuf>,
    pub symbols_file: Option<PathBuf>,
    pub alert_history: PathBuf,
}

impl Paths {
    fn installed() -> Self {
        Self {
            indicator_folders: headless::indicator_search_folders(),
            symbols_file: None,
            alert_history: data::data_path(Some(data::config::alert_history::ALERT_HISTORY_PATH)),
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
    /// binance or okx. Default: the exchange in the user's scan-symbols.txt,
    /// else okx.
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
    /// binance or okx. Default binance.
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
    /// binance or okx. Default binance.
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
    /// binance or okx. Default binance.
    pub exchange: Option<String>,
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
            tool_router: Self::tool_router(),
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
            None => crate::scan::symbols_from_file(self.paths.symbols_file.as_deref(), false)?,
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
            Some(text) => perp_exchange(Some(text))?,
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
                    signal_count += signals.len();
                    rows.push(json!({
                        "symbol": series.instrument,
                        "last_closed_bar": utc(last.time.as_u64() as i64),
                        "close": last.close.to_f64(),
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
                    rows.push(json!({ "symbol": coin, "error": error }));
                }
            }
        }
        to_text(&json!({
            "indicator": indicator.descriptor.title,
            "path": path.display().to_string(),
            "exchange": exchange.name(),
            "timeframe": timeframe.to_string(),
            "lookback_bars": lookback,
            "signal_count": signal_count,
            "failed_symbols": failed,
            "rows": rows,
        }))
    }

    #[tool(
        description = "Closed OHLCV candles of a USDT perpetual on Binance or OKX, oldest first. Times are bar open times in UTC."
    )]
    pub async fn get_candles(
        &self,
        Parameters(params): Parameters<CandlesParams>,
    ) -> Result<String, String> {
        let exchange = perp_exchange(params.exchange.as_deref())?;
        let timeframe = timeframe(params.timeframe.as_deref())?;
        let limit = params.limit.unwrap_or(100);
        if !(1..=MAX_CANDLES).contains(&limit) {
            return Err(format!("limit must be 1 to {MAX_CANDLES}"));
        }
        let series = self
            .candles
            .closed_candles(exchange, symbol_coin(&params.symbol)?, timeframe, limit)
            .await?;
        let first = series.klines.len().saturating_sub(limit);
        let rows = series.klines[first..]
            .iter()
            .map(|kline| {
                json!([
                    utc(kline.time.as_u64() as i64),
                    kline.open.to_f64(),
                    kline.high.to_f64(),
                    kline.low.to_f64(),
                    kline.close.to_f64(),
                    kline.volume.total().to_f64(),
                ])
            })
            .collect::<Vec<_>>();
        to_text(&json!({
            "symbol": series.instrument,
            "exchange": exchange.name(),
            "timeframe": timeframe.to_string(),
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
        let exchange = perp_exchange(params.exchange.as_deref())?;
        let timeframe = timeframe(params.timeframe.as_deref())?;
        let last = params.last.unwrap_or(20);
        if !(1..=MAX_LAST).contains(&last) {
            return Err(format!("last must be 1 to {MAX_LAST}"));
        }
        let bars = params.bars.unwrap_or(DEFAULT_RUN_BARS);
        if !(last..=MAX_BARS).contains(&bars) {
            return Err(format!("bars must be {last} to {MAX_BARS}"));
        }
        let series = self
            .candles
            .closed_candles(exchange, symbol_coin(&params.symbol)?, timeframe, bars)
            .await?;
        let report = indicator_report(&indicator, &config, &series, last)?;
        to_text(&json!({
            "indicator": indicator.descriptor.title,
            "id": indicator.descriptor.id.as_str(),
            "path": path.display().to_string(),
            "symbol": series.instrument,
            "exchange": exchange.name(),
            "timeframe": timeframe.to_string(),
            "bars_run": series.klines.len(),
            "report": report,
        }))
    }

    #[tool(
        description = "Current Key Levels of a USDT perpetual: 4H, daily, weekly, monthly, quarterly and yearly opens, previous highs, lows and mids, and the Monday range, sorted by price, with the last close."
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
        let exchange = perp_exchange(params.exchange.as_deref())?;
        let series = self
            .candles
            .closed_candles(
                exchange,
                symbol_coin(&params.symbol)?,
                Timeframe::H1,
                key_levels_bars(chrono::Utc::now()),
            )
            .await?;
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
            .map(|kline| kline.close.to_f64())
            .unwrap_or_default();
        let mut levels = indicator
            .descriptor
            .plots
            .iter()
            .filter_map(|plot| {
                let value = output
                    .plots
                    .iter()
                    .find(|out| out.id == plot.id)
                    .and_then(|out| number(&out.value))?;
                Some((plot.label.clone(), plot.id.clone(), value))
            })
            .collect::<Vec<_>>();
        if levels.is_empty() {
            return Err("Key Levels returned no levels for this history".to_owned());
        }
        levels.sort_by(|a, b| b.2.total_cmp(&a.2));
        to_text(&json!({
            "symbol": series.instrument,
            "exchange": exchange.name(),
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
        description = "List the user's installed indicators: name, ID, path, inputs (with defaults and options) and alert conditions."
    )]
    pub async fn list_indicators(&self) -> Result<String, String> {
        let indicators = self
            .installed()
            .into_iter()
            .map(|(path, indicator)| {
                let descriptor = &indicator.descriptor;
                json!({
                    "name": descriptor.title,
                    "id": descriptor.id.as_str(),
                    "path": path.display().to_string(),
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
                    "alerts": alert_plots(&indicator)
                        .into_iter()
                        .map(|(_, title)| title)
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        to_text(&json!({ "indicators": indicators }))
    }

    #[tool(
        description = "The newest alerts Orbit Tape fired (indicator and price alerts), newest first, from its saved alert history."
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
                json!({
                    "fired_utc": utc(alert.fired_at as i64),
                    "symbol": alert.symbol,
                    "exchange": alert.exchange,
                    "source": alert.source,
                    "condition": alert.condition,
                    "price": alert.price,
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
                "Orbit Tape: read-only crypto market tools. Candles are closed bars of USDT \
                 perpetuals on Binance or OKX; times are UTC. Use list_indicators to see the \
                 user's indicators and their inputs, scan for alert signals across many coins, \
                 run_indicator for one coin's values, key_levels for support and resistance \
                 levels, and alert_history for what fired in the app. Nothing here places \
                 orders or is financial advice.",
            )
    }
}

// ------------------------------------------------------------------- helpers

impl OrbitTapeMcp {
    /// Every installed indicator that compiles, first folder wins per ID.
    fn installed(&self) -> Vec<(PathBuf, CompiledIndicator)> {
        let mut found: Vec<(PathBuf, CompiledIndicator)> = Vec::new();
        for folder in &self.paths.indicator_folders {
            let Ok((paths, _)) = indicator_engine::source_files_in_directory(folder) else {
                continue;
            };
            for path in paths {
                let Ok(indicator) = headless::compile(&path) else {
                    continue;
                };
                let id = indicator.descriptor.id.as_str().to_owned();
                if !found
                    .iter()
                    .any(|(_, known)| known.descriptor.id.as_str() == id)
                {
                    found.push((path, indicator));
                }
            }
        }
        found
    }

    /// An indicator by .ind path, ID, file name, short name or title.
    fn indicator(&self, wanted: Option<&str>) -> Result<(PathBuf, CompiledIndicator), String> {
        let wanted = wanted
            .map(str::trim)
            .filter(|wanted| !wanted.is_empty())
            .unwrap_or(headless::DEFAULT_INDICATOR);
        let as_path = Path::new(wanted);
        if wanted.ends_with(".ind") && as_path.components().count() > 1 {
            let indicator = headless::compile(as_path)?;
            return Ok((as_path.to_path_buf(), indicator));
        }
        let key = wanted.trim_end_matches(".ind").to_lowercase();
        let installed = self.installed();
        let matches = |path: &Path, indicator: &CompiledIndicator| {
            let id = indicator.descriptor.id.as_str().to_lowercase();
            id == key
                || id.rsplit('.').next() == Some(key.as_str())
                || path.file_stem().and_then(|stem| stem.to_str()) == Some(key.as_str())
                || indicator.descriptor.title.to_lowercase() == key
        };
        installed
            .into_iter()
            .find(|(path, indicator)| matches(path, indicator))
            .ok_or_else(|| {
                format!(
                    "unknown indicator '{wanted}'; use list_indicators to see the installed ones, or pass a .ind path"
                )
            })
    }
}

fn check_sources(params: &CheckParams) -> Result<(tooling::SourceReport, PathBuf), String> {
    let path = PathBuf::from(params.path.trim());
    if path.is_file() && path.extension().is_none_or(|extension| extension != "ind") {
        return Err(format!("{} is not a .ind file", path.display()));
    }
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
            let mut row = vec![json!(utc(output.time)), json!(kline.close.to_f64())];
            for plot in plots {
                let value = output
                    .plots
                    .iter()
                    .find(|out| out.id == plot.id)
                    .map_or(Json::Null, |out| value_json(&out.value));
                if plot.alert && value == json!(true) {
                    alerts.push(json!({ "time_utc": utc(output.time), "alert": plot.label }));
                }
                row.push(value);
            }
            rows.push(Json::Array(row));
            for marker in output.markers.iter() {
                markers.push(json!({
                    "time_utc": utc(output.time),
                    "id": marker.id,
                    "text": marker.text,
                    "price": marker.value,
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

fn perp_exchange(text: Option<&str>) -> Result<PerpExchange, String> {
    match text {
        None => Ok(PerpExchange::Binance),
        Some(text) => PerpExchange::parse(text)
            .ok_or_else(|| format!("unknown exchange '{text}' (binance or okx)")),
    }
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

fn value_json(value: &Value) -> Json {
    match value {
        Value::Int(value) => json!(value),
        Value::Float(value) if value.is_finite() => json!(value),
        Value::Float(_) | Value::Na => Json::Null,
        Value::Bool(value) => json!(value),
        Value::String(value) => json!(value),
        Value::Color(value) => json!(format!("#{value:08x}")),
        other => json!(format!("{other:?}")),
    }
}

fn input_json(value: &InputValue) -> Json {
    match value {
        InputValue::Int(value) | InputValue::Time(value) => json!(value),
        InputValue::Float(value) => json!(value),
        InputValue::Bool(value) => json!(value),
        InputValue::String(value) => json!(value),
        InputValue::Color(value) => json!(format!("#{value:08x}")),
    }
}

fn to_text(value: &Json) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::adapter::Exchange;
    use exchange::{Kline, Ticker, TickerInfo};

    const FIXTURE: &str = "indicators/market/tests/binance-btcusdt-perp-1h-20250601-20260115.csv";

    /// BTC 1h candles from a CSV; every other coin or timeframe is unlisted.
    struct FixtureCandles;

    impl CandleSource for FixtureCandles {
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
            symbols_file: None,
            alert_history: repo("target/no-such-alert-history.json"),
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
        assert_eq!(out["exchange"], "Binance");
        let rows = out["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "duplicates are dropped: {rows:?}");
        assert_eq!(rows[0]["symbol"], "BTCUSDT");
        assert_eq!(rows[0]["last_closed_bar"], "2026-01-15 23:00");
        assert!(rows[0]["signals"].is_array());
        assert_eq!(rows[1]["symbol"], "DOGE");
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
        with_file.symbols_file = Some(file);
        let out = parse(block_on(
            server_with(Arc::new(FixtureCandles), with_file).scan(Parameters(ScanParams {
                indicator: Some("roman-setup".into()),
                timeframe: Some("1h".into()),
                exchange: None,
                symbols: None,
                lookback: None,
                inputs: None,
            })),
        ));
        assert_eq!(out["exchange"], "OKX");
        assert_eq!(out["rows"].as_array().unwrap().len(), 1);
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
        assert_eq!(out["symbol"], "BTCUSDT");
        let candles = out["candles"].as_array().unwrap();
        assert_eq!(candles.len(), 3);
        assert_eq!(candles[2][0], "2026-01-15 23:00");
        assert_eq!(candles[2][4], 95567.3);

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
                .contains("no Binance USDT perpetual for NOPE")
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
        }))));
        assert_eq!(out["as_of_bar_utc"], "2026-01-15 23:00");
        let levels = out["levels"].as_array().unwrap();
        assert!(levels.len() >= 20, "{levels:?}");
        let prices = levels
            .iter()
            .map(|level| level["price"].as_f64().unwrap())
            .collect::<Vec<_>>();
        assert!(prices.windows(2).all(|pair| pair[0] >= pair[1]));
        let labels = levels
            .iter()
            .map(|level| level["label"].as_str().unwrap())
            .collect::<Vec<_>>();
        for label in [
            "Weekly open",
            "Prev day high",
            "Monthly open",
            "Yearly open",
        ] {
            assert!(labels.contains(&label), "{label} missing from {labels:?}");
        }
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
}
