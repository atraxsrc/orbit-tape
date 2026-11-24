//! Regression tests for the Roman indicator ports in `indicators/roman`.
//!
//! The candles are 3000 Binance perpetual 1H bars per fixture. Roman Setup is
//! checked against values exported from the original Pine script on
//! TradingView: a rev 5 export (rev 6 additions switched off) and rev 7
//! exports with every default on, with and without counter-trend mode. The
//! TRXUSDT export covers HTF pivots with equal highs or lows, which pins down
//! the pivot tie rule. Two 4H counter-trend exports (3000 bars each) cover
//! what the 1H windows never hit: counter-trend shorts (ETHUSDT) and HTF
//! pivots tied with a newer bar (ADAUSDT). Enhanced OB is checked against an independent
//! re-implementation of its Pine logic.

use orbit_tape_indicator_engine::{
    ChartFrame, FrameOutput, IndicatorConfig, IndicatorFrame, InputValue, InstrumentContext,
    TimeOrTick, Value, compile,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../indicators/roman")
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

const BTC_CANDLES: &str = "tests/binance-btcusdt-perp-1h.csv";

/// Runs a script over a candle fixture and returns each bar's output by time.
fn run(script: &str, candles: &str, config: IndicatorConfig) -> BTreeMap<i64, FrameOutput> {
    let source = std::fs::read_to_string(fixture(script)).expect("script should exist");
    let mut instance = compile(&source, script)
        .unwrap_or_else(|diagnostics| panic!("{script} should compile: {diagnostics:?}"))
        .instantiate(config)
        .expect("inputs should bind");
    let field = |bar: &BTreeMap<String, String>, name: &str| -> f64 {
        bar[name].parse().expect("candle values are numbers")
    };
    read_csv(candles)
        .iter()
        .enumerate()
        .map(|(index, bar)| {
            let time = bar["time"].parse().expect("times are integers");
            let frame = IndicatorFrame {
                index,
                time,
                basis: TimeOrTick::Time,
                instrument: InstrumentContext {
                    min_tick: Some(0.1),
                },
                chart: ChartFrame {
                    open: Some(field(bar, "open")),
                    high: Some(field(bar, "high")),
                    low: Some(field(bar, "low")),
                    close: Some(field(bar, "close")),
                    volume: Some(field(bar, "volume")),
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
                .unwrap_or_else(|error| panic!("bar {index} failed: {error}"));
            (time, output)
        })
        .collect()
}

fn plot(output: &FrameOutput, id: &str) -> Option<f64> {
    match output.plots.iter().find(|plot| plot.id == id)?.value {
        Value::Int(value) => Some(value as f64),
        Value::Float(value) => Some(value),
        Value::Bool(value) => Some(if value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn flag(output: &FrameOutput, id: &str) -> bool {
    plot(output, id) == Some(1.0)
}

fn same(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left - right).abs() <= 1e-6 * left.abs().max(1.0),
        (None, None) => true,
        _ => false,
    }
}

#[test]
fn roman_setup_matches_tradingview() {
    let config = IndicatorConfig::default()
        .with("mssGate", InputValue::String("None".to_owned()))
        .with("thinBody", InputValue::Float(0.0))
        .with("brkTap", InputValue::Bool(false));
    let outputs = run("roman-setup.ind", BTC_CANDLES, config);
    let expected = read_csv("tests/tradingview-roman-setup-rev5.csv");
    assert!(expected.len() > 900);

    let mut mismatches = Vec::new();
    for row in &expected {
        let time: i64 = row["time"].parse().unwrap();
        let output = outputs
            .get(&time)
            .expect("TradingView bar is in the fixture");
        let value = |name: &str| row[name].parse::<f64>().ok();
        let mut check = |what: &str, ours: Option<f64>, theirs: Option<f64>| {
            if !same(ours, theirs) {
                mismatches.push(format!(
                    "{time} {what}: ours {ours:?}, TradingView {theirs:?}"
                ));
            }
        };
        check("bias", plot(output, "dbg_bias"), value("x_bias"));
        check("fib50", plot(output, "dbg_fib50"), value("x_fib50"));
        for slot in ["b1", "b2", "s1", "s2"] {
            check(
                &format!("{slot} top"),
                plot(output, &format!("dbg_{slot}_top")),
                value(&format!("x_{slot}Top")),
            );
            check(
                &format!("{slot} bottom"),
                plot(output, &format!("dbg_{slot}_bot")),
                value(&format!("x_{slot}Bot")),
            );
            // TradingView state: 0 order block, 1 breaker, +2 hidden.
            check(
                &format!("{slot} breaker"),
                plot(output, &format!("dbg_{slot}_brk")),
                value(&format!("x_{slot}St")).map(|state| (state as i64 % 2) as f64),
            );
        }
        let event = |name: &str| value(name) == Some(0.0);
        let htf_mss = flag(output, "htf_bias_bullish") || flag(output, "htf_bias_bearish");
        let ltf_mss = flag(output, "ltf_mss_up") || flag(output, "ltf_mss_down");
        let sfp = flag(output, "htf_sfp_bullish") || flag(output, "htf_sfp_bearish");
        for (what, ours, theirs) in [
            ("HTF MSS", htf_mss, event("x_htfMssAgo")),
            ("LTF MSS", ltf_mss, event("x_mssAgo")),
            ("SFP", sfp, event("x_sfpAgo")),
        ] {
            if ours != theirs {
                mismatches.push(format!("{time} {what}: ours {ours}, TradingView {theirs}"));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatches, first: {:#?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(10)]
    );
}

/// Compares the port with a rev 7 TradingView export (closed bars only) and
/// returns how often each event fired, so callers can check coverage.
fn check_rev7(
    candles: &str,
    export: &str,
    config: IndicatorConfig,
) -> BTreeMap<&'static str, usize> {
    let outputs = run("roman-setup.ind", candles, config);
    let expected = read_csv(export);
    assert!(expected.len() > 900);

    let mut series = [
        ("dbg_bias", "x_bias"),
        ("dbg_fib50", "x_fib50"),
        ("dbg_broken_level", "x_brkLvl"),
        ("dbg_broken_away", "x_brkAway"),
        ("dbg_up_flip", "x_upFlip"),
        ("dbg_dn_flip", "x_dnFlip"),
        ("dbg_ct_arm_up", "x_ctArmUp"),
        ("dbg_ct_arm_down", "x_ctArmDn"),
    ]
    .map(|(ours, theirs)| (ours.to_owned(), theirs.to_owned()))
    .to_vec();
    for slot in ["b1", "b2", "s1", "s2"] {
        for (ours, theirs) in [("top", "Top"), ("bot", "Bot"), ("brk", "Brk")] {
            series.push((format!("dbg_{slot}_{ours}"), format!("x_{slot}{theirs}")));
        }
    }
    let events = [
        ("HTF MSS up", "htf_bias_bullish", "x_evBull"),
        ("HTF MSS down", "htf_bias_bearish", "x_evBear"),
        ("SFP up", "htf_sfp_bullish", "x_sfpBull"),
        ("SFP down", "htf_sfp_bearish", "x_sfpBear"),
        ("MSS up", "ltf_mss_up", "x_alnUp"),
        ("MSS down", "ltf_mss_down", "x_alnDn"),
        ("CT MSS up", "ltf_ct_mss_up", "x_mssCtUp"),
        ("CT MSS down", "ltf_ct_mss_down", "x_mssCtDn"),
    ];

    let mut counts = BTreeMap::new();
    let mut mismatches = Vec::new();
    for row in &expected {
        let time: i64 = row["time"].parse().unwrap();
        let output = outputs
            .get(&time)
            .expect("TradingView bar is in the candles");
        let value = |name: &str| row[name].parse::<f64>().ok();
        for (ours, theirs) in &series {
            let (ours_value, theirs_value) = (plot(output, ours), value(theirs));
            if !same(ours_value, theirs_value) {
                mismatches.push(format!(
                    "{time} {ours}: ours {ours_value:?}, TradingView {theirs_value:?}"
                ));
            }
        }
        for (what, ours, theirs) in events {
            let (ours, theirs) = (flag(output, ours), value(theirs) == Some(1.0));
            if ours != theirs {
                mismatches.push(format!("{time} {what}: ours {ours}, TradingView {theirs}"));
            }
            *counts.entry(what).or_default() += usize::from(theirs);
        }
        if value("x_brkAway") == Some(1.0) {
            *counts.entry("broken away").or_default() += 1;
        }
    }
    assert!(
        mismatches.is_empty(),
        "{export}: {} mismatches, first: {:#?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(10)]
    );
    counts
}

#[test]
fn roman_setup_rev7_matches_tradingview() {
    let counts = check_rev7(
        "tests/binance-btcusdt-perp-1h-20261123.csv",
        "tests/tradingview-roman-setup-rev7-btc.csv",
        IndicatorConfig::default(),
    );
    // Most of these MSS signals only pass the filter through a broken-level retest.
    assert!(counts["MSS down"] > 10, "{counts:?}");
    assert!(counts["broken away"] > 0, "{counts:?}");
    assert_eq!(counts["CT MSS up"] + counts["CT MSS down"], 0);
}

#[test]
fn roman_setup_rev7_counter_trend_matches_tradingview() {
    for (candles, export) in [
        (
            "tests/binance-btcusdt-perp-1h-20261123.csv",
            "tests/tradingview-roman-setup-rev7-btc-ct.csv",
        ),
        (
            "tests/binance-trxusdt-perp-1h-20261123.csv",
            "tests/tradingview-roman-setup-rev7-trx-ct.csv",
        ),
    ] {
        let config = IndicatorConfig::default().with("ctMode", InputValue::Bool(true));
        let counts = check_rev7(candles, export, config);
        assert!(
            counts["CT MSS up"] + counts["CT MSS down"] > 0,
            "{export}: {counts:?}"
        );
    }
}

#[test]
fn roman_setup_rev7_4h_matches_tradingview() {
    let config = || IndicatorConfig::default().with("ctMode", InputValue::Bool(true));
    let eth = check_rev7(
        "tests/binance-ethusdt-perp-4h-20261124.csv",
        "tests/tradingview-roman-setup-rev7-eth-4h-ct.csv",
        config(),
    );
    assert!(eth["CT MSS down"] > 0, "{eth:?}");
    // ADAUSDT has two 12H swing lows tied with a newer bar inside the window.
    let ada = check_rev7(
        "tests/binance-adausdt-perp-4h-20261124.csv",
        "tests/tradingview-roman-setup-rev7-ada-4h-ct.csv",
        config(),
    );
    assert!(ada["HTF MSS down"] > 0, "{ada:?}");
}

#[test]
fn enhanced_ob_matches_the_reference() {
    let outputs = run("enhanced-ob.ind", BTC_CANDLES, IndicatorConfig::default());
    let expected = read_csv("tests/reference-enhanced-ob.csv");
    let mut events = 0;
    for row in &expected {
        let time: i64 = row["time"].parse().unwrap();
        let output = &outputs[&time];
        for name in ["bull_formed", "bear_formed", "mitigated"] {
            let theirs = row[name] == "true";
            assert_eq!(flag(output, name), theirs, "{time} {name}");
            events += usize::from(theirs);
        }
    }
    assert!(events > 300, "the fixture should contain plenty of events");
}
