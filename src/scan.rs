//! `orbit-tape scan`: a headless indicator scan.
//!
//! Fetches recent candles for a list of USDT perpetuals (Binance or OKX), runs one
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

/// Coins written to the symbols file the first time the scan runs without one.
const DEFAULT_SYMBOLS: &[&str] = &[
    "AAVE", "ADA", "AIXBT", "ALGO", "APT", "ARB", "ASTER", "ATOM", "AVAX", "BCH", "BNB", "BONK",
    "BTC", "CRV", "DOGE", "DOT", "ENA", "ETC", "ETH", "ETHFI", "FARTCOIN", "FIL", "FLOKI", "GRASS",
    "HBAR", "HYPE", "ICP", "INJ", "JTO", "JUP", "KAITO", "LDO", "LIT", "LINK", "LTC", "MON",
    "MOODENG", "NEAR", "ONDO", "OP", "PENDLE", "PENGU", "PEPE", "PNUT", "POL", "POPCAT", "PUMP",
    "RENDER", "S", "SHIB", "SOL", "STX", "SUI", "TAO", "TIA", "TRUMP", "TRX", "UNI", "VIRTUAL",
    "WIF", "WLD", "XLM", "XPL", "XRP", "ZEC", "ZRO",
];
const SYMBOLS_FILE: &str = "scan-symbols.txt";
const SYMBOLS_FILE_HEADER: &str = "\
# Coins for `orbit-tape scan`, one per line, scanned as USDT perpetuals.
# Lines starting with # are ignored. Exchange: binance or okx.
# Binance's 1000x contracts (1000PEPEUSDT) are found automatically.
# Delete this file to get the default list back.
exchange = okx
";
/// Notification lines before the rest is summed up as "and n more".
const NOTIFY_MAX_LINES: usize = 10;
/// Symbols per notification line before "+n".
const NOTIFY_MAX_SYMBOLS: usize = 12;
const DEFAULT_INDICATOR: &str = "roman-setup.ind";
const DEFAULT_BARS: usize = 2000;
const MAX_BARS: usize = 20_000;

pub const USAGE: &str = "\
usage: orbit-tape scan [--exchange binance|okx] [--indicator <file.ind>] [--symbols <A,B,...>]
                       [--symbols-file <file>] [--timeframe <tf>] [--bars <n>] [--lookback <n>] [--input <id>=<value> ...] [--no-notify]

Fetches candles for USDT perpetuals on Binance or OKX, runs the indicator over them as
closed bars and lists the alert plots (plot(..., alert = true)) that fired on
the last closed bar. Sends one desktop notification when anything fired.

  --indicator   .ind file to run. Default: roman-setup.ind from the installed
                indicators folder, /usr/share/doc/orbit-tape/indicators/roman
                or ./indicators/roman
  --exchange    binance or okx. Default: the `exchange =` line of the
                symbols file, else okx
  --symbols     coins, comma separated, e.g. BTC,ETH (BTCUSDT and
                BTC-USDT-SWAP work too)
  --symbols-file
                file with one coin per line (# for comments) and an optional
                `exchange = okx` line. Default: scan-symbols.txt in the data
                folder, created with 66 coins on the first run
  --timeframe   1m 3m 5m 15m 30m 1h 2h 4h 12h 1d (default 1h)
  --bars        candles of history per symbol (default 2000)
  --lookback    report alerts from the last n closed bars (default 1)
  --input       indicator input by id or label, e.g. --input ctMode=true
  --no-notify   print only, no desktop notification

Exit status: 0 when the scan ran (with or without signals), 1 on errors.";

struct Arguments {
    indicator: Option<PathBuf>,
    /// From `--symbols`; otherwise the symbols file is read.
    symbols: Option<Vec<String>>,
    symbols_file: Option<PathBuf>,
    exchange: Option<ScanExchange>,
    timeframe: Timeframe,
    bars: usize,
    lookback: usize,
    inputs: Vec<(String, String)>,
    notify: bool,
}

/// Where the candles come from: USDT-margined perpetuals of one exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanExchange {
    Binance,
    Okx,
}

impl ScanExchange {
    fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "binance" => Some(Self::Binance),
            "okx" | "okex" => Some(Self::Okx),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
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
    fn instruments(self, coin: &str) -> Vec<String> {
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
fn coin(symbol: &str) -> String {
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

/// One alert plot that fired.
struct Signal {
    symbol: String,
    bar_time: i64,
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
    let (file_exchange, symbols) = match &arguments.symbols {
        Some(symbols) => (None, symbols.clone()),
        None => symbols_from_file(arguments.symbols_file.as_deref())?,
    };
    let exchange = arguments
        .exchange
        .or(file_exchange)
        .unwrap_or(ScanExchange::Okx);
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

    eprintln!(
        "Scanning {} pairs on {}, this takes a few minutes for a long list...",
        symbols.len(),
        exchange.name()
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start the network runtime: {error}"))?;
    let rows = runtime.block_on(fetch_all(&arguments, exchange, &symbols))?;

    println!(
        "{} on {} {} perps, {} chart, {} bars, last {} closed bar(s)",
        indicator.descriptor.title,
        symbols.len(),
        exchange.name(),
        arguments.timeframe,
        arguments.bars,
        arguments.lookback
    );
    let mut signals = Vec::new();
    let mut failures = 0;
    let mut table = vec![[
        "SYMBOL".to_owned(),
        "LAST CLOSED BAR".to_owned(),
        "CLOSE".to_owned(),
        "SIGNALS".to_owned(),
    ]];
    for (coin, symbol, fetched) in rows {
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
            &coin,
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
        symbols: None,
        symbols_file: None,
        exchange: None,
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
                let (_, symbols) = parse_symbols(&value.replace('=', " "))?;
                if symbols.is_empty() {
                    return Err("--symbols is empty".to_owned());
                }
                parsed.symbols = Some(symbols);
            }
            "--symbols-file" => parsed.symbols_file = Some(PathBuf::from(value)),
            "--exchange" => {
                parsed.exchange = Some(
                    ScanExchange::parse(value)
                        .ok_or_else(|| format!("unknown --exchange '{value}' (binance or okx)"))?,
                );
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
    if parsed.symbols.is_some() && parsed.symbols_file.is_some() {
        return Err("use either --symbols or --symbols-file".to_owned());
    }
    if parsed.lookback > parsed.bars {
        return Err("--lookback can't be larger than --bars".to_owned());
    }
    Ok(parsed)
}

/// Coins separated by commas, spaces or new lines, `#` starting a comment,
/// plus an optional `exchange = binance|okx` line. Symbols are reduced to
/// their coin ([`coin`]); duplicates are dropped, order kept.
fn parse_symbols(text: &str) -> Result<(Option<ScanExchange>, Vec<String>), String> {
    let mut exchange = None;
    let mut coins = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        if let Some((key, value)) = line.split_once('=') {
            if !key.trim().eq_ignore_ascii_case("exchange") {
                return Err(format!("unknown setting '{}'", key.trim()));
            }
            exchange =
                Some(ScanExchange::parse(value).ok_or_else(|| {
                    format!("unknown exchange '{}' (binance or okx)", value.trim())
                })?);
            continue;
        }
        for symbol in line.split(|c: char| c == ',' || c.is_whitespace()) {
            let coin = coin(symbol);
            if !coin.is_empty() && !coins.contains(&coin) {
                coins.push(coin);
            }
        }
    }
    Ok((exchange, coins))
}

/// Reads the symbols file. The default one is created with [`DEFAULT_SYMBOLS`]
/// when it does not exist yet; a file passed with `--symbols-file` must exist.
fn symbols_from_file(
    explicit: Option<&Path>,
) -> Result<(Option<ScanExchange>, Vec<String>), String> {
    let path = explicit.map_or_else(|| data::data_path(Some(SYMBOLS_FILE)), Path::to_path_buf);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (exchange, symbols) =
                parse_symbols(&text).map_err(|error| format!("{}: {error}", path.display()))?;
            if symbols.is_empty() {
                return Err(format!("{} lists no coins", path.display()));
            }
            Ok((exchange, symbols))
        }
        Err(error) if explicit.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
            let text = format!("{SYMBOLS_FILE_HEADER}{}\n", DEFAULT_SYMBOLS.join("\n"));
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, text));
            match written {
                Ok(()) => println!(
                    "Created {} with {} coins, edit it to change the list.",
                    path.display(),
                    DEFAULT_SYMBOLS.len()
                ),
                Err(error) => eprintln!(
                    "orbit-tape scan: could not create {}: {error}",
                    path.display()
                ),
            }
            Ok((
                None,
                DEFAULT_SYMBOLS
                    .iter()
                    .map(|&symbol| symbol.to_owned())
                    .collect(),
            ))
        }
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
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

/// Per coin: the coin, the exchange instrument (or the coin when it is not
/// listed) and its candles.
type FetchedRow = (String, String, Fetched);

async fn fetch_all(
    arguments: &Arguments,
    exchange: ScanExchange,
    coins: &[String],
) -> Result<Vec<FetchedRow>, String> {
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

    let interval = arguments.timeframe.to_milliseconds();
    // Open time of the bar that is still forming; everything before it is closed.
    let forming = UnixMs::now().as_u64() / interval * interval;
    let from = forming.saturating_sub(interval * arguments.bars as u64);

    let mut rows = Vec::new();
    // Progress on the terminal only, so the journal of a timer run stays clean.
    let progress = std::io::IsTerminal::is_terminal(&std::io::stderr());
    for (index, coin) in coins.iter().enumerate() {
        if progress {
            eprint!(
                "\rFetching {coin:<16} {:>3}/{}\u{1b}[K",
                index + 1,
                coins.len()
            );
        }
        let listed = exchange
            .instruments(coin)
            .into_iter()
            .find_map(|instrument| {
                metadata
                    .get(&Ticker::new(&instrument, exchange.exchange()))
                    .copied()
                    .flatten()
                    .map(|ticker_info| (instrument, ticker_info))
            });
        let row = match listed {
            None => (
                coin.clone(),
                coin.clone(),
                Err(format!("no {} USDT perpetual", exchange.name())),
            ),
            Some((instrument, ticker_info)) => {
                let fetched =
                    fetch_klines(&handles, ticker_info, arguments.timeframe, from, forming)
                        .await
                        .map(|klines| (ticker_info, klines));
                (coin.clone(), instrument, fetched)
            }
        };
        rows.push(row);
    }
    if progress {
        eprint!("\r\u{1b}[K");
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

/// Summary and body of the scan notification: one line per signal type and
/// bar, newest bar first, e.g. "LTF MSS down Tue 13:00: BTC, BNB, SOL".
fn notification_text(indicator: &str, signals: &[Signal]) -> (String, String) {
    let mut groups: Vec<(i64, &str, Vec<&str>)> = Vec::new();
    for signal in signals {
        let short = signal.symbol.as_str();
        match groups
            .iter_mut()
            .find(|(time, title, _)| *time == signal.bar_time && *title == signal.title)
        {
            Some((_, _, symbols)) => symbols.push(short),
            None => groups.push((signal.bar_time, &signal.title, vec![short])),
        }
    }
    // Newest bar first; the sort is stable, so titles keep the indicator's order.
    groups.sort_by_key(|(time, _, _)| std::cmp::Reverse(*time));

    let mut lines = groups
        .iter()
        .take(NOTIFY_MAX_LINES)
        .map(|(time, title, symbols)| {
            let mut names = symbols
                .iter()
                .take(NOTIFY_MAX_SYMBOLS)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            if symbols.len() > NOTIFY_MAX_SYMBOLS {
                names.push_str(&format!(" +{}", symbols.len() - NOTIFY_MAX_SYMBOLS));
            }
            format!("{title} {}: {names}", short_time(*time))
        })
        .collect::<Vec<_>>();
    if groups.len() > NOTIFY_MAX_LINES {
        let rest = groups[NOTIFY_MAX_LINES..]
            .iter()
            .map(|(_, _, symbols)| symbols.len())
            .sum::<usize>();
        lines.push(format!("and {rest} more, see the terminal or journal"));
    }
    lines.push(indicator.to_owned());
    (
        format!("Orbit Tape scan: {} signal(s)", signals.len()),
        lines.join("\n"),
    )
}

fn short_time(time_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(time_ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%a %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// One desktop notification for the whole scan.
fn notify(indicator: &str, signals: &[Signal]) {
    let (summary, body) = notification_text(indicator, signals);
    if let Err(error) = notify_rust::Notification::new()
        .appname("Orbit Tape")
        .summary(&summary)
        .body(&body)
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
        assert_eq!(
            parsed.symbols.as_deref(),
            Some(&["BTC".to_owned(), "ETH".to_owned()][..])
        );
        assert_eq!(parsed.exchange, None);
        let okx = parse_arguments(&args("--exchange OKX")).unwrap();
        assert_eq!(okx.exchange, Some(ScanExchange::Okx));
        assert_eq!(parsed.timeframe, Timeframe::H4);
        assert_eq!((parsed.bars, parsed.lookback), (500, 3));
        assert_eq!(parsed.inputs, [("ctMode".to_owned(), "true".to_owned())]);
        assert!(!parsed.notify);

        let defaults = parse_arguments(&[]).unwrap();
        assert!(defaults.symbols.is_none() && defaults.symbols_file.is_none());
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
            "--symbols BTCUSDT --symbols-file list.txt",
            "--exchange bybit",
        ] {
            assert!(parse_arguments(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_symbol_lists() {
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
            parse_symbols("# header\nexchange = OKX\nbtcusdt\n ETH, sol # majors\n\nBTC\n")
                .unwrap(),
            (
                Some(ScanExchange::Okx),
                vec!["BTC".to_owned(), "ETH".to_owned(), "SOL".to_owned()]
            )
        );
        assert!(parse_symbols("exchange = kraken\nBTC").is_err());
        assert!(parse_symbols("bars = 10\nBTC").is_err());

        assert_eq!(DEFAULT_SYMBOLS.len(), 66);
        let written = format!("{SYMBOLS_FILE_HEADER}{}\n", DEFAULT_SYMBOLS.join("\n"));
        assert_eq!(
            parse_symbols(&written).unwrap(),
            (
                Some(ScanExchange::Okx),
                DEFAULT_SYMBOLS.iter().map(|s| s.to_string()).collect()
            )
        );
        assert_eq!(
            ScanExchange::Binance.instruments("PEPE")[..2],
            ["PEPEUSDT".to_owned(), "1000PEPEUSDT".to_owned()]
        );
        assert_eq!(ScanExchange::Okx.instruments("PEPE"), ["PEPE-USDT-SWAP"]);
    }

    #[test]
    fn groups_the_notification_by_signal_and_bar() {
        let signal = |symbol: &str, hour: i64, title: &str| Signal {
            symbol: symbol.to_owned(),
            bar_time: hour * 3_600_000,
            title: title.to_owned(),
        };
        let signals = [
            signal("BTC", 13, "LTF MSS down"),
            signal("ETH", 9, "LTF MSS down"),
            signal("SOL", 11, "HTF Bearish SFP"),
            signal("SOL", 13, "LTF MSS down"),
            signal("PEPE", 13, "LTF MSS down"),
        ];
        let (summary, body) = notification_text("Roman", &signals);
        assert_eq!(summary, "Orbit Tape scan: 5 signal(s)");
        let lines = body.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 4, "{body}");
        assert!(lines[0].starts_with("LTF MSS down ") && lines[0].ends_with(": BTC, SOL, PEPE"));
        assert!(lines[1].starts_with("HTF Bearish SFP ") && lines[1].ends_with(": SOL"));
        assert!(lines[2].ends_with(": ETH"));
        assert_eq!(lines[3], "Roman");

        let many = (0..20)
            .map(|index| signal(&format!("C{index}"), index, "MSS"))
            .chain((0..15).map(|index| signal(&format!("D{index}"), 100, "SFP")))
            .collect::<Vec<_>>();
        let (_, body) = notification_text("Roman", &many);
        let lines = body.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), NOTIFY_MAX_LINES + 2);
        assert!(
            lines[0].starts_with("SFP ") && lines[0].ends_with("D11 +3"),
            "{}",
            lines[0]
        );
        assert_eq!(
            lines[NOTIFY_MAX_LINES],
            "and 11 more, see the terminal or journal"
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
