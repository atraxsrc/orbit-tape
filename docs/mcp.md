# MCP server

`orbit-tape mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdin and stdout. It lets an AI assistant such as Claude scan
markets, read candles, run your Orbit Tape indicators and set up alerts, from
the same exchange adapters and indicator engine the app uses.

What it can and cannot do:

- No API keys and no accounts: it only reads public candles from Binance or OKX
  USDT perpetuals, and for `spot_status` OKX spot.
- No orders and no trading actions of any kind.
- The only file it writes is your saved alerts, `alerts.json` (see
  [Alerts](#alerts)). `scan` never creates `scan-symbols.txt`, and
  `alert_history` only reads the saved history.
- No window. It does not talk to a running Orbit Tape; the app and
  `orbit-tape watch` pick up alert changes from the file.

Logs go to stderr; stdout carries the protocol.

## Setup

The `.deb` installs `orbit-tape` in `/usr/bin`. From a source checkout, build it
with `cargo build --release` and use the full path to
`target/release/orbit-tape` instead.

### Claude Code

```sh
claude mcp add orbit-tape -- orbit-tape mcp
```

Add `--scope user` to use it in every project. `claude mcp list` should show
`orbit-tape` as connected; inside Claude Code, `/mcp` lists its tools.

### Claude desktop app and other MCP clients

Clients that read a JSON list of servers (the Claude desktop app's
`claude_desktop_config.json`, and most other MCP clients) take:

```json
{
  "mcpServers": {
    "orbit-tape": {
      "command": "/usr/bin/orbit-tape",
      "args": ["mcp"]
    }
  }
}
```

Restart the client after editing its config.

## Tools

All times are UTC and all candles are **closed** bars. Exchanges are `binance`
or `okx` (USDT perpetuals). Every tool that takes `exchange` defaults to the
`exchange =` line of your `scan-symbols.txt`, else `okx`, and every result
names the exchange it used. Symbols can be written as `BTC`, `BTCUSDT` or
`BTC-USDT-SWAP`, and Binance's 1000x contracts (`1000PEPEUSDT`) are found
from `PEPE`; results carry both `base` (`BTC`, to join results across tools
and exchanges) and `symbol`, the exchange's own name. Timeframes:
`1m 3m 5m 15m 30m 1h 2h 4h 12h 1d 1w` (weeks open on Monday 00:00 UTC; 1w is for the tools only, charts stop at 1d).

Prices are rounded to the instrument's tick size. Candle `volume` is quote
volume in USDT (base volume times the bar's close, the app's default size
unit), rounded to cents. Indicator values use the plot's own precision, or
10 significant digits. Colors are `#RRGGBB`, `#RRGGBBAA` when not opaque, or
a desktop theme role such as `theme.danger` (`theme.text@f0` with alpha).

| Tool | What it does | Main parameters |
| --- | --- | --- |
| `scan` | Runs an indicator over many coins and lists the alerts that fired on the last closed bar(s), each as `condition` and `bar_utc`. Default: Roman Trade Setup on 1h over your `scan-symbols.txt`. | `indicator`, `timeframe`, `exchange`, `symbols` (at most 100), `lookback` (1 to 50), `inputs` |
| `get_candles` | Closed OHLCV candles, oldest first. | `symbol`, `timeframe`, `exchange`, `limit` (default 100, at most 500) |
| `run_indicator` | Runs one indicator on one coin: every plot value (hidden ones too, columns named by plot title, `plot_ids` maps titles to IDs, `last_values` holds the newest closed bar), markers the script showed, fired alerts and the info panel for the last N bars. `history_note` says when the exchange had fewer bars than asked for. | `indicator`, `symbol`, `timeframe`, `exchange`, `inputs`, `last` (default 20, at most 200), `bars` (default 2000, at most 20000) |
| `key_levels` | Current Key Levels sorted by price, each marked above or below the last close. By default the levels `key-levels.ind` shows with its own settings (4H off; daily open; Monday range and mid; weekly and monthly open, high, low and mid; quarterly open and mid; yearly open and mid). Runs on 1h candles back to the start of the year or the previous quarter; levels without a value (e.g. a coin listed this year) are listed under `missing` with the reason, and `history_note` says when the exchange had less history. | `symbol`, `exchange`, `levels` (groups `4h daily monday weekly monthly quarterly yearly`, or level IDs such as `w_open`, `d_prev_high`) |
| `check_indicator` | Same as `orbit-tape-indicator check`. | `path` (a `.ind` file or folder), `namespace` |
| `test_indicator` | Same as `orbit-tape-indicator test`: check, then run on synthetic bars. | `path`, `namespace` |
| `list_indicators` | Your installed indicators: name, ID, path relative to its indicators folder (`roman/roman-setup.ind`) with `source` `user` or `bundled`, inputs with defaults and options, and alert conditions. | none |
| `alert_history` | The newest alerts that fired (in the app or `orbit-tape watch`), from the saved history: fire time, the bar it fired on (`bar_utc`, null for alerts saved by older versions), exchange, market, timeframe, condition, price (a number) and `missed_while_closed`. | `last` (default 20, at most 200) |
| `create_alert` | Saves an alert: a price crossing a level, or an indicator's alert condition on a closed bar. Returns its ID, a one-line summary and whether a program runs the alerts now. | `symbol`, `exchange`, `market` (`perp` default, or `spot`: OKX USDT spot pairs such as `BTC-USDT`), `condition` (`crosses_above`, `crosses_below`, `crosses`, `indicator`), `price`, `indicator`, `alert` (one condition; default any), `inputs`, `timeframe` (default 1h), `frequency` (`once` or `every_bar`), `channel` (`desktop`, `ntfy`, `both`), `message` |
| `list_alerts` | The saved alerts with ID, summary, status (`active`, `paused`, `fired`), who made them and any error. | none |
| `update_alert` | Changes an alert's price, direction, frequency, channel, message, or pauses/resumes it. | `id` and the fields to change |
| `delete_alert` | Deletes an alert. | `id` |
| `pause_alerts` / `resume_alerts` | Pauses or resumes alerts by ID, or all of them. Resuming all skips `once` alerts that already fired. | `ids` (optional) |
| `get_plan` | Your trading plan file (see [Daily routine](#daily-routine)), checked, with the per-coin Roman Setup inputs and leverage filled in. | none |
| `market_snapshot` | Per coin of the plan's universe: close, day % and the 24h high and low (a rolling 24 hours of 1h bars), week % (against 7 days ago), RSI(14) 1D, 4H and 1W, daily ATR(14), the 1M high and low, the 3M low, SMA200 1D and 1W and price against them, and Roman Setup's 12H bias. `notes` says what was missing. | `coins`, `exchange` |
| `roman_setups` | Roman Setup on every coin of the plan with its inputs, and the routine's rules applied: the primary bias-aligned level and its distance, entry, stop (beyond the area plus 0.3 daily ATR), T1 and T2 with R multiples, notional and margin per $10 risked. Each level is named (e.g. a bear-slot breaker is a broken bear order block, so it supports a long). Statuses `LIVE` (ranked: closest to the entry, then best R to T2, then higher max leverage), `RAN` (triggered, but price is at or past T1 or further past the entry than the stop distance), `WATCH`, `FLIP-WATCH`, `COUNTER-TREND`, and coins grouped by them; a setup whose stop is over the plan's maximum is skipped with the reason. Each coin's `alerts` holds ready `create_alert` arguments for its WATCH and FLIP-WATCH levels, with the full message (level, entry, stop, T1/T2, "Only enter after a 1H MSS"); when price is already inside the WATCH zone, that alert waits at the zone's near edge, so it fires when price leaves the zone and comes back. | `coins` |
| `spot_status` | The plan's spot accumulation on OKX spot: price against the reference price, the next level below, RSI 1D and 1W, against SMA200 1W, monthly buys done and next (skipped dates do not count), lump-sum parts left and how they are spread after the last date, and whether today is a SPOT ZONE, with the reasons. | `date` (default today) |

Indicators are found by `.ind` path, ID (`roman.roman-setup`), short name
(`roman-setup`) or title, in `~/.local/share/orbit-tape/indicators/`, then
the copies the `.deb` ships in `/usr/share/doc/orbit-tape/indicators/`. `inputs`
take values as text by input ID or label, for example `{"ctMode": "true"}`;
`list_indicators` shows the IDs.

Your own copy always runs. When it has the ID of an indicator the `.deb`
ships but an older `version` (or none, while the shipped one has one), the
tools that run it (`scan`, `run_indicator`, `key_levels`, `market_snapshot`,
`roman_setups`) add a `warnings` list with the `cp` command that updates it,
and `list_indicators` shows it as `update` on that indicator. Without a
version on either side, a copy whose text differs is named the same way.

`check_indicator`, `test_indicator` and `run_indicator` read only `.ind`
sources: a path must be a folder (scanned for `.ind` files, links skipped) or
a regular `.ind` file. A link named `.ind` that points at any other file is
refused.

`get_candles` and `run_indicator` also return `history_note` when the
exchange had fewer bars than asked for (a coin listed recently, or the
exchange's own limit); it is null otherwise.

Errors (unknown symbols, timeframes, indicators or inputs, and network
failures) come back as tool errors with a short message; the server keeps
running.

**Speed.** Candles are fetched for several coins at once (6 on OKX, 8 on
Binance), paced by each exchange's rate limit; OKX allows fewer requests, so
a 66-coin `scan` on OKX takes about a minute, on Binance less. Fetched
candles are reused until the next bar closes, so a second tool on the same
coins and timeframe answers at once. `key_levels` fetches up to a year of 1h
candles.

## Daily routine

The routine tools and the `daily_report` prompt run a daily check of your
plan: a market snapshot, Roman setups near a level, a few alerts on the
levels that matter, and your spot accumulation, as one short report.

**The plan file** is `~/.config/orbit-tape/plan.toml` (`$ORBIT_TAPE_PLAN`
moves it). Start from the example, then put in your own numbers:

```sh
mkdir -p ~/.config/orbit-tape
cp /usr/share/doc/orbit-tape/plan.example.toml ~/.config/orbit-tape/plan.toml
```

It holds the prop rules (risk %, counter-trend risk %, the most correlated
trades, the widest stop, leverage per coin), the coin universe and exchange,
Roman Setup inputs with per-coin overrides, the optional spot plan (pair,
holdings, goals, reference price, heads-up and buy levels with amounts and
`used`, monthly dates with an optional `skipped` list, the SPOT ZONE
distance) and the report settings (`max_new_alerts`, `alert_channel`,
`footer`, `model`). Unknown keys are errors that name the key. The file stays on your
machine; nothing in it is sent anywhere except to the Claude session you
run the report in.

**The prompt.** In Claude Code, `/mcp__orbit-tape__daily_report` runs it by
hand. It calls `get_plan`, `market_snapshot`, `roman_setups` and
`spot_status`, creates at most `max_new_alerts` alerts after checking
`list_alerts` for duplicates (the nearest BTC or ETH bias-flip level first,
then the nearest WATCH levels; never spot alerts), and replies with the
report ending in your `footer`.

**Every day, unattended.** `orbit-tape report` runs the prompt once with
`claude -p`, with only this program's MCP server and only the six tools the
routine uses allowed (it can create alerts, not change, pause or delete them),
saves the answer to `~/.local/share/orbit-tape/reports/YYYY-MM-DD.md`
(readable by you only) and sends it to the desktop and/or ntfy (the plan's
`alert_channel`, or `--channel`). A failed run is sent too, with the reason.
Each run uses the Claude plan or API key `claude` is logged in with, and the
plan's `model` (default `sonnet`, cheaper than the default model; `--model`
overrides it for one run). The timer is off until you turn it on:

```sh
systemctl --user enable --now orbit-tape-report.timer   # daily at 11:05
systemctl --user edit orbit-tape-report.timer           # another time
journalctl --user -u orbit-tape-report                  # what happened
orbit-tape report --no-send                             # try it once now
```

If the service cannot find `claude`, set `Environment=ORBIT_TAPE_CLAUDE=/path/to/claude`
with `systemctl --user edit orbit-tape-report.service`.

## Alerts

Alerts are saved in `~/.local/share/orbit-tape/alerts.json`, shared by the
app, `orbit-tape watch` and the MCP server. Alerts made with `create_alert`
show up in the app's alert list (the bell in the sidebar) marked "Claude",
and price alerts also as lines on charts of that market. Right-click price
alerts made in the app are saved there too.

- **Price alerts** follow live trades. `crosses_above` fires when the price
  goes from below the level to at or above it; a price that is already above
  waits until it comes back and crosses again. `crosses` fires either way.
- **Indicator alerts** run an installed indicator (with your inputs) on the
  closed bars of their timeframe, a few seconds after each bar closes, and
  fire when one of its alert conditions (`plot(..., alert = true)`) is true.
  Leave out `alert` to fire on any of them.
- **`once`** fires and turns the alert off (status `fired`; resume it to
  re-arm). **`every_bar`** keeps going: indicator alerts on every bar the
  condition is true, price alerts on the cross and then once per bar while
  the price stays past the level. Defaults: `once` for price alerts,
  `every_bar` for indicator alerts.
- **Channels:** `desktop` notifications, `ntfy` push messages to your phone,
  or `both`. ntfy needs a topic in Settings > Alerts (see below).
- **Markets:** USDT perpetuals on Binance or OKX by default; with
  `market: "spot"`, OKX USDT spot pairs (e.g. `BTC-USDT`). Spot alerts show
  `OKX spot` in their summary.
- At most 50 alerts can be active. Everything is checked before an alert is
  saved: the exchange, that the symbol is a listed USDT perpetual (or OKX
  spot pair), the timeframe, the indicator, the alert condition, the inputs
  and the price.

### Alerts while the app is closed

Alerts fire while the Orbit Tape app is open. To keep them running when it is
closed, turn on the watcher, a systemd user service the `.deb` installs (off
by default):

```sh
systemctl --user enable --now orbit-tape-watch.service   # start, and at every login
journalctl --user -u orbit-tape-watch -f                 # what it does
systemctl --user disable --now orbit-tape-watch.service  # stop
```

Only one program runs the alerts at a time, so nothing fires twice: while the
app is open it runs them and the watcher waits; when the app quits, the
watcher takes over. The alert list says which one runs them, and
`create_alert` and `list_alerts` say whether either is running.

When the watcher (or the app) starts, or the computer wakes from sleep,
indicator alerts catch up on a bar that closed in the meantime if it closed
at most one bar ago; it is labelled "missed while closed". Older bars are
skipped. A price cross while the connection was down is caught from the
one-minute candles of the gap.

### ntfy (phone)

[ntfy](https://ntfy.sh) delivers push messages to its phone app. In Orbit
Tape, open Settings > Alerts > Phone (ntfy), enter a topic and press Save,
then subscribe to the same topic in the ntfy app. Send test checks it. The
server is `https://ntfy.sh` unless you enter your own. Anyone who knows the
topic can read the messages, so pick one that is hard to guess. "Chart alerts
to ntfy too" also sends the open charts' indicator alerts there.

## Example questions

- "Scan my list with Roman Setup on the 1h chart. Anything in the last 3 bars?"
- "Run Roman Setup in counter-trend mode on ETH 4h and explain the current
  bias and the open zones."
- "What are BTC's key levels right now, and which are closest above and below
  price?"
- "Show the last 50 daily candles for SOL on OKX and summarise the trend."
- "Which alerts fired in Orbit Tape today?"
- "Alert me when BTC crosses 62,000 on OKX." (a `crosses` price alert,
  `once`, to the desktop)
- "Alert me on any Roman Setup signal on ETH 1H, on my phone too." (an
  `indicator` alert on `roman-setup`, `every_bar`, `both`)
- "Pause all my alerts." / "Which alerts do I have, and did any fail?"
- "Run my daily report." (the `daily_report` prompt)
- "Which coins in my plan are within 5% of a Roman level?" (`roman_setups`)
- "Check `~/.local/share/orbit-tape/indicators/my-rsi.ind` and fix the errors."

Nothing the server returns is financial advice.
