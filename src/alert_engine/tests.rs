use super::*;
use data::config::alert_store::{Creator, PriceCross};
use exchange::adapter::Exchange;
use std::path::Path;

const FIXTURE: &str = "indicators/market/tests/binance-btcusdt-perp-1h-20250601-20260115.csv";
const HOUR: u64 = 3_600_000;

/// Fires on every bar that closed above its open.
const UP_BAR: &str = r#"language(1)
indicator("Up Bar", id = "test.up-bar", max_bars_back = 1)
display(view = "plot", attachment = "overlay", layer_name = "Up Bar")
plot(chart.close > chart.open, id = "up", title = "Up bar", alert = true)
plot(chart.close < chart.open, id = "down", title = "Down bar", alert = true)
"#;

fn ticker_info() -> TickerInfo {
    TickerInfo::new(
        Ticker::new("BTCUSDT", Exchange::BinanceLinear),
        0.1,
        0.001,
        None,
    )
}

fn temp_paths(name: &str) -> Paths {
    let dir = std::env::temp_dir().join(format!("orbit-tape-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let indicators = dir.join("indicators/test");
    std::fs::create_dir_all(&indicators).unwrap();
    std::fs::write(indicators.join("up-bar.ind"), UP_BAR).unwrap();
    Paths {
        store: dir.join("alerts.json"),
        history: dir.join("alert-history.json"),
        indicator_folders: vec![dir.join("indicators")],
    }
}

fn candles() -> Vec<Kline> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let info = ticker_info();
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| {
            let field: Vec<f64> = line
                .split(',')
                .map(|value| value.parse().unwrap())
                .collect();
            Kline::new(
                field[0] as u64,
                field[1],
                field[2],
                field[3],
                field[4],
                exchange::Volume::TotalOnly(exchange::unit::Qty::from_f64(field[5])),
                info.min_ticksize,
            )
        })
        .collect()
}

fn alert(id: &str, kind: AlertKind, frequency: Frequency) -> StoredAlert {
    StoredAlert {
        id: id.to_owned(),
        ticker: ticker_info().ticker,
        timeframe: Timeframe::H1,
        kind,
        message: String::new(),
        frequency,
        channel: Channel::Desktop,
        enabled: true,
        created_by: Creator::Claude,
        created_at: 0,
        fired_at: None,
        error: None,
    }
}

fn up_bar(plot: Option<&str>) -> AlertKind {
    AlertKind::Indicator {
        indicator: "test.up-bar".to_owned(),
        plot: plot.map(str::to_owned),
        inputs: BTreeMap::new(),
    }
}

fn core_with(paths: &Paths, alerts: Vec<StoredAlert>, started_at: u64) -> Core {
    let mut store = AlertStore::default();
    for alert in alerts {
        store.add(alert).unwrap();
    }
    data::config::shared_file::update(&paths.store, |saved: &mut AlertStore| {
        *saved = store.clone();
    })
    .unwrap();
    let mut core = Core::new(paths.clone(), started_at);
    core.set_ticker_info(ticker_info());
    core.set_store(store);
    core
}

/// Claims without sending anything: what `deliver` keeps, minus the
/// notification.
fn claim(paths: &Paths, firings: Vec<Firing>) -> Vec<Firing> {
    let candidates = firings.iter().map(|firing| firing.fired.clone()).collect();
    let (claimed, _) = AlertHistory::claim_in(&paths.history, candidates).unwrap();
    firings
        .into_iter()
        .filter(|firing| claimed.contains(&firing.fired))
        .collect()
}

// ------------------------------------------------------------------ price

#[test]
fn price_alert_fires_on_recorded_trades_then_turns_off() {
    let paths = temp_paths("price");
    let price = |cross| AlertKind::Price {
        cross,
        level: 100_000.0,
    };
    let mut core = core_with(
        &paths,
        vec![
            alert("up", price(PriceCross::Above), Frequency::Once),
            alert("either", price(PriceCross::Cross), Frequency::EveryBar),
        ],
        0,
    );
    let ticker = ticker_info().ticker;
    let trades = [
        (HOUR, 99_990.0),
        (HOUR + 1_000, 100_000.5), // crosses up
        (HOUR + 2_000, 99_999.0),  // crosses down, same hour
        (HOUR + 3_000, 100_001.0), // up again: `up` is off now
        (2 * HOUR + 5, 100_010.0), // next hour, still above
    ];
    let fired = core.on_trades(ticker, &trades, 10);
    let summary: Vec<(&str, &str)> = fired
        .iter()
        .map(|firing| (firing.alert_id.as_str(), firing.fired.condition.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            ("up", "crossed above 100000.0"),
            ("either", "crossed above 100000.0"),
            ("either", "still above 100000.0"),
        ]
    );
    assert!(!core.store().get("up").unwrap().enabled);
    assert_eq!(fired[0].outgoing.summary, "BTCUSDT: crossed above 100000.0");
    // Every-bar alerts are keyed by the bar, so two programs that both saw
    // the cross deliver it once.
    assert_eq!(fired[1].fired.bar_time, Some(HOUR));
    assert_eq!(claim(&paths, fired.clone()).len(), 3);
    assert!(claim(&paths, fired).is_empty());
}

#[test]
fn a_gap_after_a_reconnect_is_checked_with_candles() {
    let paths = temp_paths("gap");
    let mut core = core_with(
        &paths,
        vec![alert(
            "up",
            AlertKind::Price {
                cross: PriceCross::Above,
                level: 100_000.0,
            },
            Frequency::Once,
        )],
        0,
    );
    let ticker = ticker_info().ticker;
    assert!(core.on_trades(ticker, &[(HOUR, 99_000.0)], 0).is_empty());
    let info = ticker_info();
    let minute = |n: u64, high: f64| {
        Kline::new(
            HOUR + n * 60_000,
            99_000.0,
            high,
            98_500.0,
            99_100.0,
            exchange::Volume::TotalOnly(exchange::unit::Qty::from_f64(1.0)),
            info.min_ticksize,
        )
    };
    let fired = core.on_gap(ticker, &[minute(1, 99_500.0), minute(2, 100_200.0)], 0);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].fired.bar_time, Some(HOUR + 2 * 60_000));
}

// -------------------------------------------------------------- indicator

/// Index of the first bar after `start` where the bar closed up (or down).
fn find_bar(klines: &[Kline], start: usize, up: bool) -> usize {
    (start..klines.len())
        .find(|&index| (klines[index].close > klines[index].open) == up)
        .unwrap()
}

#[test]
fn indicator_alert_fires_on_bar_close_once_per_bar() {
    let paths = temp_paths("indicator");
    let klines = candles();
    let first = find_bar(&klines, 2_100, true);
    let bar = klines[first].time.as_u64();
    let mut core = core_with(
        &paths,
        vec![alert("up", up_bar(Some("up")), Frequency::EveryBar)],
        0,
    );

    // The engine has been running; the bar before was already checked.
    let ticker = ticker_info().ticker;
    let now = bar + HOUR + 5_000;
    let due = core.due(now);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].to, bar + HOUR);
    core.series
        .get_mut(&(ticker, Timeframe::H1))
        .unwrap()
        .checked = Some(bar - HOUR);
    let fired = core.on_candles(ticker, Timeframe::H1, klines[..=first].to_vec(), now);
    assert_eq!(fired.len(), 1);
    let firing = &fired[0];
    assert_eq!(firing.fired.bar_time, Some(bar));
    assert_eq!(firing.fired.condition, "Up bar");
    assert_eq!(firing.fired.source, "Up Bar");
    assert_eq!(firing.fired.condition_key.as_deref(), Some("alert/up/up"));
    assert!(!firing.fired.missed);
    assert!(firing.outgoing.summary.starts_with("BTCUSDT 1h: Up bar"));
    // Not due again until the next bar closes.
    assert!(core.due(now + 60_000).is_empty());
    // Still enabled: every bar.
    assert!(core.store().get("up").unwrap().enabled);
}

#[test]
fn a_missing_bar_is_asked_for_again() {
    let paths = temp_paths("lagging");
    let klines = candles();
    let index = 2_200;
    let bar = klines[index].time.as_u64();
    let mut core = core_with(
        &paths,
        vec![alert("up", up_bar(None), Frequency::EveryBar)],
        0,
    );
    let now = bar + HOUR + 5_000;
    // The exchange answers without the bar that just closed.
    let ticker = ticker_info().ticker;
    assert!(
        core.on_candles(ticker, Timeframe::H1, klines[..index].to_vec(), now)
            .is_empty()
    );
    assert!(core.due(now + 1_000).is_empty());
    let again = core.due(now + RETRY_MS);
    assert_eq!(again.len(), 1);
    // Only the missing bar is asked for.
    assert_eq!((again[0].from, again[0].to), (bar, bar + HOUR));
}

#[test]
fn catch_up_reports_a_missed_bar_only_when_it_is_recent() {
    let paths = temp_paths("catch-up");
    let klines = candles();
    let index = find_bar(&klines, 2_300, true);
    let bar = klines[index].time.as_u64();
    let closed = bar + HOUR;
    let ticker = ticker_info().ticker;

    // Started 20 minutes after the bar closed: delivered as missed.
    let started = closed + 20 * 60_000;
    let mut core = core_with(
        &paths,
        vec![alert("up", up_bar(Some("up")), Frequency::EveryBar)],
        started,
    );
    let fired = core.on_candles(
        ticker,
        Timeframe::H1,
        klines[..=index].to_vec(),
        started + 1_000,
    );
    assert_eq!(fired.len(), 1);
    assert!(fired[0].fired.missed);
    assert!(fired[0].outgoing.summary.ends_with("(missed while closed)"));

    // Started more than one bar after it closed: the first look only sees
    // the newest closed bar (the one after), never this one.
    let late = closed + HOUR + 60_000;
    let mut core = core_with(
        &paths,
        vec![alert("up", up_bar(Some("up")), Frequency::EveryBar)],
        late,
    );
    let fired = core.on_candles(ticker, Timeframe::H1, klines[..=index + 1].to_vec(), late);
    assert!(
        fired
            .iter()
            .all(|firing| firing.fired.bar_time == Some(closed)),
        "{fired:?}"
    );
}

#[test]
fn a_restart_in_the_same_bar_does_not_deliver_again() {
    let paths = temp_paths("restart");
    let klines = candles();
    let index = find_bar(&klines, 2_400, true);
    let bar = klines[index].time.as_u64();
    let now = bar + HOUR + 5_000;
    let ticker = ticker_info().ticker;
    let alerts = || vec![alert("up", up_bar(Some("up")), Frequency::EveryBar)];

    let mut first = core_with(&paths, alerts(), bar);
    first
        .series
        .get_mut(&(ticker, Timeframe::H1))
        .unwrap()
        .checked = Some(bar - HOUR);
    let fired = first.on_candles(ticker, Timeframe::H1, klines[..=index].to_vec(), now);
    assert_eq!(claim(&paths, fired).len(), 1);

    // Restarted 10 seconds later, within the same bar: the catch-up finds the
    // same signal, already in the history.
    let mut second = core_with(&paths, alerts(), now + 10_000);
    let fired = second.on_candles(
        ticker,
        Timeframe::H1,
        klines[..=index].to_vec(),
        now + 12_000,
    );
    assert_eq!(fired.len(), 1);
    assert!(claim(&paths, fired).is_empty());
}

#[test]
fn once_indicator_alert_turns_off_and_deliver_saves_it() {
    let paths = temp_paths("once");
    let klines = candles();
    let index = find_bar(&klines, 2_500, true);
    let bar = klines[index].time.as_u64();
    let ticker = ticker_info().ticker;
    let mut core = core_with(&paths, vec![alert("up", up_bar(None), Frequency::Once)], 0);
    core.series
        .get_mut(&(ticker, Timeframe::H1))
        .unwrap()
        .checked = Some(bar - HOUR);
    let fired = core.on_candles(
        ticker,
        Timeframe::H1,
        klines[..=index].to_vec(),
        bar + HOUR + 5_000,
    );
    assert_eq!(fired.len(), 1);
    assert!(!core.store().get("up").unwrap().enabled);

    // Channel none: deliver claims and saves without a desktop popup in tests.
    let quiet: Vec<Firing> = fired
        .into_iter()
        .map(|firing| Firing {
            channel: Channel::Ntfy,
            ..firing
        })
        .collect();
    assert_eq!(deliver(&paths, &mut core, quiet).len(), 1);
    let saved: AlertStore = data::config::shared_file::read(&paths.store).unwrap();
    let saved = saved.get("up").unwrap();
    assert!(!saved.enabled);
    assert!(saved.fired_at.is_some());
    assert!(saved.is_done());
}

#[test]
fn a_missing_indicator_is_saved_as_the_alerts_error() {
    let paths = temp_paths("missing");
    let klines = candles();
    let bar = klines[2_000].time.as_u64();
    let ticker = ticker_info().ticker;
    let kind = AlertKind::Indicator {
        indicator: "test.nope".to_owned(),
        plot: None,
        inputs: BTreeMap::new(),
    };
    let mut core = core_with(&paths, vec![alert("x", kind, Frequency::EveryBar)], 0);
    core.series
        .get_mut(&(ticker, Timeframe::H1))
        .unwrap()
        .checked = Some(bar - HOUR);
    assert!(
        core.on_candles(
            ticker,
            Timeframe::H1,
            klines[..=2_000].to_vec(),
            bar + HOUR + 5_000
        )
        .is_empty()
    );
    deliver(&paths, &mut core, Vec::new());
    let saved: AlertStore = data::config::shared_file::read(&paths.store).unwrap();
    assert_eq!(
        saved.get("x").unwrap().error.as_deref(),
        Some("indicator test.nope is not installed")
    );
}

#[test]
fn an_alert_added_while_running_does_not_fire_on_an_older_bar() {
    let paths = temp_paths("added-later");
    let klines = candles();
    let index = find_bar(&klines, 2_600, true);
    let bar = klines[index].time.as_u64();
    let ticker = ticker_info().ticker;
    // Running with no alerts, then one is added mid-bar.
    let mut core = core_with(&paths, Vec::new(), 0);
    let mut store = core.store().clone();
    store
        .add(alert("up", up_bar(Some("up")), Frequency::EveryBar))
        .unwrap();
    core.set_store(store);
    let now = bar + HOUR + 20 * 60_000;
    assert!(
        core.on_candles(ticker, Timeframe::H1, klines[..=index].to_vec(), now)
            .is_empty()
    );
    // The next bar closing is checked as usual.
    let next = find_bar(&klines, index + 1, true);
    let fired = core.on_candles(
        ticker,
        Timeframe::H1,
        klines[index + 1..=next].to_vec(),
        klines[next].time.as_u64() + HOUR + 5_000,
    );
    assert!(
        fired
            .iter()
            .any(|firing| firing.fired.bar_time == Some(klines[next].time.as_u64())),
        "{fired:?}"
    );
}
