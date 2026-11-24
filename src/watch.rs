//! `orbit-tape watch`: runs the saved alerts with no window, so they fire
//! while the app is closed. Meant to run as the systemd user service
//! `orbit-tape-watch.service`; logs go to stderr (the journal).
//!
//! Only one program runs the alerts at a time (see `alert_engine`). When
//! the app is open and running them, the watcher waits and takes over as
//! soon as the app quits.

use crate::alert_engine;
use data::config::alert_store::AlertStore;
use data::config::shared_file;

const USAGE: &str = "\
Usage: orbit-tape watch

Runs the alerts saved in Orbit Tape (alerts.json in the data folder) without
a window: price alerts on live trades, indicator alerts on each bar close.
Alerts go to the desktop and/or ntfy, as each alert says, and into the alert
history the app shows.

While the Orbit Tape app is open it runs the alerts itself; the watcher waits
and takes over when the app quits.

To run it in the background from login:
  systemctl --user enable --now orbit-tape-watch.service
Logs: journalctl --user -u orbit-tape-watch";

pub fn run(arguments: &[String]) -> i32 {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return 0;
    }
    if let Some(argument) = arguments.first() {
        eprintln!("orbit-tape watch: unknown argument '{argument}' (see --help)");
        return 1;
    }
    if let Err(error) = setup_logging() {
        eprintln!("orbit-tape watch: could not set up logging: {error}");
    }
    // Candle volume in USDT like the charts, so volume-based indicators match.
    exchange::unit::qty::set_preferred_currency(exchange::SizeUnit::Quote);

    let lock_path = AlertStore::engine_lock_path();
    let lock = match shared_file::try_lock(&lock_path) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            log::info!(
                "another Orbit Tape program (usually the app) runs the alerts; waiting until it stops"
            );
            match shared_file::lock(&lock_path) {
                Ok(lock) => lock,
                Err(error) => {
                    log::error!("could not take the alert engine lock: {error}");
                    return 1;
                }
            }
        }
        Err(error) => {
            log::error!("could not take the alert engine lock: {error}");
            return 1;
        }
    };
    let result = alert_engine::run_blocking(alert_engine::Paths::installed());
    drop(lock);
    match result {
        Ok(()) => 0,
        Err(error) => {
            log::error!("{error}");
            1
        }
    }
}

/// Info and above to stderr; the journal adds the time.
fn setup_logging() -> Result<(), log::SetLoggerError> {
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!("{}: {message}", record.level()));
        })
        .level(log::LevelFilter::Warn)
        .level_for("orbit_tape", log::LevelFilter::Info)
        .level_for("orbit_tape_data", log::LevelFilter::Info)
        .chain(std::io::stderr())
        .apply()
}
