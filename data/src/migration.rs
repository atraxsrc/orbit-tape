//! One-time copy of a Flowsurface data directory into Orbit Tape's.
//!
//! Orbit Tape keeps everything (settings, layouts, themes, user indicators,
//! logs) in `$XDG_DATA_HOME/orbit-tape`. When that folder does not exist yet
//! and an old `$XDG_DATA_HOME/flowsurface` does, the old folder is copied.
//! The old folder is never moved, changed or deleted, so Flowsurface keeps
//! working. Re-downloadable market data and old log files are not copied.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Data directory override read when the new variable is not set.
pub const LEGACY_DATA_PATH_ENV: &str = "FLOWSURFACE_DATA_PATH";

const LEGACY_DIR_NAME: &str = "flowsurface";

/// Top-level entries that are left behind: a cache the app fills again, and
/// the old app's logs.
const SKIPPED: &[&str] = &["market_data"];

#[derive(Debug)]
pub enum Outcome {
    /// Nothing to do: the new folder exists, there is no old one, or the data
    /// path is overridden by an environment variable.
    NotNeeded,
    Copied {
        from: PathBuf,
        to: PathBuf,
    },
    Failed {
        from: PathBuf,
        to: PathBuf,
        error: io::Error,
    },
}

/// Copies the old Flowsurface folder on first run. Call before anything
/// writes to the data directory.
pub fn migrate_legacy_data_dir() -> Outcome {
    if std::env::var_os(crate::DATA_PATH_ENV).is_some()
        || std::env::var_os(LEGACY_DATA_PATH_ENV).is_some()
    {
        return Outcome::NotNeeded;
    }
    let Some(base) = dirs_next::data_dir() else {
        return Outcome::NotNeeded;
    };
    migrate(&base.join(LEGACY_DIR_NAME), &base.join(crate::APP_DIR_NAME))
}

fn migrate(from: &Path, to: &Path) -> Outcome {
    if to.exists() || !from.is_dir() {
        return Outcome::NotNeeded;
    }
    // Copy into a staging folder first, so a failed copy never leaves a
    // half-filled data directory that would stop a retry on the next start.
    let staging = to.with_extension("migrating");
    let result = (|| {
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        copy_dir(from, &staging, true)?;
        fs::rename(&staging, to)
    })();
    match result {
        Ok(()) => Outcome::Copied {
            from: from.to_owned(),
            to: to.to_owned(),
        },
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Outcome::Failed {
                from: from.to_owned(),
                to: to.to_owned(),
                error,
            }
        }
    }
}

fn copy_dir(from: &Path, to: &Path, top_level: bool) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if top_level && is_skipped(&name.to_string_lossy()) {
            continue;
        }
        let source = entry.path();
        let target = to.join(&name);
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
        } else if kind.is_dir() {
            copy_dir(&source, &target, false)?;
        } else if kind.is_file() {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

fn is_skipped(name: &str) -> bool {
    SKIPPED.contains(&name) || (name.starts_with("flowsurface-") && name.ends_with(".log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "orbit-tape-migration-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn copies_old_folder_once_and_keeps_it() {
        let root = temp_root("copy");
        let old = root.join("flowsurface");
        let new = root.join("orbit-tape");
        fs::create_dir_all(old.join("indicators/roman")).unwrap();
        fs::create_dir_all(old.join("market_data/binance")).unwrap();
        fs::write(old.join("saved-state.json"), "{}").unwrap();
        fs::write(old.join("indicators/roman/a.ind"), "x").unwrap();
        fs::write(old.join("market_data/binance/big.zip"), "zip").unwrap();
        fs::write(old.join("flowsurface-current.log"), "log").unwrap();

        assert!(matches!(migrate(&old, &new), Outcome::Copied { .. }));
        assert_eq!(
            fs::read_to_string(new.join("saved-state.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            fs::read_to_string(new.join("indicators/roman/a.ind")).unwrap(),
            "x"
        );
        assert!(!new.join("market_data").exists());
        assert!(!new.join("flowsurface-current.log").exists());
        assert!(old.join("market_data/binance/big.zip").exists());
        assert!(old.join("saved-state.json").exists());
        assert!(!root.join("orbit-tape.migrating").exists());

        fs::write(new.join("saved-state.json"), "new").unwrap();
        assert!(matches!(migrate(&old, &new), Outcome::NotNeeded));
        assert_eq!(
            fs::read_to_string(new.join("saved-state.json")).unwrap(),
            "new"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nothing_to_copy_without_old_folder() {
        let root = temp_root("none");
        assert!(matches!(
            migrate(&root.join("flowsurface"), &root.join("orbit-tape")),
            Outcome::NotNeeded
        ));
        assert!(!root.join("orbit-tape").exists());
        let _ = fs::remove_dir_all(root);
    }
}
