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

#[test]
fn roman_setups_apply_the_rules_to_the_universe() {
    let out = parse(block_on(
        server().roman_setups(Parameters(SetupsParams { coins: None })),
    ));
    assert_eq!(out["exchange"], "okx");
    assert_eq!(out["failed"], 4);
    assert_eq!(out["rules"]["max_stop_pct"], 5.0);
    let btc = &out["coins"][0];
    assert_eq!(btc["base"], "BTC");
    assert_eq!(btc["close"], 95567.3);
    // The fixture's last bar: bullish, the broken HTF level below price is
    // the nearest support, 3.73% away: WATCH.
    assert_eq!(btc["bias"], "bullish");
    assert_eq!(btc["statuses"], json!(["WATCH"]));
    assert_eq!(btc["primary"]["kind"], "broken_level");
    assert_eq!(btc["primary"]["distance_pct"], 3.73);
    let trade = &btc["trade"];
    assert_eq!(trade["direction"], "long");
    assert_eq!(trade["entry"], 91999.0);
    // 0.3 x the daily ATR (2610.9) below the level, rounded to the tick.
    assert_eq!(btc["atr_1d"], 2610.9);
    assert_eq!(trade["stop"], 91215.7);
    assert_eq!(trade["max_leverage"], 10.0, "the example plan's BTC");
    assert_eq!(trade["notional_per_10"], 1175.0);
    assert_eq!(btc["flip"]["to"], "bearish");
    assert_eq!(out["watch"], json!(["BTC"]));
    assert_eq!(btc["primary"]["name"], "broken HTF level");
    assert_eq!(btc["primary"]["role"], "support");
    // A ready create_alert for the WATCH level, with the whole message.
    let alert = &btc["alerts"][0];
    assert_eq!(alert["kind"], "watch");
    assert_eq!(alert["symbol"], "BTC");
    assert_eq!(alert["condition"], "crosses_below");
    assert_eq!(alert["price"], 91999.0);
    let message = alert["message"].as_str().unwrap();
    for part in [
        "BTC long WATCH at the broken HTF level 91999 (support)",
        "Entry 91999, stop 91215.7",
        ", T1 ",
        ", T2 ",
        "R)",
        "Only enter after a 1H MSS.",
    ] {
        assert!(message.contains(part), "{part}: {message}");
    }
    // Groups list coins by status.
    for group in ["live", "ran", "watch", "flip_watch", "counter_trend"] {
        assert!(out[group].is_array());
    }
}

#[test]
fn spot_status_from_the_plan_and_spot_candles() {
    let status = |date: &str| {
        parse(block_on(server().spot_status(Parameters(SpotParams {
            date: Some(date.to_owned()),
        }))))
    };
    // The fixture's BTC closes at 95567.3; the example plan's levels are far
    // below it.
    let out = status("2027-07-20");
    assert_eq!(out["pair"], "BTC-USDT");
    assert_eq!(out["close"], 95567.3);
    assert_eq!(out["date"], "2027-07-20");
    assert_eq!(out["spot_zone"], false);
    assert_eq!(out["next_level_below"]["price"], 56000.0);
    assert_eq!(out["vs_reference_pct"], 91.13);
    assert!(out["rsi_1d"].is_number());
    assert_eq!(out["sma200_1w"], Json::Null);
    assert!(out["notes"][0].as_str().unwrap().contains("weekly bars"));
    assert_eq!(out["monthly"]["done"], 1);
    // A planned Monday is a SPOT ZONE.
    let out = status("2027-08-02");
    assert_eq!(out["spot_zone"], true);
    assert_eq!(out["zone_reasons"][0], "monthly buy today: 800");
    let error = block_on(server().spot_status(Parameters(SpotParams {
        date: Some("tomorrow".into()),
    })))
    .unwrap_err();
    assert!(error.contains("not a date"), "{error}");
}

/// Recorded OKX BTC-USDT-SWAP 1h candles (2026-11-25); every other coin is
/// unlisted.
struct OkxBtcCandles;

const OKX_BTC: &str = "indicators/roman/tests/okx-btcusdt-perp-1h-20261125.csv";

fn okx_btc_info() -> exchange::TickerInfo {
    exchange::TickerInfo::new(
        exchange::Ticker::new("BTC-USDT-SWAP", exchange::adapter::Exchange::OkexLinear),
        0.1,
        0.01,
        None,
    )
}

impl crate::mcp::CandleSource for OkxBtcCandles {
    fn resolve(
        &self,
        _exchange: PerpExchange,
        coin: String,
    ) -> crate::mcp::BoxFuture<'_, Result<(String, exchange::TickerInfo), String>> {
        Box::pin(async move {
            match coin.as_str() {
                "BTC" => Ok(("BTC-USDT-SWAP".to_owned(), okx_btc_info())),
                _ => Err(format!("no OKX USDT perpetual for {coin}")),
            }
        })
    }

    fn closed_candles(
        &self,
        _exchange: PerpExchange,
        coin: String,
        timeframe: Timeframe,
        bars: usize,
    ) -> crate::mcp::BoxFuture<'_, Result<Series, String>> {
        Box::pin(async move {
            if coin != "BTC" {
                return Err(format!("no OKX USDT perpetual for {coin}"));
            }
            crate::mcp::tests::csv_series(OKX_BTC, "BTC-USDT-SWAP", okx_btc_info(), timeframe, bars)
        })
    }

    fn spot_candles(
        &self,
        pair: String,
        _timeframe: Timeframe,
        _bars: usize,
    ) -> crate::mcp::BoxFuture<'_, Result<Series, String>> {
        Box::pin(async move { Err(format!("no OKX spot pair {pair}")) })
    }
}

fn okx_server(paths: crate::mcp::Paths) -> OrbitTapeMcp {
    crate::mcp::tests::server_with(std::sync::Arc::new(OkxBtcCandles), paths)
}

fn btc_only() -> Parameters<SetupsParams> {
    Parameters(SetupsParams {
        coins: Some(vec!["BTC".into()]),
    })
}

/// A copy of the example plan with `edit` applied, in its own folder.
fn plan_with(name: &str, edit: impl Fn(&str) -> String) -> crate::mcp::Paths {
    let dir = crate::mcp::tests::temp_dir(name);
    let example =
        std::fs::read_to_string(crate::mcp::tests::repo("docs/plan.example.toml")).unwrap();
    let plan = dir.join("plan.toml");
    std::fs::write(&plan, edit(&example)).unwrap();
    crate::mcp::Paths {
        plan,
        ..crate::mcp::tests::paths()
    }
}

#[test]
fn roman_setups_on_recorded_okx_btc_have_a_bias_and_a_flip_level() {
    // The example plan's BTC override is the major-coin profile
    // (maxDist = 5, maxHeight = 2), on top of ctMode and mssGate.
    let out = parse(block_on(
        okx_server(crate::mcp::tests::paths()).roman_setups(btc_only()),
    ));
    assert_eq!(out["failed"], 0, "{out}");
    assert!(
        out["indicator"]
            .as_str()
            .unwrap()
            .ends_with("roman/roman-setup.ind")
    );
    let btc = &out["coins"][0];
    assert_eq!(btc["base"], "BTC");
    assert_ne!(btc["bias"], "none", "{btc}");
    assert!(btc["flip"]["level"].is_number(), "{btc}");
    assert!(btc["flip"]["distance_pct"].is_number(), "{btc}");
}

#[test]
fn unknown_or_invalid_plan_inputs_are_errors_not_a_missing_bias() {
    let unknown = plan_with("plan-unknown-input", |plan| {
        plan.replace("maxDist = 5", "maxDistance = 5")
    });
    let out = parse(block_on(okx_server(unknown).roman_setups(btc_only())));
    assert_eq!(out["failed"], 1, "{out}");
    let error = out["coins"][0]["error"].as_str().unwrap();
    assert!(error.contains("unknown input 'maxDistance'"), "{error}");

    let invalid = plan_with("plan-invalid-option", |plan| {
        plan.replace("mssGate = \"Zone tap or SFP\"", "mssGate = \"Wick tap\"")
    });
    let out = parse(block_on(okx_server(invalid).roman_setups(btc_only())));
    assert_eq!(out["failed"], 1);
    assert!(
        out["coins"][0]["error"]
            .as_str()
            .unwrap()
            .contains("mssGate"),
        "{out}"
    );
}

#[test]
fn a_roman_copy_without_setup_values_is_refused() {
    // An installed copy from before the x_* plots, ahead of the current one.
    let dir = crate::mcp::tests::temp_dir("stale-roman");
    let folder = dir.join("roman");
    std::fs::create_dir_all(&folder).unwrap();
    let current =
        std::fs::read_to_string(crate::mcp::tests::repo("indicators/roman/roman-setup.ind"))
            .unwrap();
    std::fs::write(
        folder.join("roman-setup.ind"),
        current.replace("\"x_", "\"old_"),
    )
    .unwrap();
    let paths = crate::mcp::Paths {
        indicator_folders: vec![dir, crate::mcp::tests::repo("indicators")],
        ..crate::mcp::tests::paths()
    };
    let error = block_on(okx_server(paths.clone()).roman_setups(btc_only())).unwrap_err();
    assert!(error.contains("older than this Orbit Tape"), "{error}");
    assert!(error.contains("x_bias"), "{error}");
    let error = block_on(
        okx_server(paths).market_snapshot(Parameters(SnapshotParams {
            coins: Some(vec!["BTC".into()]),
            exchange: None,
        })),
    )
    .unwrap_err();
    assert!(error.contains("older than this Orbit Tape"), "{error}");
}

#[test]
fn an_older_installed_copy_is_named_in_the_output() {
    // The user's copy has the setup values but no version; the shipped one
    // is 7.1.
    let dir = crate::mcp::tests::temp_dir("older-roman");
    let folder = dir.join("roman");
    std::fs::create_dir_all(&folder).unwrap();
    let current =
        std::fs::read_to_string(crate::mcp::tests::repo("indicators/roman/roman-setup.ind"))
            .unwrap();
    assert!(current.contains(", version = \"7.1\""));
    std::fs::write(
        folder.join("roman-setup.ind"),
        current.replace(", version = \"7.1\"", ""),
    )
    .unwrap();
    let paths = crate::mcp::Paths {
        indicator_folders: vec![dir.clone(), crate::mcp::tests::repo("indicators")],
        ..crate::mcp::tests::paths()
    };
    let server = okx_server(paths);
    // The user's copy still runs, and the output says it is older.
    let out = parse(block_on(server.roman_setups(btc_only())));
    assert_ne!(out["coins"][0]["bias"], "none");
    let warning = out["warnings"][0].as_str().unwrap();
    assert!(
        warning.contains("(no version) is older than the version 7.1"),
        "{warning}"
    );
    assert!(
        warning.contains("Your copy is the one that runs"),
        "{warning}"
    );
    // Shipped copy, then the user's (the home folder may show as ~).
    let command = &warning[warning.find("cp ").unwrap()..];
    assert!(
        command.contains("indicators/roman/roman-setup.ind ")
            && command.ends_with(&folder.join("roman-setup.ind").display().to_string()),
        "{warning}"
    );
    let listed = parse(block_on(server.list_indicators()));
    let roman = listed["indicators"]
        .as_array()
        .unwrap()
        .iter()
        .find(|indicator| indicator["id"] == "roman.roman-setup")
        .unwrap();
    assert_eq!(roman["source"], "user");
    assert_eq!(roman["version"], Json::Null);
    assert!(roman["update"].as_str().unwrap().contains("older"));
    // Other indicators are not flagged.
    let key_levels = listed["indicators"]
        .as_array()
        .unwrap()
        .iter()
        .find(|indicator| indicator["id"] == "market.key-levels")
        .unwrap();
    assert_eq!(key_levels["version"], "1.1");
    assert_eq!(key_levels["update"], Json::Null);
    // The same version: nothing to say.
    std::fs::write(folder.join("roman-setup.ind"), &current).unwrap();
    let out = parse(block_on(server.roman_setups(btc_only())));
    assert!(out.get("warnings").is_none(), "{out}");
}

#[test]
fn a_watch_alert_inside_the_zone_waits_at_the_near_edge() {
    // Bearish at 103, inside the resistance order block 102-104.
    let mut input = setups::CoinInput {
        close: 103.0,
        atr: Some(2.0),
        high_24h: Some(104.0),
        low_24h: Some(98.0),
        high_1m: Some(110.0),
        low_1m: Some(90.0),
        values: BTreeMap::new(),
    };
    for (name, value) in [
        ("x_bias", -1.0),
        ("x_s1Top", 104.0),
        ("x_s1Bot", 102.0),
        ("x_s1St", 0.0),
        ("x_fib50", 103.0),
        ("x_bullFlip", 120.0),
        ("x_tapDnAgo", 30.0),
        ("x_mssDir", -1.0),
        ("x_mssAgo", 5.0),
        ("x_ctArm", 0.0),
    ] {
        input.values.insert(name.to_owned(), Some(value));
    }
    let rules = setups::Rules {
        max_stop_pct: 5.0,
        leverage: 5.0,
        risk_pct: 0.5,
        counter_trend_risk_pct: 0.25,
    };
    let setup = input.setup(&rules);
    assert_eq!(setup.statuses, [setups::Status::Watch]);
    assert_eq!(setup.primary.unwrap().distance_pct, 0.0);
    let alerts = ready_alerts("TEST", &setup, Precision::of(&fixture_info()));
    let watch = &alerts[0];
    assert_eq!(watch["condition"], "crosses_above");
    // The near edge, not the close (the entry).
    assert_eq!(watch["price"], 102.0);
    let message = watch["message"].as_str().unwrap();
    assert!(message.contains("Entry 103,"), "{message}");
    assert!(message.contains("comes back to 102."), "{message}");

    // Outside the zone the alert is still at the entry.
    input.close = 100.0;
    let setup = input.setup(&rules);
    let alerts = ready_alerts("TEST", &setup, Precision::of(&fixture_info()));
    assert_eq!(alerts[0]["price"], 102.0);
    assert!(!alerts[0]["message"].as_str().unwrap().contains("inside"));
}
