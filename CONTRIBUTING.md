# Contributing to Orbit Tape

Thanks for taking a look. Orbit Tape is a small, Linux-only project run by
one person, so the roadmap is mostly polish (see the README's
[Roadmap](README.md#roadmap)). Bug reports, fixes, new example indicators and
docs improvements are the most welcome contributions. For a bigger change,
open an issue first so we can agree on the approach before you spend time
on it.

## Reporting bugs

Use the bug report form under
[Issues](https://github.com/atraxsrc/orbit-tape/issues/new/choose). Include:

- the version (`orbit-tape --version`, or Settings in the app),
- your distribution and desktop (Orbit Tape targets COSMIC on Pop!_OS),
- what you did, what you expected and what happened,
- relevant lines from `~/.local/share/orbit-tape/orbit-tape-current.log`.

Check the log for anything private (proxy hosts, server addresses, ntfy
topics) before you paste it. Security problems go through private reporting
instead, see [SECURITY.md](SECURITY.md).

## Building

Build dependencies on Pop!_OS / Ubuntu:

```sh
sudo apt install build-essential pkg-config libasound2-dev git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust via rustup
```

`rust-toolchain.toml` pins the Rust version (CI uses the same one), and
rustup installs it on the first build.

```sh
cargo build                       # debug build
cargo run                         # start the app
cargo run -- --help               # the headless commands
```

Workspace crates and where things live: [docs/ARCHITECTURE-NOTES.md](docs/ARCHITECTURE-NOTES.md).

## Checks

CI runs these on every pull request; run them before you push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run -p orbit-tape-indicator-engine --bin orbit-tape-indicator -- check indicators/roman
cargo run -p orbit-tape-indicator-engine --bin orbit-tape-indicator -- check indicators/market
cargo run -p orbit-tape-indicator-engine --bin orbit-tape-indicator -- check examples/indicators
cargo run -p orbit-tape-indicator-engine --bin orbit-tape-indicator -- test data/indicators/sources --namespace core
sh scripts/check-private-files.sh
```

Run clippy on the pinned toolchain (the default inside this repo): newer
Rust versions can report different lints. The tests need no network and no
exchange account.

## Pull requests

- Keep each pull request to one change, and each commit small and focused.
- Commit subjects are `area: what changed`, lower case, no trailing period
  (for example `alerts: fire spot alerts on OKX`), with a body that says why.
- Add or update tests for behaviour changes. Indicator ports are checked bar
  for bar against recorded data in `indicators/*/tests/`.
- Update the README or `docs/` when you change something users see.
- UI changes: add a screenshot to the pull request.
- Don't commit personal data: emails, home paths, API keys, tokens, ntfy
  topics or your own trading plan. `scripts/check-private-files.sh` blocks
  the usual local-only files.

## Indicators

New example indicators go in `examples/indicators/`, with comments that
explain each step. Start with [docs/custom-indicators.md](docs/custom-indicators.md)
and the language reference [indicator-engine/LANGUAGE.md](indicator-engine/LANGUAGE.md).
Indicators you want to share with other users fit better in the community
catalog, [orbit-tape-indicators](https://github.com/atraxsrc/orbit-tape-indicators).

Only port scripts whose license allows it, keep the original's license and
credit in an SPDX header, and never translate code that has no license.

## License

By contributing, you agree that your contribution is released under the
license of the component it touches: GPL-3.0-or-later for the app, data crate
and indicators, Apache-2.0 for the indicator engine and MIT for the exchange
adapters (see the README's [License](README.md#license) table).
