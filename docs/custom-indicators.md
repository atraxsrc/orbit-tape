# Custom indicators

Orbit Tape indicators are small `.ind` text files in a Pine-like language. You
can write your own without installing Rust: the `orbit-tape-indicator` CLI that
comes with the `.deb` creates, checks and tests them.

## Where indicator files go

| What | Path |
| --- | --- |
| Your indicators | `~/.local/share/orbit-tape/indicators/` (created on first start) |
| Override that folder | `ORBIT_TAPE_INDICATORS_PATH=/some/folder orbit-tape` |
| Load one file for this run only | `orbit-tape --indicator-path my-indicator.ind` (repeatable) |
| Built-in indicators | compiled into the app ([sources](../data/indicators/sources/)) |
| Examples | [examples/indicators/](../examples/indicators/), and `/usr/share/doc/orbit-tape/examples/indicators/` with the `.deb` |

The app reads every `.ind` file below the indicators folder, including
subfolders. Keep one folder per namespace, for example
`indicators/community/example.ind` for the ID `community.example`.

Indicators **reload live**: Orbit Tape watches the indicators folder (and
every `--indicator-path` file) and recompiles a file a moment after you save
it. Charts that show the indicator recalculate; unchanged indicators are left
alone. If a save does not compile, the chart keeps the last good version and
the error appears as a notice and in a red box at the bottom of the
indicator list, until the file compiles again. A file that has never compiled
is skipped.

## 1. Create a file

```sh
mkdir -p ~/.local/share/orbit-tape/indicators/community
orbit-tape-indicator new ~/.local/share/orbit-tape/indicators/community \
    --id community.example --title "Example"
```

This writes `example.ind`, a 14-period moving average:

```text
language(1)
indicator("Example", id = "community.example")
format(precision = "base_ticker_min_tick")
period = input.int(14, "Period", min = 1, max = 200)
value = ta.sma(chart.close, period)
plot(value, id = "value", title = "Value")
```

IDs are `<namespace>.<name>`. The file name must be `<name>.ind`. The `core`
namespace belongs to the built-in indicators.

## 2. Edit it

Open the file in any editor. The language basics:

- `input.int`, `input.float`, `input.bool`, `input.string`, `input.source`
  declare settings shown in the indicator's settings dialog.
- `chart.open/high/low/close/volume` are the bars; `x[1]` is the previous
  bar's value; `ta.*` has moving averages, RSI, standard deviation, pivots and
  more.
- `plot(...)` outputs a series. `display(...)` picks a separate panel or the
  price chart; `style(...)` declares theme-aware colors.
- `plot(condition, alert = true)` on a boolean series turns it into an alert:
  a desktop notification when it is true on a newly closed bar.
- `htf.bars("720")` reads closed candles of a higher timeframe (here 12H).

The full reference is [LANGUAGE.md](../indicator-engine/LANGUAGE.md). The
[examples](../examples/indicators/) are commented and cover overlay lines with
alerts (`ema-crossover.ind`), bands with a fill (`vwap-bands.ind`) and a
trade-based histogram (`volume-delta.ind`).

## 3. Check and test

```sh
cd ~/.local/share/orbit-tape/indicators
orbit-tape-indicator check community/example.ind
orbit-tape-indicator test community/example.ind
```

- `check` compiles the file and reports syntax, type, ID and file-name errors
  with line and column.
- `test` also runs it with default inputs on synthetic bars, including an
  open-bar preview, under the same resource limits as the app.

Both accept a folder and check every file in it. Add `--namespace community`
to make sure every file in a collection uses that namespace.

To see actual values, run the indicator on a candle CSV (columns `time`
(Unix ms), `open`, `high`, `low`, `close`, `volume`):

```sh
orbit-tape-indicator run community/example.ind --bars candles.csv --out values.csv
```

`--input period=50` overrides an input; `values.csv` has one row per bar with
every plot.

## 4. Load it in the app

1. Save the file in the indicators folder (no restart needed).
2. Open a candlestick chart pane.
3. In the chart's indicator sidebar, press **+** and pick your indicator from
   the list (your own indicators are listed with the built-in ones).
4. Open its settings to change inputs and colors.

While working on one file you can skip copying it into the folder:

```sh
orbit-tape --indicator-path ./example.ind
```

## Sharing a collection

A collection is a folder of `.ind` files under one namespace, for example a
Git repository with `mytools/*.ind`. Others copy the folder into their
indicators folder. Before publishing:

```sh
orbit-tape-indicator test mytools --namespace mytools
orbit-tape-indicator catalog mytools --output catalog.json --namespace mytools
```

`catalog.json` lists every indicator with its checksum, for catalog sites. The
in-app catalog browser shows the upstream
[Flowsurface indicator catalog](https://github.com/flowsurface-rs/indicators);
see its contributing guide to submit indicators there. Pick a license for your
files; the examples and built-in indicators are GPL-3.0-or-later.

## Troubleshooting

| Problem | What to do |
| --- | --- |
| Indicator missing from the list | Check the file ends in `.ind`, is below `~/.local/share/orbit-tape/indicators/` and compiles (see the error box in the indicator list). |
| "Indicator source quarantined" notice | The file failed to load; run `orbit-tape-indicator check` on it for the exact error. |
| `source filename ... must be ...` error | Rename the file to the part of the ID after the last dot. |
| Duplicate ID | Two files declare the same ID; IDs must be unique across the folder. |
| Nothing drawn | Make sure the plot is numeric and the history is long enough for the lookback; boolean plots are never drawn. |
| Volume delta stays empty | It needs trade data: enable historical trades under Settings > Network, or wait for live trades. |
| More detail | The app log is `~/.local/share/orbit-tape/orbit-tape-current.log`. |
