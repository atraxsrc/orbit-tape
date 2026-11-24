//! `orbit-tape scan`: a headless indicator scan.
//!
//! Fetches recent candles for a list of Binance USDT perpetuals, runs one
//! indicator over them as closed bars and reports every alert plot
//! (`plot(..., alert = true)`) that fired on the last closed bar(s). Prints a
//! table and, when anything fired, sends one desktop notification. Meant to be
//! run by hand or from a systemd user timer; no window is opened.

use exchange::adapter::{AdapterHandles, Exchange, MarketKind, Venue};
use exchange::{Kline, Ticker, TickerInfo, Timeframe, UnixMs};
use indicator_engine::{
    ChartFrame, CompiledIndicator, IndicatorConfig, IndicatorFrame, InputValue, InstrumentContext,
    TimeOrTick, Value, ValueType,
};
use std::path::{Path, PathBuf};

const DEFAULT_SYMBOLS: &[&str] = &[
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "BNBUSDT", "DOGEUSDT", "ADAUSDT", "TRXUSDT",
    "LINKUSDT", "LTCUSDT", "AVAXUSDT", "DOTUSDT", "BCHUSDT", "NEARUSDT", "SUIUSDT",
];
const DEFAULT_INDICATOR: &str = "roman-setup.ind";
const DEFAULT_BARS: usize = 2000;
const MAX_BARS: usize = 20_000;

pub const USAGE: &str = "\
usage: orbit-tape scan [--indicator <file.ind>] [--symbols <A,B,...>] [--timeframe <tf>]
                       [--bars <n>] [--lookback <n>] [--input <id>=<value> ...] [--no-notify]

Fetches candles for Binance USDT perpetuals, runs the indicator over them as
closed bars and lists the alert plots (plot(..., alert = true)) that fired on
the last closed bar. Sends one desktop notification when anything fired.

  --indicator   .ind file to run. Default: roman-setup.ind from the installed
                indicators folder, /usr/share/doc/orbit-tape/indicators/roman
                or ./indicators/roman
  --symbols     comma separated, e.g. BTCUSDT,ETHUSDT (default: 15 majors)
  --timeframe   1m 3m 5m 15m 30m 1h 2h 4h 12h 1d (default 1h)
  --bars        candles of history per symbol (default 2000)
  --lookback    report alerts from the last n closed bars (default 1)
  --input       indicator input by id or label, e.g. --input ctMode=true
  --no-notify   print only, no desktop notification

Exit status: 0 when the scan ran (with or without signals), 1 on errors.";

struct Arguments {
    indicator: Option<PathBuf>,
    symbols: Vec<String>,
    timeframe: Timeframe,
    bars: usize,
    lookback: usize,
    inputs: Vec<(String, String)>,
    notify: bool,
}

/// One alert plot that fired.
struct Signal {
    symbol: String,
    bar_time: i64,
    close: f64,
    title: String,
}

/// Runs the scan with the arguments after `scan` and returns the exit code.
pub fn run(arguments: &[String]) -> i32 {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return 0;
    }
    match scan(arguments) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("orbit-tape scan: {error}");
            1
        }
    }
}

fn scan(arguments: &[String]) -> Result<(), String> {
    let arguments = parse_arguments(arguments)?;
    let path = match &arguments.indicator {
        Some(path) => path.clone(),
        None => default_indicator().ok_or_else(|| {
            format!("could not find {DEFAULT_INDICATOR}; pass --indicator <file.ind>")
        })?,
    };
    let indicator = compile(&path)?;
    let config = indicator_config(&indicator, &arguments.inputs)?;
    let alert_plots = indicator
        .descriptor
        .plots
        .iter()
        .filter(|plot| plot.alert)
        .map(|plot| (plot.id.clone(), plot.label.clone()))
        .collect::<Vec<_>>();
    if alert_plots.is_empty() {
        return Err(format!(
            "{} has no alert plots (plot(..., alert = true))",
            path.display()
        ));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start the network runtime: {error}"))?;
    let rows = runtime.block_on(fetch_all(&arguments))?;

    println!(
        "{} on {} Binance perps, {} bars, last {} closed bar(s)",
        indicator.descriptor.title, arguments.timeframe, arguments.bars, arguments.lookback
    );
    let mut signals = Vec::new();
    let mut failures = 0;
    let mut table = vec![[
        "SYMBOL".to_owned(),
        "LAST CLOSED BAR".to_owned(),
        "CLOSE".to_owned(),
        "SIGNALS".to_owned(),
    ]];
    for (symbol, fetched) in rows {
        let (ticker_info, klines) = match fetched {
            Ok(fetched) => fetched,
            Err(error) => {
                failures += 1;
                table.push([symbol, "-".to_owned(), "-".to_owned(), error]);
                continue;
            }
        };
        match evaluate(
            &indicator,
            &config,
            &alert_plots,
            &symbol,
            ticker_info,
            &klines,
            arguments.lookback,
        ) {
            Ok(found) => {
                let last = klines.last().expect("evaluate rejects empty candles");
                let titles = found
                    .iter()
                    .map(|signal| {
                        if arguments.lookback > 1 {
                            format!("{} ({})", signal.title, local_time(signal.bar_time))
                        } else {
                            signal.title.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                table.push([
                    symbol,
                    local_time(last.time.as_u64() as i64),
                    format_price(last.close.to_f64()),
                    if titles.is_empty() {
                        "-".to_owned()
                    } else {
                        titles.join(", ")
                    },
                ]);
                signals.extend(found);
            }
            Err(error) => {
                failures += 1;
                table.push([symbol, "-".to_owned(), "-".to_owned(), error]);
            }
        }
    }
    print_table(&table);
    println!(
        "{} signal(s){}",
        signals.len(),
        if failures > 0 {
            format!(", {failures} symbol(s) failed")
        } else {
            String::new()
        }
    );

    if arguments.notify && !signals.is_empty() {
        notify(&indicator.descriptor.title, &signals);
    }
    if failures == table.len() - 1 {
        return Err("every symbol failed".to_owned());
    }
    Ok(())
}

fn parse_arguments(arguments: &[String]) -> Result<Arguments, String> {
    let mut parsed = Arguments {
        indicator: None,
        symbols: DEFAULT_SYMBOLS.iter().map(|&s| s.to_owned()).collect(),
        timeframe: Timeframe::H1,
        bars: DEFAULT_BARS,
        lookback: 1,
        inputs: Vec::new(),
        notify: true,
    };
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if argument == "--no-notify" {
            parsed.notify = false;
            index += 1;
            continue;
        }
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{argument} needs a value"))?;
        match argument {
            "--indicator" => parsed.indicator = Some(PathBuf::from(value)),
            "--symbols" => {
                parsed.symbols = value
                    .split(',')
                    .map(|symbol| symbol.trim().to_uppercase())
                    .filter(|symbol| !symbol.is_empty())
                    .collect();
                if parsed.symbols.is_empty() {
                    return Err("--symbols is empty".to_owned());
                }
            }
            "--timeframe" => {
                parsed.timeframe = Timeframe::KLINE
                    .iter()
                    .chain(&[Timeframe::H12, Timeframe::D1])
                    .copied()
                    .find(|timeframe| timeframe.to_string().eq_ignore_ascii_case(value))
                    .ok_or_else(|| format!("unknown --timeframe '{value}'"))?;
            }
            "--bars" => {
                parsed.bars = value
                    .parse()
                    .ok()
                    .filter(|bars| (2..=MAX_BARS).contains(bars))
                    .ok_or_else(|| format!("--bars must be 2 to {MAX_BARS}, got '{value}'"))?;
            }
            "--lookback" => {
                parsed.lookback = value
                    .parse()
                    .ok()
                    .filter(|bars| *bars >= 1)
                    .ok_or_else(|| format!("--lookback must be 1 or more, got '{value}'"))?;
            }
            "--input" => {
                let (id, input) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--input expects <id>=<value>, got '{value}'"))?;
                parsed
                    .inputs
                    .push((id.trim().to_owned(), input.trim().to_owned()));
            }
            _ => return Err(format!("unknown argument '{argument}' (see --help)")),
        }
        index += 2;
    }
    if parsed.lookback > parsed.bars {
        return Err("--lookback can't be larger than --bars".to_owned());
    }
    Ok(parsed)
}

/// The bundled Roman Setup script: the installed copy first, then the one the
/// .deb ships, then a source checkout.
fn default_indicator() -> Option<PathBuf> {
    let installed = std::env::var_os("ORBIT_TAPE_INDICATORS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data::data_path(Some("indicators")));
    find_file(&installed, DEFAULT_INDICATOR, 4)
        .or_else(|| {
            let path =
                Path::new("/usr/share/doc/orbit-tape/indicators/roman").join(DEFAULT_INDICATOR);
            path.is_file().then_some(path)
        })
        .or_else(|| {
            let path = Path::new("indicators/roman").join(DEFAULT_INDICATOR);
            path.is_file().then_some(path)
        })
}

fn find_file(directory: &Path, name: &str, depth: usize) -> Option<PathBuf> {
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

fn compile(path: &Path) -> Result<CompiledIndicator, String> {
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

fn indicator_config(
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

type Fetched = Result<(TickerInfo, Vec<Kline>), String>;

async fn fetch_all(arguments: &Arguments) -> Result<Vec<(String, Fetched)>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| format!("could not build the HTTP client: {error}"))?;
    let handles = AdapterHandles::spawn_venues(&client, [Venue::Binance], None);
    let metadata = handles
        .fetch_ticker_metadata(Venue::Binance, &[MarketKind::LinearPerps])
        .await
        .map_err(|error| format!("could not load Binance perp symbols: {error}"))?;

    let interval = arguments.timeframe.to_milliseconds();
    // Open time of the bar that is still forming; everything before it is closed.
    let forming = UnixMs::now().as_u64() / interval * interval;
    let from = forming.saturating_sub(interval * arguments.bars as u64);

    let mut rows = Vec::new();
    for symbol in &arguments.symbols {
        let ticker = Ticker::new(symbol, Exchange::BinanceLinear);
        let fetched = match metadata.get(&ticker).copied().flatten() {
            None => Err("not a Binance USDT perpetual".to_owned()),
            Some(ticker_info) => {
                fetch_klines(&handles, ticker_info, arguments.timeframe, from, forming)
                    .await
                    .map(|klines| (ticker_info, klines))
            }
        };
        rows.push((symbol.clone(), fetched));
    }
    Ok(rows)
}

/// Closed candles with open times in `[from, to)`, oldest first.
async fn fetch_klines(
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
    if klines.is_empty() {
        return Err("no candles".to_owned());
    }
    Ok(klines)
}

fn evaluate(
    indicator: &CompiledIndicator,
    config: &IndicatorConfig,
    alert_plots: &[(String, String)],
    symbol: &str,
    ticker_info: TickerInfo,
    klines: &[Kline],
    lookback: usize,
) -> Result<Vec<Signal>, String> {
    if klines.is_empty() {
        return Err("no candles".to_owned());
    }
    let mut instance = indicator
        .instantiate(config.clone())
        .map_err(|error| format!("indicator: {error}"))?;
    let first_reported = klines.len().saturating_sub(lookback);
    let mut signals = Vec::new();
    for (index, kline) in klines.iter().enumerate() {
        let time = kline.time.as_u64() as i64;
        let close = kline.close.to_f64();
        let frame = IndicatorFrame {
            index,
            time,
            basis: TimeOrTick::Time,
            instrument: InstrumentContext {
                min_tick: Some(ticker_info.min_ticksize.as_f64()),
            },
            chart: ChartFrame {
                open: Some(kline.open.to_f64()),
                high: Some(kline.high.to_f64()),
                low: Some(kline.low.to_f64()),
                close: Some(close),
                volume: Some(kline.volume.total().to_f64()),
                directional_volume_available: false,
                buy_qty: 0.0,
                sell_qty: 0.0,
                trade: None,
            },
            open_interest: None,
            series_inputs: Vec::new().into(),
        };
        let output = instance
            .evaluate_closed(frame)
            .map_err(|error| format!("bar {index} failed: {error}"))?;
        if index < first_reported {
            continue;
        }
        for (id, title) in alert_plots {
            let fired = output
                .plots
                .iter()
                .any(|plot| plot.id == *id && matches!(plot.value, Value::Bool(true)));
            if fired {
                signals.push(Signal {
                    symbol: symbol.to_owned(),
                    bar_time: time,
                    close,
                    title: title.clone(),
                });
            }
        }
    }
    Ok(signals)
}

fn local_time(time_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(time_ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%a %d %b %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn format_price(price: f64) -> String {
    let text = format!("{price:.8}");
    let text = text.trim_end_matches('0');
    text.trim_end_matches('.').to_owned()
}

fn print_table(rows: &[[String; 4]]) {
    let widths = (0..3)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    for row in rows {
        println!(
            "{:<w0$}  {:<w1$}  {:>w2$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
        );
    }
}

/// One notification for the whole scan, one line per signal.
fn notify(indicator: &str, signals: &[Signal]) {
    let summary = format!("Orbit Tape scan: {} signal(s)", signals.len());
    let mut body = signals
        .iter()
        .take(8)
        .map(|signal| {
            format!(
                "{} {} at {} ({})",
                signal.symbol,
                signal.title,
                format_price(signal.close),
                local_time(signal.bar_time)
            )
        })
        .collect::<Vec<_>>();
    if signals.len() > 8 {
        body.push(format!("and {} more", signals.len() - 8));
    }
    body.push(indicator.to_owned());
    if let Err(error) = notify_rust::Notification::new()
        .appname("Orbit Tape")
        .summary(&summary)
        .body(&body.join("\n"))
        .icon("appointment-soon")
        .timeout(notify_rust::Timeout::Milliseconds(30_000))
        .show()
    {
        eprintln!("orbit-tape scan: desktop notification failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_arguments() {
        let parsed = parse_arguments(&args(
            "--symbols btcusdt,ETHUSDT --timeframe 4h --bars 500 --lookback 3 --input ctMode=true --no-notify",
        ))
        .unwrap();
        assert_eq!(parsed.symbols, ["BTCUSDT", "ETHUSDT"]);
        assert_eq!(parsed.timeframe, Timeframe::H4);
        assert_eq!((parsed.bars, parsed.lookback), (500, 3));
        assert_eq!(parsed.inputs, [("ctMode".to_owned(), "true".to_owned())]);
        assert!(!parsed.notify);

        let defaults = parse_arguments(&[]).unwrap();
        assert_eq!(defaults.symbols.len(), DEFAULT_SYMBOLS.len());
        assert_eq!(defaults.timeframe, Timeframe::H1);
        assert!(defaults.notify);

        for bad in [
            "--timeframe 7h",
            "--bars 1",
            "--lookback 0",
            "--input ctMode",
            "--symbols ,",
            "--bogus 1",
            "--bars",
            "--bars 10 --lookback 11",
        ] {
            assert!(parse_arguments(&args(bad)).is_err(), "{bad}");
        }
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
        let alert_plots = vec![("up".to_owned(), "Up bar".to_owned())];
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
            evaluate(
                &indicator,
                &config,
                &alert_plots,
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
