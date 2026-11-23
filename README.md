<div align="center">
  <img src="assets/app-icon/hicolor/256x256/apps/io.github.atraxsrc.OrbitTape.png" alt="Orbit Tape icon" width="128" />

# Orbit Tape

**Order flow on orbit - a native crypto charting desk for COSMIC**

[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Rust 2024](https://img.shields.io/badge/Rust-2024-orange.svg?logo=rust)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Made with iced](https://iced.rs/badge.svg)](https://github.com/iced-rs/iced)
[![Built for COSMIC](https://img.shields.io/badge/built%20for-COSMIC-48b9c7.svg)](https://system76.com/cosmic)
[![Linux](https://img.shields.io/badge/platform-Linux-lightgrey.svg?logo=linux&logoColor=white)](#install)

</div>

Orbit Tape is a native desktop charting app for crypto markets: order book
heatmaps, footprint and candlestick charts, time and sales and a DOM ladder,
streamed straight from the exchanges, with scriptable indicators and desktop
alerts. It is for discretionary traders who read order flow and want a fast,
local tool instead of a browser tab.

It runs on **Linux only** and is made for the [COSMIC](https://system76.com/cosmic)
desktop on Pop!_OS. Orbit Tape is a fork of
[Flowsurface](https://github.com/flowsurface-rs/flowsurface) and builds on its
work; see [Credits](#credits).

<div align="center">
  <!-- TODO: replace with a COSMIC screenshot after the COSMIC UI work. -->
  <img src="assets/screenshot.png" alt="Orbit Tape screenshot" style="max-width: 100%; height: auto;" />
</div>

## Features

- **Pane types**
    - **Time & Sales:** scrollable list of live trades, mergeable across tickers.
    - **DOM / Ladder:** the current order book next to recent traded volume on
      grouped price levels.
    - **Heatmap (historical DOM):** live trades and order book as a time-series
      heatmap, with adjustable price grouping and timeframes.
    - **Candlestick:** footprints, volume and TPO profiles, big trades, custom
      tick-based intervals and scriptable indicators. See [Studies](#studies).
- **Scriptable indicators** in a Pine-like language, with higher-timeframe data,
  Pine-style drawing and an indicator CLI. See [Custom indicators](#custom-indicators).
- **Indicator alerts:** boolean plots marked `alert = true` send a desktop
  notification and a chime when they are true on a newly closed bar
  (Settings → Alerts).
- **Price alerts:** right-click the price chart and pick "Add alert at <price>".
  The alert line fires once when the live price crosses it, then stays on the
  chart as "Alert fired" until you right-click it and re-arm it. Right-click
  and "Remove alert" deletes the line; to keep the line, untick "Alert" in its
  toolbar. Any horizontal line can be turned into an alert from its toolbar.
  The same menu can remove all alerts, drawings, indicators or studies at once.
- **Trade sound effects** driven by the live trade stream.
- **Multi-window:** pop panes out into their own windows.
- **Pane linking:** panes in the same link group switch ticker together.
- **Saved layouts, chart presets and themes** with editable color palettes.
- **Exchanges:** Binance, Bybit, Hyperliquid, OKX and MEXC (spot and perpetuals
  where the exchange offers them).

### Market data

**Live:** trades, order book and kline updates stream from the exchanges'
WebSocket feeds.

**Historical trades:** optional backfill for the visible range on time-based
candlestick charts, set in **Settings → Network**:

- **Off:** no backfill.
- **Exchange:** fetches from Binance's public REST API and bulk data mirrors
  ([data.binance.vision](https://data.binance.vision/)). Daily archives are
  cached on disk; intraday ranges are paginated and rate-limited. **Binance
  only** (spot, linear, inverse).
- **Server:** fetches from a server exposing `GET /trades.arrow`, such as the
  upstream [`flowsurface-server`](https://github.com/flowsurface-rs/server)
  collector. Works with every supported exchange the server collects.

**Historical klines, open interest and ticker metadata** always come from the
exchanges' REST APIs.

### Studies

Footprints need **Trade data** enabled in the pane settings and enough zoom for
their columns to show.

- A pane uses one profile source, **candle-based** or **trade-based**. Studies
  of the same source can coexist (one visible-range profile, one recurring
  profile, one TPO); **Big Trades** is trade-only and independent.
- **Candle-based** studies use lower-timeframe klines (`1m` up to `2h`, `3m` up
  to `4h`, `5m` up to `12h`, `15m` above). They approximate: a candle's volume
  is spread over its range and counts as buying when `close >= open`.
- **Trade-based** studies use actual prints: more precise, but they need trade
  data and more processing.
- TPO anchors are UTC; the initial balance is the first hour of the profile.
- Big Trades filters on quote-currency notional; rings group several trades.

## COSMIC status

What is COSMIC-specific today:

- **Launcher and window integration:** the app ID `io.github.atraxsrc.OrbitTape`
  is the Wayland app_id and matches the `.desktop` entry, so COSMIC shows the
  right icon and groups the windows in the dock.
- **Original icon** in the hicolor theme, sized from 16 to 512 px.
- **Wayland-only** rendering (no X11 backend).
- **COSMIC theme** (Settings → Theme, on by default): the app reads COSMIC's
  theme files and follows its light/dark mode, accent, background, text,
  success/warning/destructive colors and corner radii live. The 8 color
  swatches for drawings, indicators and series come from the COSMIC palette
  (accent_blue, indigo, purple, pink, red, orange, yellow, green).
- **Frosted glass:** a see-through, compositor-blurred window that follows
  COSMIC's frosted-window setting, or a hand-set opacity.
- **Desktop notifications** through the freedesktop notification service that
  COSMIC provides.

Planned:

- A port of the UI to [libcosmic](https://github.com/pop-os/libcosmic), with a
  native header bar and COSMIC widgets.
- Settings integration with COSMIC Settings.

## Install

### .deb (recommended)

Coming soon - see [Releases](https://github.com/atraxsrc/orbit-tape/releases).
Once a release is published:

1. Download `orbit-tape_<version>_amd64.deb` from the
   [latest release](https://github.com/atraxsrc/orbit-tape/releases/latest).
   Optionally check it against `SHA256SUMS` from the same release:
   `sha256sum -c SHA256SUMS --ignore-missing`.
2. Install it:

   ```sh
   sudo apt install ./orbit-tape_*_amd64.deb
   ```

3. Start **Orbit Tape** from the COSMIC launcher.

Updates are manual for now: download the new release and run the same
command. Uninstall with `sudo apt remove orbit-tape`; your settings in
`~/.local/share/orbit-tape` are kept.

The package also installs `orbit-tape-indicator`, the indicator CLI.

### Build from source (Pop!_OS 24.04)

```sh
sudo apt install build-essential pkg-config libasound2-dev git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust via rustup

git clone https://github.com/atraxsrc/orbit-tape
cd orbit-tape
cargo build --release --locked
./target/release/orbit-tape
```

`rust-toolchain.toml` pins the Rust version; rustup installs it on the first
build. To build the `.deb` yourself: `scripts/package-deb.sh` (installs
`cargo-deb` if needed and writes `target/debian/orbit-tape_<version>-1_amd64.deb`).

## Custom indicators

Indicators are `.ind` text files. Put them in
`~/.local/share/orbit-tape/indicators/`, then restart the app:

```sh
orbit-tape-indicator new ~/.local/share/orbit-tape/indicators/community --id community.example --title "Example"
orbit-tape-indicator check ~/.local/share/orbit-tape/indicators/community/example.ind
orbit-tape-indicator test ~/.local/share/orbit-tape/indicators/community/example.ind
```

Then add it to a candlestick chart from the indicator sidebar (**+**).

- Step-by-step guide: [docs/custom-indicators.md](docs/custom-indicators.md)
- Language reference: [indicator-engine/LANGUAGE.md](indicator-engine/LANGUAGE.md)
- Commented examples: [examples/indicators/](examples/indicators/)
- Optional Roman Setup and Order Block ports: [indicators/](indicators/README.md)
- Shared indicators: the in-app catalog browser lists the upstream
  [Flowsurface indicator catalog](https://github.com/flowsurface-rs/indicators).

## Where files are stored

Everything lives in one folder, `~/.local/share/orbit-tape/`
(`$XDG_DATA_HOME/orbit-tape`):

| What | Path |
| --- | --- |
| Settings, layouts, themes, presets | `~/.local/share/orbit-tape/saved-state.json` |
| User indicators | `~/.local/share/orbit-tape/indicators/` |
| Logs | `~/.local/share/orbit-tape/orbit-tape-current.log` |
| Market data cache (Binance archives) | `~/.local/share/orbit-tape/market_data/` |
| Proxy and server credentials | the system keyring (service `orbit-tape.proxy` / `orbit-tape.server`) |

`ORBIT_TAPE_DATA_PATH` moves the whole folder and `ORBIT_TAPE_INDICATORS_PATH`
just the indicators. **Settings → Open data folder** opens it.

**Coming from Flowsurface:** on the first start, if
`~/.local/share/orbit-tape` does not exist yet and `~/.local/share/flowsurface`
does, Orbit Tape copies the old folder (except the re-downloadable
`market_data` cache and old logs). The Flowsurface folder is left untouched.
Stored proxy and server credentials are read from the old keyring entries too.

## Roadmap

- libcosmic UI port with a native header bar
- `.deb` releases on GitHub for every version
- A Flatpak, possibly
- More example indicators
- Live reloading of user indicators without a restart
- An in-app list of fired alerts

## Contributing and building

```sh
cargo build                                  # debug build
cargo fmt --all --check                      # formatting
cargo clippy --workspace --all-targets       # lints
cargo test --workspace                       # tests
cargo run -p orbit-tape-indicator-engine --bin orbit-tape-indicator -- \
    test data/indicators/sources --namespace core
```

- Formatting is `rustfmt` with the defaults; CI runs `cargo fmt --check` and
  `cargo clippy -- -D warnings`.
- Commits are small and focused, with a short `area: what changed` subject
  (for example `deb: cargo-deb packaging`) and a body that explains why.
- Changes follow the license of the component they touch (see [License](#license)).

Code map: [docs/ARCHITECTURE-NOTES.md](docs/ARCHITECTURE-NOTES.md).

## Privacy

Orbit Tape has no telemetry, analytics or accounts. It talks to the exchanges
you use and, only when you open it, the upstream indicator catalog. See
[PRIVACY.md](PRIVACY.md) for details.

## Credits

- [Flowsurface](https://github.com/flowsurface-rs/flowsurface) and its
  contributors: Orbit Tape is built on their app, indicator engine and
  exchange adapters.
- [iced](https://github.com/iced-rs/iced), the GUI library underneath.
- [Kraken Desktop](https://www.kraken.com/desktop) (formerly Cryptowatch), the
  inspiration Flowsurface credits for the project.
- [Halloy](https://github.com/squidowl/halloy), the IRC client Flowsurface
  credits as the reference for its code design.
- [System76](https://system76.com/) for COSMIC and Pop!_OS.

## License

Orbit Tape is a fork of [Flowsurface](https://github.com/flowsurface-rs/flowsurface) and is released under [GPL-3.0-or-later](LICENSE). Some components use separate licenses:

| Component | License |
|---|---|
| Application, data crate (`orbit-tape-data`), [bundled indicators](data/indicators/README.md), and [example indicators](examples/indicators/) | [GPL-3.0-or-later](LICENSE) |
| Indicator engine and CLI (`orbit-tape-indicator-engine`, `orbit-tape-indicator`) | [Apache-2.0](indicator-engine/LICENSE) |
| [Indicator catalog](https://github.com/flowsurface-rs/indicators) and its documentation | [Apache-2.0](https://github.com/flowsurface-rs/indicators/blob/main/LICENSE) |
| Exchange adapters (`orbit-tape-exchange`) | [MIT](exchange/LICENSE) |
