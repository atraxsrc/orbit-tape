//! Copies in the user's indicators folder that are older than the indicator
//! Orbit Tape ships with the same ID.
//!
//! The user's copy always runs (it may hold their own edits); this only says
//! when the shipped one is newer, with the command that updates the copy, so
//! an old script never quietly gives old results.

use indicator_engine::IndicatorVersion;
use std::path::{Path, PathBuf};

/// Where the .deb puts the shipped indicator scripts.
pub const SHIPPED_FOLDERS: [&str; 2] = [
    "/usr/share/doc/orbit-tape/indicators",
    "/usr/share/doc/orbit-tape/examples/indicators",
];

/// How deep indicator folders nest (`indicators/<namespace>/<name>.ind`).
const DEPTH: usize = 4;

/// A user copy that differs from the shipped indicator with the same ID.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateNotice {
    pub id: String,
    pub user_path: PathBuf,
    pub shipped_path: PathBuf,
    pub user_version: Option<String>,
    pub shipped_version: Option<String>,
    pub kind: NoticeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// The shipped copy has a higher version, or the user copy has none.
    Older,
    /// Neither has a version and their text differs: which is newer is
    /// unknown.
    Differs,
}

/// One indicator file: its ID, version and a hash of its text.
#[derive(Debug, Clone)]
struct Script {
    path: PathBuf,
    id: String,
    version: Option<IndicatorVersion>,
    hash: u64,
}

fn read(path: &Path) -> Option<Script> {
    let bytes = std::fs::read(path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let compiled = indicator_engine::compile(text, path).ok()?;
    Some(Script {
        path: path.to_path_buf(),
        id: compiled.descriptor.id.as_str().to_owned(),
        version: compiled.descriptor.version,
        hash: crate::util::stable_hash(&bytes),
    })
}

fn scripts(folder: &Path) -> Vec<Script> {
    let mut found = Vec::new();
    collect(folder, DEPTH, &mut found);
    found.sort();
    found.iter().filter_map(|path| read(path)).collect()
}

fn collect(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
        if path.is_dir() {
            if depth > 0 {
                collect(&path, depth - 1, found);
            }
        } else if path.extension().is_some_and(|extension| extension == "ind") {
            found.push(path);
        }
    }
}

fn compare(user: &Script, shipped: &Script) -> Option<NoticeKind> {
    match (&user.version, &shipped.version) {
        (Some(user), Some(shipped)) => (shipped > user).then_some(NoticeKind::Older),
        (None, Some(_)) => Some(NoticeKind::Older),
        (Some(_), None) => None,
        (None, None) => (user.hash != shipped.hash).then_some(NoticeKind::Differs),
    }
}

/// Every copy in `user_folder` that is older than (or, without versions,
/// differs from) the first shipped copy with its ID in `shipped_folders`.
pub fn find(user_folder: &Path, shipped_folders: &[PathBuf]) -> Vec<UpdateNotice> {
    let shipped: Vec<Script> = shipped_folders
        .iter()
        .filter(|folder| folder.as_path() != user_folder)
        .flat_map(|folder| scripts(folder))
        .collect();
    scripts(user_folder)
        .into_iter()
        .filter_map(|user| {
            let shipped = shipped.iter().find(|script| script.id == user.id)?;
            let kind = compare(&user, shipped)?;
            Some(UpdateNotice {
                id: user.id.clone(),
                user_path: user.path.clone(),
                shipped_path: shipped.path.clone(),
                user_version: user.version.as_ref().map(ToString::to_string),
                shipped_version: shipped.version.as_ref().map(ToString::to_string),
                kind,
            })
        })
        .collect()
}

/// The shipped folders that exist on this machine.
pub fn shipped_folders() -> Vec<PathBuf> {
    SHIPPED_FOLDERS
        .iter()
        .map(PathBuf::from)
        .filter(|folder| folder.is_dir())
        .collect()
}

/// `path` with the home folder shown as `~`.
fn display(path: &Path) -> String {
    let home = dirs_next::home_dir();
    let shown = match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    if shown.contains([' ', '\'', '"', '$', '`', '\\']) {
        format!("'{}'", shown.replace('\'', r"'\''"))
    } else {
        shown
    }
}

impl UpdateNotice {
    /// The command that replaces the user copy with the shipped one.
    pub fn copy_command(&self) -> String {
        format!(
            "cp {} {}",
            display(&self.shipped_path),
            display(&self.user_path)
        )
    }

    /// One paragraph for the app and the MCP tools.
    pub fn message(&self) -> String {
        let user = display(&self.user_path);
        let version = |version: &Option<String>| match version {
            Some(version) => format!("version {version}"),
            None => "no version".to_owned(),
        };
        let what = match self.kind {
            NoticeKind::Older => format!(
                "{user} ({}) is older than the {} Orbit Tape ships for {}.",
                version(&self.user_version),
                version(&self.shipped_version),
                self.id
            ),
            NoticeKind::Differs => format!(
                "{user} differs from the copy of {} Orbit Tape ships, and neither has a version, so \
                 which is newer is unknown.",
                self.id
            ),
        };
        format!(
            "{what} Your copy is the one that runs. To use the shipped one: {}",
            self.copy_command()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, version: Option<&str>, extra: &str) -> String {
        let version = version.map_or(String::new(), |version| {
            format!(", version = \"{version}\"")
        });
        format!(
            "language(1)\nindicator(\"Test\", id = \"{id}\"{version})\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close{extra})\n"
        )
    }

    fn write(folder: &Path, name: &str, text: &str) {
        let path = folder.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn older_or_unversioned_copies_are_named_with_the_command() {
        let root = std::env::temp_dir().join(format!("orbit-tape-updates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, shipped) = (root.join("user"), root.join("shipped"));
        // Older version, no version, newer version, same version edited,
        // both unversioned and different, both unversioned and the same, and
        // one the app does not ship.
        for (name, user_version, shipped_version, user_extra) in [
            ("a/older.ind", Some("1.0"), Some("1.1"), ""),
            ("a/none.ind", None, Some("7.1"), ""),
            ("a/newer.ind", Some("2.0"), Some("1.0"), ""),
            ("a/edited.ind", Some("1.0"), Some("1.0"), ", id = \"mine\""),
            ("a/differs.ind", None, None, ", id = \"mine\""),
            ("a/same.ind", None, None, ""),
        ] {
            let id = format!(
                "a.{}",
                name.trim_start_matches("a/").trim_end_matches(".ind")
            );
            write(&user, name, &source(&id, user_version, user_extra));
            write(&shipped, name, &source(&id, shipped_version, ""));
        }
        write(&user, "b/own.ind", &source("b.own", None, ""));
        let notices = find(&user, std::slice::from_ref(&shipped));
        let found: Vec<(&str, NoticeKind)> = notices
            .iter()
            .map(|notice| (notice.id.as_str(), notice.kind))
            .collect();
        assert_eq!(
            found,
            [
                ("a.differs", NoticeKind::Differs),
                ("a.none", NoticeKind::Older),
                ("a.older", NoticeKind::Older),
            ]
        );
        let none = &notices[1];
        let message = none.message();
        assert!(
            message.contains("(no version) is older than the version 7.1"),
            "{message}"
        );
        assert!(
            message.contains("Your copy is the one that runs"),
            "{message}"
        );
        assert_eq!(
            none.copy_command(),
            format!(
                "cp {} {}",
                shipped.join("a/none.ind").display(),
                user.join("a/none.ind").display()
            )
        );
        assert!(notices[0].message().contains("which is newer is unknown"));
        // The user folder listed as shipped too is not compared with itself.
        assert!(find(&user, std::slice::from_ref(&user)).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        assert_eq!(
            display(Path::new("/opt/my dir/x.ind")),
            "'/opt/my dir/x.ind'"
        );
        assert_eq!(display(Path::new("/opt/x.ind")), "/opt/x.ind");
    }
}
