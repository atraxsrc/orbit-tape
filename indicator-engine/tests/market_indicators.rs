//! Regression tests for the scripts in `indicators/market`.
//!
//! Key Levels is checked bar for bar against levels computed from Binance's
//! own 1d, 1w and 1M candles (quarters and years aggregated from months) on
//! 5496 BTCUSDT perpetual 1H bars, 2025-06-01 to 2026-01-15. That window
//! covers two quarter starts and a year start, and its first week and quarter
//! begin before the history does, so the "incomplete candle" rules are
//! checked too. The Bull Market Support Band is checked against values
//! exported from the original Pine script on TradingView (BINANCE:BTCUSDT.P,
//! 1H chart, 100 weeks), run here on the exchange's weekly candles. The
//! SMAs are checked against a direct calculation.

use chrono::{Datelike, TimeZone, Utc};
use orbit_tape_indicator_engine::{
    ChartFrame, FrameOutput, IndicatorConfig, IndicatorFrame, InputValue, InstrumentContext,
    TimeOrTick, Value, compile,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../indicators/market")
        .join(name)
}

fn read_csv(name: &str) -> Vec<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(fixture(name)).expect("fixture should exist");
    let mut lines = text.lines();
    let header = lines
        .next()
        .expect("fixture has a header")
        .split(',')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            header
                .iter()
                .cloned()
                .zip(line.split(',').map(str::to_owned))
                .collect()
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Candle {
    time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

fn candles(name: &str) -> Vec<Candle> {
    let number = |row: &BTreeMap<String, String>, field: &str| -> f64 {
        row[field].parse().expect("candle values are numbers")
    };
    read_csv(name)
        .iter()
        .map(|row| Candle {
            time: row["time"].parse().expect("times are integers"),
            open: number(row, "open"),
            high: number(row, "high"),
            low: number(row, "low"),
            close: number(row, "close"),
        })
        .collect()
}

fn by_time(candles: Vec<Candle>) -> BTreeMap<i64, Candle> {
    candles
        .into_iter()
        .map(|candle| (candle.time, candle))
        .collect()
}

/// Runs a script over candles and returns each bar's output, in order.
fn run(script: &str, bars: &[Candle], config: IndicatorConfig) -> Vec<(i64, FrameOutput)> {
    let source = std::fs::read_to_string(fixture(script)).expect("script should exist");
    let mut instance = compile(&source, script)
        .unwrap_or_else(|diagnostics| panic!("{script} should compile: {diagnostics:?}"))
        .instantiate(config)
        .expect("inputs should bind");
    bars.iter()
        .enumerate()
        .map(|(index, bar)| {
            let frame = IndicatorFrame {
                index,
                time: bar.time,
                basis: TimeOrTick::Time,
                instrument: InstrumentContext {
                    min_tick: Some(0.1),
                },
                chart: ChartFrame {
                    open: Some(bar.open),
                    high: Some(bar.high),
                    low: Some(bar.low),
                    close: Some(bar.close),
                    volume: Some(1.0),
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
                .unwrap_or_else(|error| panic!("{script} bar {index} failed: {error}"));
            (bar.time, output)
        })
        .collect()
}

fn plot(output: &FrameOutput, id: &str) -> Option<f64> {
    match output.plots.iter().find(|plot| plot.id == id)?.value {
        Value::Int(value) => Some(value as f64),
        Value::Float(value) => Some(value),
        _ => None,
    }
}

fn close_to(left: Option<f64>, right: Option<f64>, tolerance: f64) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left - right).abs() <= tolerance * right.abs().max(1.0),
        (None, None) => true,
        _ => false,
    }
}

fn utc(time: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_millis_opt(time).single().expect("valid time")
}

fn month_start(year: i32, month0: i32) -> i64 {
    let year = year + month0.div_euclid(12);
    let month = month0.rem_euclid(12) as u32 + 1;
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .expect("valid month")
        .timestamp_millis()
}

/// Start of the `months`-month calendar candle holding `time`, shifted by
/// `shift` candles.
fn months_start(time: i64, months: i32, shift: i32) -> i64 {
    let date = utc(time);
    let month0 = date.month0() as i32 / months * months + shift * months;
    month_start(date.year(), month0)
}

/// Open, high and low of the `months` monthly candles from `start` (only the
/// ones that exist), or `None` if the first one is missing.
fn aggregate(monthly: &BTreeMap<i64, Candle>, start: i64, months: i32) -> Option<Candle> {
    let date = utc(start);
    let parts = (0..months)
        .filter_map(|k| monthly.get(&month_start(date.year(), date.month0() as i32 + k)))
        .collect::<Vec<_>>();
    let first = parts.first()?;
    Some(Candle {
        time: start,
        open: first.open,
        high: parts.iter().map(|c| c.high).fold(f64::MIN, f64::max),
        low: parts.iter().map(|c| c.low).fold(f64::MAX, f64::min),
        close: parts.last()?.close,
    })
}

const KEY_LEVELS: &str = "key-levels.ind";
const KEY_LEVELS_BARS: &str = "tests/binance-btcusdt-perp-1h-20250601-20260115.csv";

#[test]
fn key_levels_match_the_exchange_candles() {
    let bars = candles(KEY_LEVELS_BARS);
    let daily = by_time(candles("tests/binance-btcusdt-perp-1d.csv"));
    let weekly = by_time(candles("tests/binance-btcusdt-perp-1w.csv"));
    let monthly = by_time(candles("tests/binance-btcusdt-perp-1M.csv"));
    let first = bars[0].time;
    // A candle is only usable when the history starts at or before its start.
    let whole = |start: i64| start >= first;

    let mut checked = 0usize;
    let mut failures = Vec::new();
    let mut check = |time: i64, id: &str, got: Option<f64>, want: Option<f64>| {
        checked += 1;
        if !close_to(got, want, 1e-9) {
            failures.push(format!("{} {id}: got {got:?}, want {want:?}", utc(time)));
        }
    };
    let mid = |candle: Option<Candle>| candle.map(|c| (c.high + c.low) / 2.0);

    let export = IndicatorConfig::default().with("exportValues", InputValue::Bool(true));
    for (time, output) in run(KEY_LEVELS, &bars, export) {
        let day = time.div_euclid(DAY) * DAY;
        let week = (time - 4 * DAY).div_euclid(WEEK) * WEEK + 4 * DAY;
        let month = months_start(time, 1, 0);
        let quarter = months_start(time, 3, 0);
        let year = months_start(time, 12, 0);

        let current = [
            ("d", whole(day).then(|| daily[&day])),
            ("w", whole(week).then(|| weekly[&week])),
            ("m", whole(month).then(|| monthly[&month])),
            (
                "q",
                whole(quarter)
                    .then(|| aggregate(&monthly, quarter, 3))
                    .flatten(),
            ),
            (
                "y",
                whole(year).then(|| aggregate(&monthly, year, 12)).flatten(),
            ),
        ];
        for (prefix, candle) in current {
            let id = format!("{prefix}_open");
            check(time, &id, plot(&output, &id), candle.map(|c| c.open));
        }

        let previous_month = months_start(time, 1, -1);
        let previous_quarter = months_start(time, 3, -1);
        let previous = [
            ("d", whole(day - DAY).then(|| daily[&(day - DAY)])),
            ("w", whole(week - WEEK).then(|| weekly[&(week - WEEK)])),
            ("m", whole(previous_month).then(|| monthly[&previous_month])),
            (
                "q",
                whole(previous_quarter)
                    .then(|| aggregate(&monthly, previous_quarter, 3))
                    .flatten(),
            ),
        ];
        for (prefix, candle) in previous {
            for (field, want) in [
                ("high", candle.map(|c| c.high)),
                ("low", candle.map(|c| c.low)),
                ("mid", mid(candle)),
            ] {
                let id = format!("{prefix}_prev_{field}");
                check(time, &id, plot(&output, &id), want);
            }
        }

        // The current year so far equals the monthly candles on a month's last hour.
        if time + HOUR == months_start(time, 1, 1) && whole(year) {
            let date = utc(year);
            let so_far = (0..12)
                .map(|k| month_start(date.year(), k))
                .take_while(|start| *start <= month)
                .map(|start| monthly[&start])
                .collect::<Vec<_>>();
            let high = so_far.iter().map(|c| c.high).fold(f64::MIN, f64::max);
            let low = so_far.iter().map(|c| c.low).fold(f64::MAX, f64::min);
            check(time, "y_high", plot(&output, "y_high"), Some(high));
            check(time, "y_low", plot(&output, "y_low"), Some(low));
            check(
                time,
                "y_mid",
                plot(&output, "y_mid"),
                Some((high + low) / 2.0),
            );
        }

        // From Monday's last hour on, the Monday range is Monday's day candle.
        if time >= week + DAY - HOUR {
            let monday = whole(week).then(|| daily[&week]);
            check(
                time,
                "mon_high",
                plot(&output, "mon_high"),
                monday.map(|c| c.high),
            );
            check(
                time,
                "mon_low",
                plot(&output, "mon_low"),
                monday.map(|c| c.low),
            );
            check(time, "mon_mid", plot(&output, "mon_mid"), mid(monday));
        }
    }

    assert!(checked > 100_000, "only {checked} values checked");
    assert!(
        failures.is_empty(),
        "{} of {checked} mismatches, first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(10)]
    );
}

#[test]
fn key_levels_merge_labels_at_equal_prices() {
    // 2026-01-01 03:00 UTC, a Thursday: the day, month, quarter and year
    // candles all opened at the same price.
    let end = Utc
        .with_ymd_and_hms(2026, 1, 1, 3, 0, 0)
        .single()
        .expect("valid time")
        .timestamp_millis();
    let bars = candles(KEY_LEVELS_BARS)
        .into_iter()
        .filter(|bar| bar.time <= end)
        .collect::<Vec<_>>();
    let labels = |config: IndicatorConfig| {
        let (_, last) = run(KEY_LEVELS, &bars, config)
            .pop()
            .expect("bars were evaluated");
        let last_index = (bars.len() - 1) as i64;
        last.regions
            .iter()
            .map(|region| {
                assert_eq!(region.right_index as i64, last_index + 30);
                (
                    region.upper,
                    region.text.as_ref().map(|text| text.content.clone()),
                )
            })
            .collect::<Vec<_>>()
    };

    let merged = labels(IndicatorConfig::default());
    let year_open = merged
        .iter()
        .find(|(_, text)| {
            text.as_deref()
                .is_some_and(|text| text.contains("Yearly Open"))
        })
        .expect("a yearly open label");
    assert_eq!(
        year_open.1.as_deref(),
        Some("Yearly Open / Quarterly Open / Monthly Open / Daily Open")
    );
    let at_open = merged
        .iter()
        .filter(|(price, _)| *price == year_open.0)
        .collect::<Vec<_>>();
    // One line per level, one label among them.
    assert_eq!(at_open.len(), 4);
    assert_eq!(at_open.iter().filter(|(_, text)| text.is_some()).count(), 1);

    let separate = labels(
        IndicatorConfig::default()
            .with("mergeLevels", InputValue::Bool(false))
            .with("shortAll", InputValue::Bool(true)),
    );
    let mut at_open = separate
        .iter()
        .filter(|(price, _)| *price == year_open.0)
        .filter_map(|(_, text)| text.clone())
        .collect::<Vec<_>>();
    at_open.sort();
    assert_eq!(at_open, ["DO", "MO", "QO", "YO"]);
}

#[test]
fn bull_market_support_band_matches_tradingview() {
    // TradingView exports the value on the 1H bar where the week closes
    // (Sunday 23:00 UTC); the port holds it from the next bar, which on
    // weekly candles is the next week's bar.
    let weeks = candles("tests/binance-btcusdt-perp-1w-all-20261123.csv");
    let outputs = run(
        "bull-market-support-band.ind",
        &weeks,
        IndicatorConfig::default(),
    )
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let reference = read_csv("tests/tradingview-bmsb-btcusdt-perp-1h.csv");
    let mut compared = 0;
    for row in &reference {
        let time: i64 = row["time"].parse().expect("times are integers");
        let Some(output) = outputs.get(&(time + HOUR)) else {
            continue;
        };
        let sma: f64 = row["sma"].parse().expect("numbers");
        let ema: f64 = row["ema"].parse().expect("numbers");
        assert!(
            close_to(plot(output, "sma"), Some(sma), 1e-12),
            "{}: SMA {:?} vs TradingView {sma}",
            utc(time),
            plot(output, "sma")
        );
        assert!(
            close_to(plot(output, "ema"), Some(ema), 1e-9),
            "{}: EMA {:?} vs TradingView {ema}",
            utc(time),
            plot(output, "ema")
        );
        compared += 1;
    }
    assert!(compared >= 98, "only {compared} weeks compared");
}

#[test]
fn sma_matches_a_direct_average() {
    let bars = candles(KEY_LEVELS_BARS);
    let outputs = run("sma.ind", &bars, IndicatorConfig::default());
    for (index, (_, output)) in outputs.iter().enumerate() {
        for (id, length) in [("sma1", 20), ("sma2", 50), ("sma3", 100), ("sma4", 200)] {
            let want = (index + 1 >= length).then(|| {
                bars[index + 1 - length..=index]
                    .iter()
                    .map(|bar| bar.close)
                    .sum::<f64>()
                    / length as f64
            });
            assert!(
                close_to(plot(output, id), want, 1e-12),
                "bar {index} {id}: {:?} vs {want:?}",
                plot(output, id)
            );
        }
    }
}
