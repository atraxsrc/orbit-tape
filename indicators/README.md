# Optional indicators

Scripts in this folder are not bundled into the app. Each one is a `.ind`
source file in Flowsurface's indicator language
([indicator-engine/LANGUAGE.md](../indicator-engine/LANGUAGE.md)).

| Script | ID | Ported from |
| --- | --- | --- |
| [roman/roman-setup.ind](roman/roman-setup.ind) | `roman.roman-setup` | Roman Trade Setup (H12 to H1) (Pine v6, rev 7) |
| [roman/enhanced-ob.ind](roman/enhanced-ob.ind) | `roman.enhanced-ob` | Enhanced Roman Order Block (Pine v6, rev 3) |

## Tests

`roman/tests/` holds 3000 Binance BTCUSDT perpetual 1H candles. `cargo test -p
flowsurface-indicator-engine --test roman_ports` checks:

- Roman Setup against values exported from the original Pine script on TradingView
  (bias, 50% fib, all four zone slots with breaker state, HTF MSS, SFP and LTF MSS events)
  on the last 999 bars. That export came from rev 5 of the script, so the test turns off
  the rev 6 additions (thin-body wick, broken-level retest); those, counter-trend mode and
  the rev 7 flip levels are not checked against TradingView yet.
- Enhanced OB against an independent re-implementation of its Pine logic: every
  formation and mitigation event.

## Install

Copy a script into the app's indicators folder, keeping its namespace folder,
then restart Flowsurface:

```sh
mkdir -p ~/.local/share/flowsurface/indicators/roman
cp indicators/roman/enhanced-ob.ind ~/.local/share/flowsurface/indicators/roman/
```

`FLOWSURFACE_INDICATORS_PATH` points the app at another folder. While working
on a script, load it directly instead:

```sh
./target/release/flowsurface --indicator-path indicators/roman/enhanced-ob.ind
```

## Alerts

Both scripts mark their signal plots with `alert = true`: HTF bias flips, HTF
SFPs, LTF MSS and counter-trend MSS (Roman Setup), and OB formed / mitigated
(Enhanced OB). With a chart showing the indicator, Flowsurface sends a desktop
notification and a chime when one is true on a newly closed bar. Turn single
conditions off under Settings > Alerts. After updating a script, copy it again
(or load it with `--indicator-path`) and restart.

## Check a script against candles

`flowsurface-indicator run` evaluates a script on a candle CSV and writes every
plot's value per bar, so the output can be compared with another platform:

```sh
cargo run --release -p flowsurface-indicator-engine --bin flowsurface-indicator -- \
  run indicators/roman/enhanced-ob.ind --bars candles.csv --out values.csv \
  --regions regions.csv --min-tick 0.1 --input mitigationType=Close
```

The candle CSV needs a header with `time`, `open`, `high`, `low`, `close` and
optionally `volume`, with time in Unix seconds or milliseconds.

## License

The Roman scripts are MIT licensed ([LICENSE-MIT](LICENSE-MIT)). The rest of
the repository is GPL-3.0.
