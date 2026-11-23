//! Regression tests for the Roman indicator ports in `indicators/roman`.
//!
//! The candles are 3000 Binance BTCUSDT perpetual 1H bars. Roman Setup is
//! checked against values exported from the original Pine script on
//! TradingView (rev 5 of the script, so the rev 6 additions are switched off),
//! and Enhanced OB against an independent re-implementation of its Pine logic.

use flowsurface_indicator_engine::{
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

/// Runs a script over the candle fixture and returns each bar's output by time.
fn run(script: &str, config: IndicatorConfig) -> BTreeMap<i64, FrameOutput> {
    let source = std::fs::read_to_string(fixture(script)).expect("script should exist");
    let mut instance = compile(&source, script)
        .unwrap_or_else(|diagnostics| panic!("{script} should compile: {diagnostics:?}"))
        .instantiate(config)
        .expect("inputs should bind");
    let field = |bar: &BTreeMap<String, String>, name: &str| -> f64 {
        bar[name].parse().expect("candle values are numbers")
    };
    read_csv("tests/binance-btcusdt-perp-1h.csv")
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
    let outputs = run("roman-setup.ind", config);
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

#[test]
fn enhanced_ob_matches_the_reference() {
    let outputs = run("enhanced-ob.ind", IndicatorConfig::default());
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
