# MCP server

`orbit-tape mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdin and stdout. It lets an AI assistant such as Claude scan
markets, read candles and run your Orbit Tape indicators, from the same
exchange adapters and indicator engine the app uses.

It is **read-only**:

- No API keys and no accounts: it only reads public candles from Binance or OKX
  USDT perpetuals.
- No orders, and no file writes. `scan` never creates `scan-symbols.txt`, and
  `alert_history` only reads the app's saved history.
- No window, and no connection to a running Orbit Tape.

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
or `okx` (USDT perpetuals); symbols can be written as `BTC`, `BTCUSDT` or
`BTC-USDT-SWAP`, and Binance's 1000x contracts (`1000PEPEUSDT`) are found
from `PEPE`. Timeframes: `1m 3m 5m 15m 30m 1h 2h 4h 12h 1d`.

| Tool | What it does | Main parameters |
| --- | --- | --- |
| `scan` | Runs an indicator over many coins and lists the alerts that fired on the last closed bar(s). Default: Roman Trade Setup on 1h over your `scan-symbols.txt` (its `exchange =` line, else OKX). | `indicator`, `timeframe`, `exchange`, `symbols` (at most 100), `lookback` (1 to 50), `inputs` |
| `get_candles` | Closed OHLCV candles, oldest first. | `symbol`, `timeframe`, `exchange` (default binance), `limit` (default 100, at most 500) |
| `run_indicator` | Runs one indicator on one coin: every plot value, markers, fired alerts and the info panel for the last N bars. | `indicator`, `symbol`, `timeframe`, `exchange`, `inputs`, `last` (default 20, at most 200), `bars` (default 2000, at most 20000) |
| `key_levels` | Current Key Levels (4H, daily, weekly, monthly, quarterly and yearly opens, previous highs, lows and mids, Monday range) sorted by price, each marked above or below the last close. Runs `key-levels.ind` on 1h candles back to the start of the year or the previous quarter. | `symbol`, `exchange` |
| `check_indicator` | Same as `orbit-tape-indicator check`. | `path` (a `.ind` file or folder), `namespace` |
| `test_indicator` | Same as `orbit-tape-indicator test`: check, then run on synthetic bars. | `path`, `namespace` |
| `list_indicators` | Your installed indicators: name, ID, path, inputs with defaults and options, and alert conditions. | none |
| `alert_history` | The newest alerts the app fired, from its saved history. | `last` (default 20, at most 200) |

Indicators are found by `.ind` path, ID (`roman.roman-setup`), short name
(`roman-setup`) or title, in `~/.local/share/orbit-tape/indicators/`, then
the copies the `.deb` ships in `/usr/share/doc/orbit-tape/indicators/`. `inputs`
take values as text by input ID or label, for example `{"ctMode": "true"}`;
`list_indicators` shows the IDs.

Errors (unknown symbols, timeframes, indicators or inputs, and network
failures) come back as tool errors with a short message; the server keeps
running.

**Speed.** Candles are fetched one coin at a time to stay inside the
exchanges' rate limits. OKX is slower than Binance (about 3 s per coin against
1.5 s), so a 20-coin `scan` on OKX takes about a minute. `key_levels` fetches
up to a year of 1h candles.

## Example questions

- "Scan my list with Roman Setup on the 1h chart. Anything in the last 3 bars?"
- "Run Roman Setup in counter-trend mode on ETH 4h and explain the current
  bias and the open zones."
- "What are BTC's key levels right now, and which are closest above and below
  price?"
- "Show the last 50 daily candles for SOL on OKX and summarise the trend."
- "Which alerts fired in Orbit Tape today?"
- "Check `~/.local/share/orbit-tape/indicators/my-rsi.ind` and fix the errors."

Nothing the server returns is financial advice.
