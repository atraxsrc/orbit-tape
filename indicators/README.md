# Optional indicators

Scripts in this folder are not bundled into the app. Each one is a `.ind`
source file in Orbit Tape's indicator language
([indicator-engine/LANGUAGE.md](../indicator-engine/LANGUAGE.md)).

| Script | ID | Ported from |
| --- | --- | --- |
| [roman/roman-setup.ind](roman/roman-setup.ind) | `roman.roman-setup` | Roman Trade Setup (H12 to H1) (Pine v6, rev 7) |
| [roman/enhanced-ob.ind](roman/enhanced-ob.ind) | `roman.enhanced-ob` | Enhanced Roman Order Block (Pine v6, rev 3) |
| [market/key-levels.ind](market/key-levels.ind) | `market.key-levels` | Original (GPL-3.0-or-later): 4H/daily/weekly/monthly/quarterly/yearly opens, previous highs/lows/mids, Monday range, FX sessions |
| [market/bull-market-support-band.ind](market/bull-market-support-band.ind) | `market.bull-market-support-band` | zkdev's "Bull Market Support Band" (Pine v5, MPL-2.0): 20W SMA, 21W EMA |
| [market/sma.ind](market/sma.ind) | `market.sma` | Original (GPL-3.0-or-later): four SMAs, 20/50/100/200 by default |

Each script states its license in an SPDX header. The `roman` scripts and
the original `market` scripts are GPL-3.0-or-later; the Bull Market Support
Band port keeps the original's MPL-2.0 terms and credit.

## Tests

`roman/tests/` holds 3000 Binance BTCUSDT perpetual 1H candles. `cargo test -p
orbit-tape-indicator-engine --test roman_ports` checks:

- Roman Setup against values exported from the original Pine script on TradingView
  (bias, 50% fib, all four zone slots with breaker state, HTF MSS, SFP and LTF MSS events)
  on the last 999 bars. That export came from rev 5 of the script, so the test turns off
  the rev 6 additions (thin-body wick, broken-level retest); those, counter-trend mode and
  the rev 7 flip levels are not checked against TradingView yet.
- Enhanced OB against an independent re-implementation of its Pine logic: every
  formation and mitigation event.

`market/tests/` holds 5496 Binance BTCUSDT perpetual 1H candles (2025-06-01 to
2026-01-15), the exchange's own 1d/1w/1M candles for that window and its full
weekly history. `cargo test -p orbit-tape-indicator-engine --test
market_indicators` checks:

- Key Levels bar for bar against levels computed from the exchange's 1d/1w/1M
  candles (quarters and years aggregated from months), including the merged
  labels on 2026-01-01.
- The Bull Market Support Band against values exported from the original Pine
  script on TradingView (BINANCE:BTCUSDT.P, 100 weeks).
- The SMAs against a direct average.

## Install

Copy a script into the app's indicators folder, keeping its namespace folder.
A running Orbit Tape loads it within a second:

```sh
mkdir -p ~/.local/share/orbit-tape/indicators/roman
cp indicators/roman/enhanced-ob.ind ~/.local/share/orbit-tape/indicators/roman/
```

`ORBIT_TAPE_INDICATORS_PATH` points the app at another folder. While working
on a script, load it directly instead:

```sh
./target/release/orbit-tape --indicator-path indicators/roman/enhanced-ob.ind
```

## Alerts

Both scripts mark their signal plots with `alert = true`: HTF bias flips, HTF
SFPs, LTF MSS and counter-trend MSS (Roman Setup), and OB formed / mitigated
(Enhanced OB). With a chart showing the indicator, Orbit Tape sends a desktop
notification and a chime when one is true on a newly closed bar. Turn single
conditions off under Settings > Alerts. After updating a script, copy it again
(or load it with `--indicator-path`); the app reloads it live.

## Check a script against candles

`orbit-tape-indicator run` evaluates a script on a candle CSV and writes every
plot's value per bar, so the output can be compared with another platform:

```sh
cargo run --release -p orbit-tape-indicator-engine --bin orbit-tape-indicator -- \
  run indicators/roman/enhanced-ob.ind --bars candles.csv --out values.csv \
  --regions regions.csv --min-tick 0.1 --input mitigationType=Close
```

The candle CSV needs a header with `time`, `open`, `high`, `low`, `close` and
optionally `volume`, with time in Unix seconds or milliseconds.

## License

The Roman scripts and the original market scripts are GPL-3.0-or-later, like
the rest of the repository. The Bull Market Support Band port is MPL-2.0.
