//! JSON files that several processes change: the app, `orbit-tape watch`
//! and `orbit-tape mcp`.
//!
//! Every change takes an exclusive lock on a `<name>.lock` file next to the
//! data, reads the current file, applies the change and replaces the file
//! with a temporary one in a single rename. Readers that do not lock still
//! see either the old or the new file, never half of one. The lock is
//! released when the process ends, also when it crashes.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Reads `path`. A missing file gives the default value; a file that cannot
/// be parsed is an error, so callers never overwrite data they could not read.
pub fn read<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read_to_string(path) {
        Ok(json) => serde_json::from_str(&json).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error),
    }
}

/// Applies `change` to the file's contents under the lock and saves the
/// result. A file that cannot be parsed is kept as `<name>.broken` and the
/// change starts from the default value.
pub fn update<T, R>(path: &Path, change: impl FnOnce(&mut T) -> R) -> io::Result<R>
where
    T: DeserializeOwned + Serialize + Default,
{
    let _lock = lock(path)?;
    let mut value = match read::<T>(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            let broken = path.with_extension("broken");
            log::warn!(
                "{error}; keeping it as {} and starting over",
                broken.display()
            );
            fs::rename(path, &broken)?;
            T::default()
        }
        Err(error) => return Err(error),
    };
    let result = change(&mut value);
    let json = serde_json::to_string_pretty(&value).map_err(io::Error::other)?;
    replace(path, json.as_bytes())?;
    Ok(result)
}

/// When the file last changed, or `None` when it does not exist. Cheap
/// enough to poll.
pub fn modified(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Holds the exclusive lock of `path` until dropped.
pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Waits for the exclusive lock of `path`.
pub fn lock(path: &Path) -> io::Result<Lock> {
    let file = open_lock_file(path)?;
    file.lock()?;
    Ok(Lock(file))
}

/// Takes the exclusive lock of `path` when nobody holds it.
pub fn try_lock(path: &Path) -> io::Result<Option<Lock>> {
    let file = open_lock_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Lock(file))),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(path))
}

/// Writes `bytes` to a temporary file and renames it over `path`.
fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(name);
    // Owner-only: the alerts file holds the ntfy topic (anyone who knows it
    // can read the messages) and maybe a server address with a password.
    // The rename also replaces an older, more open file with this one.
    let result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, path)
        });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "orbit-tape-shared-file-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn concurrent_updates_are_not_lost() {
        let dir = temp_dir("concurrent");
        let path = dir.join("numbers.json");
        let threads: Vec<_> = (0..8)
            .map(|thread| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for n in 0..25 {
                        update(&path, |numbers: &mut BTreeSet<u32>| {
                            numbers.insert(thread * 100 + n)
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let numbers: BTreeSet<u32> = read(&path).unwrap();
        assert_eq!(numbers.len(), 200);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_kept_aside() {
        let dir = temp_dir("broken");
        let path = dir.join("numbers.json");
        fs::write(&path, "not json").unwrap();
        assert!(read::<BTreeSet<u32>>(&path).is_err());
        update(&path, |numbers: &mut BTreeSet<u32>| numbers.insert(1)).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("numbers.broken")).unwrap(),
            "not json"
        );
        assert_eq!(read::<BTreeSet<u32>>(&path).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn written_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let path = dir.join("shared.json");
        fs::write(&path, "[]").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        update(&path, |list: &mut Vec<u32>| list.push(1)).unwrap();
        for file in [path.clone(), lock_path(&path)] {
            let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", file.display());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn try_lock_sees_a_held_lock() {
        let dir = temp_dir("lock");
        let path = dir.join("engine");
        let held = try_lock(&path).unwrap();
        assert!(held.is_some());
        // flock locks belong to the open file, so a second open in the same
        // process sees it held.
        assert!(try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(try_lock(&path).unwrap().is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
