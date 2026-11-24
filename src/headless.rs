//! Shared code for the windowless commands (`orbit-tape scan`, `orbit-tape mcp`):
//! USDT perpetual symbols on Binance and OKX, closed candles, compiling `.ind`
//! files and running them over candles bar by bar.

use exchange::adapter::{AdapterHandles, Exchange, MarketKind, Venue};
use exchange::{Kline, Ticker, TickerInfo, Timeframe, UnixMs};
use indicator_engine::{
    ChartFrame, CompiledIndicator, FrameOutput, IndicatorConfig, IndicatorFrame, InputValue,
    InstrumentContext, TimeOrTick, Value, ValueType,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_INDICATOR: &str = "roman-setup.ind";
/// Most candles one request may ask for.
pub const MAX_BARS: usize = 20_000;

/// Where the candles come from: USDT-margined perpetuals of one exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PerpExchange {
    Binance,
    Okx,
}

impl PerpExchange {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "binance" => Some(Self::Binance),
            "okx" | "okex" => Some(Self::Okx),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Binance => "Binance",
            Self::Okx => "OKX",
        }
    }

    fn venue(self) -> Venue {
        match self {
            Self::Binance => Venue::Binance,
            Self::Okx => Venue::Okex,
        }
    }

    fn exchange(self) -> Exchange {
        match self {
            Self::Binance => Exchange::BinanceLinear,
            Self::Okx => Exchange::OkexLinear,
        }
    }

    /// Instrument names to try for a coin, most likely first.
    pub fn instruments(self, coin: &str) -> Vec<String> {
        match self {
            Self::Binance => vec![
                format!("{coin}USDT"),
                format!("1000{coin}USDT"),
                format!("1000000{coin}USDT"),
            ],
            Self::Okx => vec![format!("{coin}-USDT-SWAP")],
        }
    }
}

/// The coin in a symbol as users write it: `BTC`, `BTCUSDT`, `1000PEPEUSDT`
/// or `BTC-USDT-SWAP` all name a coin (`BTC`, `PEPE`).
pub fn coin(symbol: &str) -> String {
    let symbol = symbol.trim().to_uppercase();
    let symbol = symbol
        .strip_suffix("-USDT-SWAP")
        .or_else(|| symbol.strip_suffix("-USDT"))
        .or_else(|| symbol.strip_suffix("USDT").filter(|rest| !rest.is_empty()))
        .unwrap_or(&symbol);
    ["1000000", "1000"]
        .iter()
        .find_map(|prefix| {
            symbol
                .strip_prefix(prefix)
                .filter(|rest| rest.starts_with(|c: char| c.is_ascii_alphabetic()))
        })
        .unwrap_or(symbol)
        .to_owned()
}

/// Timeframes the exchanges serve as candles: the chart ones plus 1w.
pub fn timeframes() -> impl Iterator<Item = Timeframe> {
    Timeframe::KLINE.into_iter().chain([Timeframe::W1])
}

pub fn parse_timeframe(text: &str) -> Result<Timeframe, String> {
    timeframes()
        .find(|timeframe| timeframe.to_string().eq_ignore_ascii_case(text.trim()))
        .ok_or_else(|| {
            let known = timeframes()
                .map(|timeframe| timeframe.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown timeframe '{text}' (use one of {known})")
        })
}

/// One instrument's closed candles, oldest first.
pub struct Series {
    /// The exchange's name for it, e.g. `BTCUSDT` or `BTC-USDT-SWAP`.
    pub instrument: String,
    pub ticker_info: TickerInfo,
    pub klines: Vec<Kline>,
}

/// One exchange's perpetuals: the symbol list (loaded once) and the adapter
/// that fetches candles.
pub struct Market {
    exchange: PerpExchange,
    handles: AdapterHandles,
    metadata: HashMap<Ticker, Option<TickerInfo>>,
}

impl Market {
    pub async fn connect(exchange: PerpExchange) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| format!("could not build the HTTP client: {error}"))?;
        let handles = AdapterHandles::spawn_venues(&client, [exchange.venue()], None);
        let metadata = handles
            .fetch_ticker_metadata(exchange.venue(), &[MarketKind::LinearPerps])
            .await
            .map_err(|error| format!("could not load {} perp symbols: {error}", exchange.name()))?;
        Ok(Self {
            exchange,
            handles,
            metadata,
        })
    }

    /// The listed instrument for a coin, if the exchange has a USDT perpetual.
    pub fn resolve(&self, coin: &str) -> Option<(String, TickerInfo)> {
        self.exchange
            .instruments(coin)
            .into_iter()
            .find_map(|instrument| {
                self.metadata
                    .get(&Ticker::new(&instrument, self.exchange.exchange()))
                    .copied()
                    .flatten()
                    .map(|ticker_info| (instrument, ticker_info))
            })
    }

    /// The last `bars` closed candles of a coin.
    pub async fn closed_candles(
        &self,
        coin: &str,
        timeframe: Timeframe,
        bars: usize,
    ) -> Result<Series, String> {
        let (instrument, ticker_info) = self
            .resolve(coin)
            .ok_or_else(|| format!("no {} USDT perpetual for {coin}", self.exchange.name()))?;
        let interval = timeframe.to_milliseconds();
        // Open time of the bar that is still forming; everything before it is closed.
        let forming = UnixMs::now().floor_to(timeframe).as_u64();
        let from = forming.saturating_sub(interval * bars as u64);
        let klines = fetch_klines(&self.handles, ticker_info, timeframe, from, forming).await?;
        Ok(Series {
            instrument,
            ticker_info,
            klines,
        })
    }
}

/// Closed candles with open times in `[from, to)`, oldest first; at least one.
async fn fetch_klines(
    handles: &AdapterHandles,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    from: u64,
    to: u64,
) -> Result<Vec<Kline>, String> {
    let klines = fetch_kline_range(handles, ticker_info, timeframe, from, to).await?;
    if klines.is_empty() {
        return Err("no candles".to_owned());
    }
    Ok(klines)
}

/// Candles with open times in `[from, to)`, oldest first; maybe none.
pub async fn fetch_kline_range(
    handles: &AdapterHandles,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    from: u64,
    to: u64,
) -> Result<Vec<Kline>, String> {
    let mut klines: Vec<Kline> = Vec::new();
    let mut next = from;
    while next < to {
        let page = handles
            .fetch_klines_page(ticker_info, timeframe, UnixMs::new(next), UnixMs::new(to))
            .await
            .map_err(|error| format!("candles: {error}"))?;
        if page.next_from.as_u64() <= next {
            break;
        }
        next = page.next_from.as_u64();
        klines.extend(page.data);
    }
    klines.retain(|kline| kline.time.as_u64() < to);
    klines.sort_unstable_by_key(|kline| kline.time);
    klines.dedup_by_key(|kline| kline.time);
    Ok(klines)
}

/// The installed indicator with this ID (`namespace.name`), searching
/// `folders` in order. Compiled fresh, so edits to the file count.
pub fn find_indicator_by_id(
    folders: &[PathBuf],
    id: &str,
) -> Result<(PathBuf, CompiledIndicator), String> {
    for folder in folders {
        let Ok((paths, _)) = indicator_engine::source_files_in_directory(folder) else {
            continue;
        };
        for path in paths {
            let Ok(indicator) = compile(&path) else {
                continue;
            };
            if indicator.descriptor.id.as_str() == id {
                return Ok((path, indicator));
            }
        }
    }
    Err(format!("indicator {id} is not installed"))
}

/// The installed indicators folder (`ORBIT_TAPE_INDICATORS_PATH` overrides it).
pub fn indicators_folder() -> PathBuf {
    std::env::var_os("ORBIT_TAPE_INDICATORS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data::data_path(Some("indicators")))
}

/// Folders searched for indicators by file name, in order: the installed
/// folder, the copies the .deb ships, then a source checkout.
pub fn indicator_search_folders() -> Vec<PathBuf> {
    vec![
        indicators_folder(),
        PathBuf::from("/usr/share/doc/orbit-tape/indicators"),
        PathBuf::from("indicators"),
    ]
}

/// The bundled Roman Setup script: the installed copy first, then the one the
/// .deb ships, then a source checkout.
pub fn default_indicator() -> Option<PathBuf> {
    indicator_search_folders()
        .iter()
        .find_map(|folder| find_file(folder, DEFAULT_INDICATOR, 4))
}

pub fn find_file(directory: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let mut entries = std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    if let Some(found) = entries
        .iter()
        .find(|path| path.is_file() && path.file_name().is_some_and(|file| file == name))
    {
        return Some(found.clone());
    }
    if depth == 0 {
        return None;
    }
    entries
        .iter()
        .filter(|path| path.is_dir())
        .find_map(|path| find_file(path, name, depth - 1))
}

pub fn compile(path: &Path) -> Result<CompiledIndicator, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    indicator_engine::compile(&source, path).map_err(|diagnostics| {
        let first = diagnostics.first().map_or_else(String::new, |diagnostic| {
            format!(": {}", diagnostic.message)
        });
        format!(
            "{} has {} error(s){first} (run orbit-tape-indicator check for details)",
            path.display(),
            diagnostics.len()
        )
    })
}

/// Inputs by id or label, values as text (`true`, `14`, `0.5`, `Close`).
pub fn indicator_config(
    indicator: &CompiledIndicator,
    inputs: &[(String, String)],
) -> Result<IndicatorConfig, String> {
    let mut config = IndicatorConfig::default();
    for (id, text) in inputs {
        let descriptor = indicator
            .descriptor
            .inputs
            .iter()
            .find(|input| input.id == *id || input.label == *id)
            .ok_or_else(|| format!("unknown input '{id}'"))?;
        let value = parse_input(descriptor.value_type, text).ok_or_else(|| {
            format!(
                "invalid value '{text}' for input '{}' ({:?})",
                descriptor.id, descriptor.value_type
            )
        })?;
        config = config.with(descriptor.id.clone(), value);
    }
    Ok(config)
}

fn parse_input(value_type: ValueType, text: &str) -> Option<InputValue> {
    Some(match value_type {
        ValueType::Int => InputValue::Int(text.parse().ok()?),
        ValueType::Time => InputValue::Time(text.parse().ok()?),
        ValueType::Float => InputValue::Float(text.parse().ok()?),
        ValueType::Bool => InputValue::Bool(match text {
            "true" => true,
            "false" => false,
            _ => return None,
        }),
        ValueType::String => InputValue::String(text.to_owned()),
        _ => return None,
    })
}

/// The engine frame for closed candle `index`.
pub fn frame(index: usize, kline: &Kline, ticker_info: TickerInfo) -> IndicatorFrame {
    IndicatorFrame {
        index,
        time: kline.time.as_u64() as i64,
        basis: TimeOrTick::Time,
        instrument: InstrumentContext {
            min_tick: Some(ticker_info.min_ticksize.as_f64()),
        },
        chart: ChartFrame {
            open: Some(kline.open.to_f64()),
            high: Some(kline.high.to_f64()),
            low: Some(kline.low.to_f64()),
            close: Some(kline.close.to_f64()),
            volume: Some(kline.volume.total().to_f64()),
            directional_volume_available: false,
            buy_qty: 0.0,
            sell_qty: 0.0,
            trade: None,
        },
        open_interest: None,
        series_inputs: Vec::new().into(),
    }
}

/// Runs the indicator over every candle as a closed bar and hands each
/// bar's output to `visit` (index, output).
pub fn run_closed(
    indicator: &CompiledIndicator,
    config: &IndicatorConfig,
    ticker_info: TickerInfo,
    klines: &[Kline],
    mut visit: impl FnMut(usize, &FrameOutput),
) -> Result<(), String> {
    if klines.is_empty() {
        return Err("no candles".to_owned());
    }
    let mut instance = indicator
        .instantiate(config.clone())
        .map_err(|error| format!("indicator: {error}"))?;
    for (index, kline) in klines.iter().enumerate() {
        let output = instance
            .evaluate_closed(frame(index, kline, ticker_info))
            .map_err(|error| format!("bar {index} failed: {error}"))?;
        visit(index, &output);
    }
    Ok(())
}

/// One alert plot that fired.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub symbol: String,
    pub bar_time: i64,
    pub title: String,
}

/// The alert plots (`plot(..., alert = true)`): id and title.
pub fn alert_plots(indicator: &CompiledIndicator) -> Vec<(String, String)> {
    indicator
        .descriptor
        .plots
        .iter()
        .filter(|plot| plot.alert)
        .map(|plot| (plot.id.clone(), plot.label.clone()))
        .collect()
}

/// Alerts that fired on the last `lookback` closed bars.
pub fn evaluate_alerts(
    indicator: &CompiledIndicator,
    config: &IndicatorConfig,
    alert_plots: &[(String, String)],
    symbol: &str,
    ticker_info: TickerInfo,
    klines: &[Kline],
    lookback: usize,
) -> Result<Vec<Signal>, String> {
    let first_reported = klines.len().saturating_sub(lookback);
    let mut signals = Vec::new();
    run_closed(indicator, config, ticker_info, klines, |index, output| {
        if index < first_reported {
            return;
        }
        for (id, title) in alert_plots {
            let fired = output
                .plots
                .iter()
                .any(|plot| plot.id == *id && matches!(plot.value, Value::Bool(true)));
            if fired {
                signals.push(Signal {
                    symbol: symbol.to_owned(),
                    bar_time: output.time,
                    title: title.clone(),
                });
            }
        }
    })?;
    Ok(signals)
}

pub fn local_time(time_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(time_ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%a %d %b %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn format_price(price: f64) -> String {
    let text = format!("{price:.8}");
    let text = text.trim_end_matches('0');
    text.trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_coins_and_timeframes() {
        for (symbol, expected) in [
            ("btc", "BTC"),
            ("BTCUSDT", "BTC"),
            ("BTC-USDT-SWAP", "BTC"),
            ("1000PEPEUSDT", "PEPE"),
            ("1000000MOGUSDT", "MOG"),
            ("SUSDT", "S"),
            ("1INCHUSDT", "1INCH"),
        ] {
            assert_eq!(coin(symbol), expected, "{symbol}");
        }
        assert_eq!(
            PerpExchange::Binance.instruments("PEPE")[..2],
            ["PEPEUSDT".to_owned(), "1000PEPEUSDT".to_owned()]
        );
        assert_eq!(PerpExchange::Okx.instruments("PEPE"), ["PEPE-USDT-SWAP"]);
        assert_eq!(PerpExchange::parse(" OKEX "), Some(PerpExchange::Okx));
        assert_eq!(PerpExchange::parse("bybit"), None);
        assert_eq!(parse_timeframe("4H").unwrap(), Timeframe::H4);
        assert_eq!(parse_timeframe("1d").unwrap(), Timeframe::D1);
        assert_eq!(parse_timeframe("1W").unwrap(), Timeframe::W1);
        assert_eq!(
            parse_timeframe("7h").unwrap_err(),
            "unknown timeframe '7h' (use one of 1m, 3m, 5m, 15m, 30m, 1h, 2h, 4h, 12h, 1d, 1w)"
        );
    }

    #[test]
    fn reports_alerts_on_the_last_closed_bars() {
        let indicator = indicator_engine::compile(
            "language(1)\nindicator(\"Up bars\", id = \"test.up\")\n\
             format(precision = \"base_ticker_min_tick\")\n\
             plot(chart.close > chart.open, id = \"up\", title = \"Up bar\", alert = true)\n",
            "up.ind",
        )
        .unwrap();
        let plots = alert_plots(&indicator);
        assert_eq!(plots, [("up".to_owned(), "Up bar".to_owned())]);
        let ticker_info = TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            0.1,
            0.001,
            None,
        );
        let kline = |hour: u64, open: f64, close: f64| {
            Kline::new(
                hour * 3_600_000,
                open,
                open.max(close),
                open.min(close),
                close,
                exchange::Volume::TotalOnly(exchange::unit::Qty::from_f64(1.0)),
                ticker_info.min_ticksize,
            )
        };
        // up, down, up
        let klines = [kline(0, 1.0, 2.0), kline(1, 2.0, 1.0), kline(2, 1.0, 3.0)];
        let config = IndicatorConfig::default();
        let run = |lookback| {
            evaluate_alerts(
                &indicator,
                &config,
                &plots,
                "BTCUSDT",
                ticker_info,
                &klines,
                lookback,
            )
            .unwrap()
            .iter()
            .map(|signal| signal.bar_time / 3_600_000)
            .collect::<Vec<_>>()
        };
        assert_eq!(run(1), [2]);
        assert_eq!(run(2), [2]);
        assert_eq!(run(3), [0, 2]);
    }
}
