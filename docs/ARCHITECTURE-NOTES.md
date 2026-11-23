# Architecture notes (fork)

A short map of the parts of Flowsurface that matter for the fork's theming,
chart and indicator work. Based on upstream 0.9.0 (commit `bc545fc`).

## Workspace

| Crate | Path | Role |
| --- | --- | --- |
| `flowsurface` | `src/` | iced app: windows, dashboard, panes, widgets, modals |
| `flowsurface-data` | `data/` | Persisted state, layouts, theme palette, chart models, indicator host |
| `flowsurface-exchange` | `exchange/` | Exchange adapters (REST + WebSocket), market types |
| `flowsurface-indicator-engine` | `indicator-engine/` | Compiler and runtime for `.ind` indicator scripts (Apache-2.0) |

- iced **0.14.0** from crates.io (features: `wgpu`, `canvas`, `advanced`, `x11`,
  `wayland`, ...). Not a fork, so libcosmic (which ships its own iced fork) cannot
  simply be dropped in.
- Toolchain pinned to Rust 1.95.0 (`rust-toolchain.toml`), edition 2024.

## App shell

- `src/main.rs`: `iced::daemon` with `Flowsurface` as the state. `update`,
  `view(window)`, `theme(window)`, `subscription`. Multi-window.
- `subscription()` batches: window events, sidebar, exchange streams
  (`Dashboard::market_subscriptions`), a per-frame `Tick`, and hotkeys. A COSMIC
  theme watcher would be one more entry here.
- `src/modal/`: settings (`settings.rs`), theme editor, layout manager, network
  editor, audio. `src/screen/dashboard.rs` owns the pane grid.

## Theming

- `data/src/config/theme.rs`: `data::Theme(iced_core::Theme)` with custom serde
  (built-ins by name, custom themes as a palette). `default_theme()` is the
  "Flowsurface" custom palette. Accent colors for series are derived from the
  palette (`generated_theme_accents`, OKLCH hue placement away from signal hues).
- `src/theme.rs`: `ThemeState` holds the current theme and derived accents with a
  generation counter, so charts know when to recolor.
- `Flowsurface::replace_theme` swaps the theme and calls
  `Dashboard::theme_updated`, which pushes it to every pane
  (`content.update_theme`). This is the hook for live COSMIC updates.
- `src/style.rs`: every widget style is a function of `&Theme` and reads the
  extended palette (`background`, `secondary`, `primary`, `success`, `danger`,
  `warning`). Corner radii are hard-coded per style, not themed.
- Chart colors use roles plus shades (`ThemeColor { role, shade }` such as
  `success/base`) so candles and indicators follow the palette unless overridden
  per series.

COSMIC side (checked on this machine): `~/.config/cosmic/com.system76.CosmicTheme.Mode/v1/is_dark`
and `com.system76.CosmicTheme.{Dark,Light}/v1/` hold RON files per key
(`accent`, `background`, `primary`, `secondary`, `success`, `warning`,
`destructive`, `corner_radii`, ...). Plain RON, so they can be read without
libcosmic if the `cosmic-theme` crate drags in its iced fork.

## Settings persistence

- `data/src/config/state.rs`: `State` (serde, `#[serde(default)]`) written to
  `saved-state.json` in the data dir (`data::data_path`, override with
  `FLOWSURFACE_DATA_PATH`). Holds layouts, selected/custom theme, timezone,
  sidebar, scale factor, audio, network, flattened `BootSettings`.
- New settings (for example "Follow COSMIC theme") go into `State` with a serde
  default, so old files still load.
- Layouts (`data/src/layout/`) persist panes; each kline pane carries its chart
  config, including its indicator instances.

## Charts

- Kline (candlestick) charts: `src/widget/chart/kline.rs` is a custom iced
  `Widget` (not a plain `canvas::Program`) with a scene cache. Submodules:
  `scene/` (viewport, domain), `painting/` (plots, marks), `chrome/legend.rs`,
  `drawing.rs` (user drawings), `footprint.rs`, `studies/` (TPO, profiles).
  Text and some layers use `canvas::Frame`.
- Pane logic and settings UI: `src/screen/dashboard/pane/content/kline*.rs`,
  including `indicator_browser/` (catalog browser) and `settings.rs`.
- Data model: `data/src/chart/kline.rs` (`Config`, `ChartSettings`,
  `IndicatorInstance`, `IndicatorInstanceStore`, panels and layers) and
  `data/src/chart/kline/series.rs` (candle series).
- Heatmap uses a wgpu shader pipeline (`src/widget/chart/heatmap/`), not relevant
  for indicators.

## Indicators (already exist upstream)

Upstream already has a full indicator system, so Phase 2 extends it instead of
starting over.

- `indicator-engine/`: a Pine-like language (`LANGUAGE.md`). Inputs with types,
  defaults, groups and tooltips; `state`/`var`; records, arrays, functions;
  `ta.*` (sma, ema, rma, rsi, stdev, highest/lowest, pivothigh/pivotlow, tr,
  avwap, ...). Outputs: plots (line/step/histogram/area), fills, regions (boxes,
  persistent or snapshot), segments, horizontal levels, markers, table view.
  Closed bars commit state, the open bar previews (non-repainting by design).
- Limits that matter for Roman Setup: no access to other timeframes or symbols
  (HTF must be aggregated inside the script or the engine extended), no free text
  labels on the chart beyond segment/marker text, tables are a separate view,
  no alerts.
- `data/src/chart/kline/indicators/`: host side. `bundled.rs` embeds
  `data/indicators/sources/*.ind` (SMA/EMA via moving-averages, Bollinger, RSI,
  VWAP, anchored VWAP, volume, CVD, OI, bar analysis). `registry.rs` and
  `loader.rs` manage the catalog and user `.ind` files in
  `<data dir>/indicators/`. `core/runtime.rs` (`IndicatorHost`) evaluates
  instances per pane, handles dependencies between indicators and replay when
  history changes.
- Picker and settings dialog already exist (Indicators sidebar, `+` opens the
  catalog browser). Instances persist per pane in the layout.
- CLI: `flowsurface-indicator check|test` validates `.ind` sources.

## Exchange streams

- `exchange/src/adapter/hub/{binance,bybit,okex,hyperliquid,mexc}`: REST fetch and
  WebSocket streams per venue. `adapter/ws.rs` is the shared socket layer.
- `src/connector/`: app-side fetch scheduling (`fetcher.rs`), stream specs
  (`stream.rs`), catalog.
- `Dashboard::market_subscriptions` turns the panes' stream specs into iced
  `Subscription`s; events come back as `Message::MarketWsEvent` and are routed to
  panes, which update their series and indicator hosts.

## Alerts and notifications

- `src/notify.rs` handles in-app toasts. `src/audio.rs` plays trade sounds via
  `rodio`.
- Indicator alerts (fork): `plot(..., alert = true)` sets `PlotDescriptor::alert`
  (engine) and `OutputChannelSpec::alert` (host). Each kline pane owns an
  `AlertWatch` (`data/src/chart/kline/alerts.rs`) that remembers the newest closed
  bar it checked per indicator instance; the first look only sets that baseline,
  so history never fires. `Flowsurface::poll_alerts` collects hits from every
  pane twice a second and `src/alerts.rs` drops duplicates and muted conditions,
  then sends a freedesktop notification (`notify-rust`, on a background thread)
  and a synthesized chime. Settings live in `data::config::alerts::Alerts`
  (`State::alerts`).
