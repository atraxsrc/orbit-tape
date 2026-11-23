# Optional indicators

Scripts in this folder are not bundled into the app. Each one is a `.ind`
source file in Flowsurface's indicator language
([indicator-engine/LANGUAGE.md](../indicator-engine/LANGUAGE.md)).

| Script | ID | Ported from |
| --- | --- | --- |
| [roman/enhanced-ob.ind](roman/enhanced-ob.ind) | `roman.enhanced-ob` | Enhanced Roman Order Block (Pine v6, rev 3) |

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
