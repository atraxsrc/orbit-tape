//! Live reload for user indicators: watches the installed indicators
//! directory and every `--indicator-path` file, and reports a change once
//! the files have been quiet for a moment.

use ::notify::{EventKind, RecursiveMode, Watcher};
use iced::Subscription;
use iced::futures::channel::mpsc;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Editors write a file in several steps (truncate, write, rename); wait
/// until the burst is over before recompiling.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Emits `()` after indicator source files changed.
pub fn subscription() -> Subscription<()> {
    Subscription::run(watch)
}

fn watch() -> impl iced::futures::Stream<Item = ()> {
    iced::stream::channel(4, async |output: mpsc::Sender<()>| {
        // Keep the watcher alive for as long as the subscription runs.
        let _watcher = match start(output) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                log::warn!("indicator live reload disabled: {error}");
                None
            }
        };
        std::future::pending::<()>().await;
    })
}

/// Which file events matter: `.ind` files inside the installed directory and
/// the exact `--indicator-path` files.
struct Filter {
    installed: PathBuf,
    files: Vec<PathBuf>,
}

impl Filter {
    fn matches(&self, path: &Path) -> bool {
        let path = absolute(path);
        (path.starts_with(&self.installed)
            && path.extension().and_then(|extension| extension.to_str()) == Some("ind"))
            || self.files.contains(&path)
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn start(output: mpsc::Sender<()>) -> Result<::notify::RecommendedWatcher, String> {
    let (installed, files) = data::chart::kline::indicators::watched_indicator_paths()
        .ok_or_else(|| "indicator catalog is not initialized".to_owned())?;
    let filter = Filter {
        installed: absolute(&installed),
        files: files.iter().map(|path| absolute(path)).collect(),
    };

    let (raw_sender, raw_receiver) = std::sync::mpsc::channel::<()>();
    let mut watcher = ::notify::recommended_watcher(
        move |result: ::notify::Result<::notify::Event>| match result {
            Ok(event) => {
                if !matches!(event.kind, EventKind::Access(_))
                    && event.paths.iter().any(|path| filter.matches(path))
                {
                    let _ = raw_sender.send(());
                }
            }
            Err(error) => log::warn!("indicator watcher: {error}"),
        },
    )
    .map_err(|error| error.to_string())?;

    let installed = absolute(&installed);
    watcher
        .watch(&installed, RecursiveMode::Recursive)
        .map_err(|error| format!("{}: {error}", installed.display()))?;
    // Watch each extra file's folder, not the file: editors that save by
    // renaming a temporary file replace the inode a file watch would follow.
    let mut folders = files
        .iter()
        .filter_map(|path| absolute(path).parent().map(Path::to_path_buf))
        .filter(|folder| !folder.starts_with(&installed))
        .collect::<Vec<_>>();
    folders.sort();
    folders.dedup();
    for folder in folders {
        if let Err(error) = watcher.watch(&folder, RecursiveMode::NonRecursive) {
            log::warn!("indicator watcher: {}: {error}", folder.display());
        }
    }

    // Debounce on a plain thread. It ends when the watcher (and with it the
    // raw sender) is dropped.
    std::thread::Builder::new()
        .name("indicator-watch".to_owned())
        .spawn(move || {
            let mut output = output;
            while raw_receiver.recv().is_ok() {
                while raw_receiver.recv_timeout(DEBOUNCE).is_ok() {}
                // A full channel means a reload is already queued.
                if let Err(error) = output.try_send(())
                    && error.is_disconnected()
                {
                    break;
                }
            }
        })
        .map_err(|error| error.to_string())?;

    log::info!("indicator live reload: watching {}", installed.display());
    Ok(watcher)
}
