use super::*;
use crate::mcp::tests::{block_on, fixture_info, parse, server};
use exchange::Kline;

/// Candles `step` ms apart with these closes (high +1, low -1).
fn series(step: u64, closes: &[f64]) -> Series {
    Series {
        instrument: "TESTUSDT".to_owned(),
        ticker_info: fixture_info(),
        klines: closes
            .iter()
            .enumerate()
            .map(|(index, close)| {
                Kline::new(
                    index as u64 * step,
                    *close,
                    close + 1.0,
                    close - 1.0,
                    *close,
                    exchange::Volume::TotalOnly(exchange::unit::Qty::from_f64(1.0)),
                    fixture_info().min_ticksize,
                )
            })
            .collect(),
    }
}

const HOUR: u64 = 3_600_000;
const DAY: u64 = 24 * HOUR;

#[test]
fn snapshot_numbers_from_candles() {
    // 200 hours rising by 1 from 100; 40 days rising by 10 from 100.
    let hours = series(
        HOUR,
        &(0..200).map(|i| 100.0 + i as f64).collect::<Vec<_>>(),
    );
    let days = series(
        DAY,
        &(0..40).map(|i| 100.0 + 10.0 * i as f64).collect::<Vec<_>>(),
    );
    let snapshot = Snapshot::new(&hours, &days, None, None);
    assert_eq!(snapshot.close, 299.0);
    // 24 hours ago: 275; 168 hours ago: 131.
    assert!((snapshot.day_pct.unwrap() - (299.0 - 275.0) / 275.0 * 100.0).abs() < 1e-9);
    assert!((snapshot.week_pct.unwrap() - (299.0 - 131.0) / 131.0 * 100.0).abs() < 1e-9);
    assert_eq!(snapshot.high_24h, Some(300.0));
    assert_eq!(snapshot.low_24h, Some(275.0));
    // Only rises: RSI 100.
    assert_eq!(snapshot.rsi_1d, Some(100.0));
    // 30 days: days 10..=39 (lows from 199), plus hours after the last day
    // closed (none here, they all came before it).
    assert_eq!(snapshot.low_1m, Some(199.0));
    assert_eq!(snapshot.high_1m, Some(491.0));
    assert_eq!(snapshot.low_3m, None);
    assert_eq!(snapshot.sma200_1d, None);
    assert!(snapshot.atr_1d.is_some());
    // Missing values say why.
    let notes = snapshot.notes.join("; ");
    assert!(notes.contains("sma200_1d: only 40 daily bars"), "{notes}");
    assert!(
        notes.contains("weekly candles could not be fetched"),
        "{notes}"
    );
    assert!(notes.contains("rsi_4h"), "{notes}");
}

#[test]
fn market_snapshot_runs_the_plan_universe() {
    let out = parse(block_on(server().market_snapshot(Parameters(
        SnapshotParams {
            coins: None,
            exchange: None,
        },
    ))));
    assert_eq!(out["exchange"], "okx");
    assert_eq!(out["plan_inputs"], true);
    let coins = out["coins"].as_array().unwrap();
    // The example plan's five coins; the fixture only has BTC.
    assert_eq!(coins.len(), 5);
    assert_eq!(out["failed"], 4);
    let btc = &coins[0];
    assert_eq!(btc["base"], "BTC");
    assert_eq!(btc["last_closed_hour_utc"], "2026-01-15 23:00");
    assert_eq!(btc["close"], 95567.3);
    for field in [
        "rsi_1d",
        "rsi_4h",
        "atr_1d",
        "sma200_1d",
        "low_3m",
        "day_pct",
    ] {
        assert!(btc[field].is_number(), "{field}: {btc}");
    }
    assert!(["bullish", "bearish", "none"].contains(&btc["roman_12h_bias"].as_str().unwrap()));
    // About 33 weeks in the fixture: no 200-week average, and it says so.
    assert_eq!(btc["sma200_1w"], Json::Null);
    assert!(
        btc["notes"][0].as_str().unwrap().contains("weekly bars"),
        "{btc}"
    );
    assert!(
        coins[1]["error"]
            .as_str()
            .unwrap()
            .contains("no OKX USDT perpetual for ETH")
    );
}

#[test]
fn market_snapshot_takes_coins_without_a_plan() {
    let server = crate::mcp::tests::server_with(
        std::sync::Arc::new(crate::mcp::tests::FixtureCandles),
        crate::mcp::Paths {
            plan: crate::mcp::tests::repo("target/no-such-plan.toml"),
            ..crate::mcp::tests::paths()
        },
    );
    let out = parse(block_on(server.market_snapshot(Parameters(
        SnapshotParams {
            coins: Some(vec!["BTCUSDT".into(), "btc".into()]),
            exchange: Some("binance".into()),
        },
    ))));
    assert_eq!(out["plan_inputs"], false);
    assert_eq!(
        out["coins"].as_array().unwrap().len(),
        1,
        "duplicates dropped"
    );
    let error = block_on(server.market_snapshot(Parameters(SnapshotParams {
        coins: None,
        exchange: None,
    })))
    .unwrap_err();
    assert!(error.contains("plan.example.toml"), "{error}");
}
