use super::{IndicatorRegistration, language};
use crate::data_path;
use log::warn;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static INSTALLED_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

/// Filesystem sources used to build the process-wide indicator catalog.
///
/// Bundled sources are managed by the data crate, while installed paths
/// belong to the user or a development workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorCatalogPaths {
    /// Overrides embedded bundled sources when set.
    pub bundled_directory: Option<PathBuf>,
    pub installed_directory: PathBuf,
    pub additional_files: Vec<PathBuf>,
}

impl IndicatorCatalogPaths {
    fn from_data_directory() -> Self {
        Self {
            bundled_directory: None,
            installed_directory: default_installed_directory(),
            additional_files: Vec::new(),
        }
    }
}

fn default_installed_directory() -> PathBuf {
    std::env::var_os("ORBIT_TAPE_INDICATORS_PATH")
        .or_else(|| std::env::var_os("FLOWSURFACE_INDICATORS_PATH"))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_path(Some("indicators")))
}

fn installed_directory() -> Result<PathBuf, String> {
    INSTALLED_DIRECTORY
        .get()
        .cloned()
        .ok_or_else(|| "Indicator catalog is not initialized".to_owned())
}

fn installed_sources(directory: &Path) -> Result<Vec<language::LoadedIndicatorSource>, String> {
    let (files, _) = language::indicator_files_in_directory(directory)?;
    Ok(language::load_from_files_report(&files, language::PackageOrigin::Installed).loaded_sources)
}

pub fn write_indicator_source(id: &super::IndicatorId, source: &[u8]) -> Result<(), String> {
    let directory = installed_directory()?;
    if super::catalog()
        .get(id)
        .is_some_and(|registration| registration.origin == language::PackageOrigin::Bundled)
    {
        return Err("Bundled indicators cannot be replaced".to_owned());
    }

    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    if installed_sources(&directory)?
        .iter()
        .any(|loaded| loaded.id == id.as_str())
    {
        return Err(
            "An indicator with this ID already exists in the indicators directory".to_owned(),
        );
    }

    let (namespace, name) = id
        .as_str()
        .rsplit_once('.')
        .expect("Validated indicator ID has a namespace");
    let destination_directory = directory.join(namespace);
    let destination = destination_directory.join(format!("{name}.ind"));

    let source_text = std::str::from_utf8(source).map_err(|error| error.to_string())?;
    let report = language::load_compiled_report(
        [(
            destination.clone(),
            indicator_engine::compile(source_text, &destination),
        )],
        language::PackageOrigin::Installed,
    );
    if !report.diagnostics.is_empty() {
        return Err(report.diagnostics.join("\n"));
    }
    if report.registrations.len() != 1 || report.registrations[0].id != *id {
        return Err("Downloaded indicator does not declare the expected ID".to_owned());
    }

    fs::create_dir_all(&destination_directory).map_err(|error| error.to_string())?;
    // A non-source extension keeps interrupted writes out of source discovery on the same filesystem.
    let temporary = destination_directory.join(format!(".install-{}.part", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        use std::io::Write;
        file.write_all(source).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);

        // A hard link publishes the complete file atomically and refuses to replace an existing path.
        fs::hard_link(&temporary, &destination).map_err(|error| error.to_string())
    })();
    if let Err(error) = fs::remove_file(&temporary) {
        log::warn!("Could not remove indicator installation staging file: {error}");
    }
    result
}

pub fn remove_indicator_source(id: &super::IndicatorId) -> Result<(), String> {
    let directory = installed_directory()?;
    if super::catalog()
        .get(id)
        .is_some_and(|registration| registration.origin == language::PackageOrigin::Bundled)
    {
        return Err("Bundled indicators cannot be removed".to_owned());
    }

    let installed = installed_sources(&directory)?;
    let mut sources = installed.iter().filter(|source| source.id == id.as_str());
    let source = sources.next().ok_or_else(|| {
        "No matching source was found in the configured indicators directory".to_owned()
    })?;
    if sources.next().is_some() {
        return Err(
            "Multiple sources declare this indicator ID; remove the conflicting files manually"
                .to_owned(),
        );
    }
    fs::remove_file(&source.path).map_err(|error| error.to_string())
}

pub fn initialize_indicator_catalog() -> Result<(), String> {
    let additional_files =
        indicator_paths_from_args().map_err(|error| format!("Invalid indicator path: {error}"))?;
    let mut paths = IndicatorCatalogPaths::from_data_directory();
    paths.additional_files.extend(additional_files);

    initialize_indicator_catalog_with_paths(&paths)
        .map_err(|error| format!("Failed to load indicators: {error}"))
}

pub fn initialize_indicator_catalog_with_paths(
    paths: &IndicatorCatalogPaths,
) -> Result<(), String> {
    let _ = INSTALLED_DIRECTORY.set(paths.installed_directory.clone());
    let sources = load_catalog_sources(paths)?;
    let bundled_ids = sources
        .bundled
        .iter()
        .map(|registration| registration.id.clone())
        .collect::<Vec<_>>();

    super::install_catalog_with_bundled_diagnostics(
        sources.bundled,
        sources.installed,
        sources.diagnostics,
    )
    .map_err(|error| error.to_string())?;
    super::bundled::validate_bundled_catalog(&bundled_ids)
        .map_err(|error| format!("Failed to load bundled indicators: {error}"))
}

fn indicator_paths_from_args() -> Result<Vec<PathBuf>, String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--indicator-path" {
            let path = args
                .get(index + 1)
                .ok_or_else(|| "--indicator-path requires an explicit .ind file".to_owned())?;
            paths.push(validate_indicator_path(PathBuf::from(path))?);
            index += 2;
            continue;
        } else if let Some(path) = args[index].strip_prefix("--indicator-path=") {
            if path.trim().is_empty() {
                return Err("--indicator-path requires an explicit .ind file".to_owned());
            }
            paths.push(validate_indicator_path(PathBuf::from(path))?);
        }
        index += 1;
    }
    Ok(paths)
}

fn validate_indicator_path(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_dir() {
        return Err(format!(
            "{} is a directory; --indicator-path accepts an explicit .ind file",
            path.display()
        ));
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("ind") {
        return Err(format!(
            "{}: --indicator-path accepts only .ind source files",
            path.display()
        ));
    }
    Ok(path)
}

#[derive(Debug)]
struct IndicatorCatalogSources {
    bundled: Vec<IndicatorRegistration>,
    installed: Vec<IndicatorRegistration>,
    diagnostics: Vec<String>,
}

/// The last version of a watched source file that compiled, kept so a broken
/// save does not take the indicator off the chart.
#[derive(Debug, Clone)]
struct LastGood {
    hash: u64,
    registration: IndicatorRegistration,
}

/// What live reload needs to rebuild the catalog without touching bundled
/// sources.
#[derive(Debug)]
struct LoaderState {
    paths: IndicatorCatalogPaths,
    bundled: Vec<IndicatorRegistration>,
    last_good: HashMap<PathBuf, LastGood>,
}

static LOADER_STATE: Mutex<Option<LoaderState>> = Mutex::new(None);
static SOURCE_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static UPDATE_NOTICES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Compile errors from the latest scan of the user's indicator files. Files
/// listed here either kept their last good version or are not loaded.
pub fn indicator_source_errors() -> Vec<String> {
    lock(&SOURCE_ERRORS).clone()
}

/// Installed copies older than the indicator Orbit Tape ships with the same
/// ID, each with the command that updates it. Refreshed on every load and
/// live reload.
pub fn indicator_update_notices() -> Vec<String> {
    lock(&UPDATE_NOTICES).clone()
}

fn refresh_update_notices(installed_directory: &Path) {
    let notices: Vec<String> = crate::indicator_updates::find(
        installed_directory,
        &crate::indicator_updates::shipped_folders(),
    )
    .iter()
    .map(crate::indicator_updates::UpdateNotice::message)
    .collect();
    for notice in &notices {
        warn!("{notice}");
    }
    *lock(&UPDATE_NOTICES) = notices;
}

/// Directories and files the live reload watcher should observe: the
/// installed indicators directory (recursive) and each `--indicator-path`
/// file.
pub fn watched_indicator_paths() -> Option<(PathBuf, Vec<PathBuf>)> {
    lock(&LOADER_STATE).as_ref().map(|state| {
        (
            state.paths.installed_directory.clone(),
            state.paths.additional_files.clone(),
        )
    })
}

/// Outcome of a live reload.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndicatorReloadReport {
    /// IDs whose source changed (or that are new) and compiled.
    pub reloaded: Vec<String>,
    /// IDs that are no longer provided by any file.
    pub removed: Vec<String>,
    /// Compile errors. A file with an earlier good version keeps it.
    pub errors: Vec<String>,
}

impl IndicatorReloadReport {
    pub fn is_empty(&self) -> bool {
        self.reloaded.is_empty() && self.removed.is_empty() && self.errors.is_empty()
    }
}

/// Load one watched source file. Unchanged files reuse their previous
/// registration, so running charts are not recomputed. A file that no longer
/// compiles falls back to its last good version, with the errors returned.
fn load_watched_file(
    path: &Path,
    origin: language::PackageOrigin,
    previous: Option<&LastGood>,
) -> (Option<LastGood>, Vec<String>) {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return (None, vec![format!("{}: {error}", path.display())]),
    };
    let hash = crate::util::stable_hash(&bytes);
    if let Some(previous) = previous.filter(|previous| previous.hash == hash) {
        return (Some(previous.clone()), Vec::new());
    }
    let compiled = match std::str::from_utf8(&bytes) {
        Ok(text) => indicator_engine::compile(text, path),
        Err(error) => {
            return (
                previous.cloned(),
                vec![format!("{}: not valid UTF-8: {error}", path.display())],
            );
        }
    };
    let mut report = language::load_compiled_report([(path.to_path_buf(), compiled)], origin);
    match report.registrations.pop() {
        Some(registration) if report.diagnostics.is_empty() => {
            (Some(LastGood { hash, registration }), Vec::new())
        }
        _ => (previous.cloned(), report.diagnostics),
    }
}

/// Load every watched file. Returns the registrations, the new last-good
/// table and the compile errors.
fn load_watched_files(
    files: &[(PathBuf, language::PackageOrigin)],
    previous: &HashMap<PathBuf, LastGood>,
) -> (
    Vec<IndicatorRegistration>,
    HashMap<PathBuf, LastGood>,
    Vec<String>,
) {
    let mut registrations = Vec::new();
    let mut last_good = HashMap::new();
    let mut errors = Vec::new();
    for (path, origin) in files {
        let (loaded, file_errors) = load_watched_file(path, *origin, previous.get(path));
        if !file_errors.is_empty() {
            let kept = loaded.is_some();
            for error in file_errors {
                errors.push(if kept {
                    format!("{error} (keeping the last good version)")
                } else {
                    error
                });
            }
        }
        if let Some(loaded) = loaded {
            registrations.push(loaded.registration.clone());
            last_good.insert(path.clone(), loaded);
        }
    }
    (registrations, last_good, errors)
}

fn watched_files(
    paths: &IndicatorCatalogPaths,
) -> (Vec<(PathBuf, language::PackageOrigin)>, Vec<String>) {
    let mut diagnostics = Vec::new();
    let installed_files = if let Err(error) = fs::create_dir_all(&paths.installed_directory) {
        diagnostics.push(format!(
            "could not create installed indicator directory {}: {error}",
            paths.installed_directory.display()
        ));
        Vec::new()
    } else {
        match language::indicator_files_in_directory(&paths.installed_directory) {
            Ok((files, discovery_diagnostics)) => {
                diagnostics.extend(discovery_diagnostics);
                files
            }
            Err(error) => {
                diagnostics.push(error);
                Vec::new()
            }
        }
    };
    let files = installed_files
        .into_iter()
        .map(|path| (path, language::PackageOrigin::Installed))
        .chain(
            paths
                .additional_files
                .iter()
                .map(|path| (path.clone(), language::PackageOrigin::Bundled)),
        )
        .collect();
    (files, diagnostics)
}

fn load_catalog_sources(paths: &IndicatorCatalogPaths) -> Result<IndicatorCatalogSources, String> {
    let bundled_report = match &paths.bundled_directory {
        Some(directory) => {
            let (files, diagnostics) = language::indicator_files_in_directory(directory)?;
            let mut report =
                language::load_from_files_report(&files, language::PackageOrigin::Bundled);
            report.diagnostics.extend(diagnostics);
            report
        }
        None => super::bundled::load_bundled_sources(),
    };
    if !bundled_report.diagnostics.is_empty() {
        return Err(format!(
            "bundled indicator source validation failed:\n{}",
            bundled_report.diagnostics.join("\n")
        ));
    }
    if bundled_report.registrations.is_empty() {
        return Err("bundled indicator collection contains no valid sources".to_owned());
    }

    let (files, mut diagnostics) = watched_files(paths);
    let (installed, last_good, errors) = load_watched_files(&files, &HashMap::new());
    for diagnostic in &errors {
        warn!("Ignoring invalid installed indicator source: {diagnostic}");
    }
    *lock(&SOURCE_ERRORS) = errors.clone();
    diagnostics.extend(errors);
    refresh_update_notices(&paths.installed_directory);

    *lock(&LOADER_STATE) = Some(LoaderState {
        paths: paths.clone(),
        bundled: bundled_report.registrations.clone(),
        last_good,
    });

    Ok(IndicatorCatalogSources {
        bundled: bundled_report.registrations,
        installed,
        diagnostics,
    })
}

/// Rescan the user's indicator files and install a new catalog. Bundled
/// sources are not recompiled. Call it from a worker thread; it reads and
/// compiles files.
pub fn reload_indicator_catalog() -> Result<IndicatorReloadReport, String> {
    let mut guard = lock(&LOADER_STATE);
    let state = guard
        .as_mut()
        .ok_or_else(|| "Indicator catalog is not initialized".to_owned())?;

    let (files, mut diagnostics) = watched_files(&state.paths);
    let (installed, last_good, errors) = load_watched_files(&files, &state.last_good);

    let old_ids = |table: &HashMap<PathBuf, LastGood>| {
        table
            .values()
            .map(|entry| (entry.registration.id.as_str().to_owned(), entry.hash))
            .collect::<HashMap<_, _>>()
    };
    let before = old_ids(&state.last_good);
    let after = old_ids(&last_good);
    let mut reloaded = after
        .iter()
        .filter(|(id, hash)| before.get(*id) != Some(*hash))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    reloaded.sort();
    let mut removed = before
        .keys()
        .filter(|id| !after.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    removed.sort();

    for error in &errors {
        warn!("indicator live reload: {error}");
    }
    *lock(&SOURCE_ERRORS) = errors.clone();
    diagnostics.extend(errors.iter().cloned());
    refresh_update_notices(&state.paths.installed_directory);
    state.last_good = last_good;

    if !reloaded.is_empty() || !removed.is_empty() {
        super::registry::replace_catalog(state.bundled.clone(), installed, diagnostics);
    }

    Ok(IndicatorReloadReport {
        reloaded,
        removed,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{catalog, language::PackageOrigin};
    use super::*;
    use std::sync::Arc;

    fn source(id: &str) -> String {
        format!(
            "language(1)\nindicator(\"Example\", id = {id:?})\nformat(precision = \"base_ticker_min_tick\")\nplot(chart.close)"
        )
    }

    #[test]
    fn origins_and_full_ids_control_collection_loading() {
        let root =
            std::env::temp_dir().join(format!("orbit-tape-host-loading-{}", std::process::id()));
        let bundled = root.join("bundled");
        let installed = root.join("installed");
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::create_dir_all(installed.join("alice")).unwrap();
        std::fs::create_dir_all(installed.join("bob")).unwrap();
        std::fs::write(bundled.join("rsi.ind"), source("catalog.rsi")).unwrap();
        std::fs::write(installed.join("alice/rsi.ind"), source("core.rsi")).unwrap();
        std::fs::write(installed.join("bob/rsi.ind"), source("bob.rsi")).unwrap();
        std::fs::write(installed.join("broken.ind"), "language(1)\nindicator(").unwrap();
        let paths = IndicatorCatalogPaths {
            bundled_directory: Some(bundled.clone()),
            installed_directory: installed,
            additional_files: Vec::new(),
        };
        let sources = load_catalog_sources(&paths).unwrap();
        assert_eq!(sources.bundled.len(), 1);
        assert_eq!(sources.installed.len(), 2);
        assert_eq!(sources.bundled[0].origin, PackageOrigin::Bundled);
        assert!(
            sources
                .installed
                .iter()
                .all(|registration| registration.origin == PackageOrigin::Installed)
        );
        assert!(!sources.diagnostics.is_empty());
        std::fs::write(bundled.join("broken.ind"), "language(1)\nindicator(").unwrap();
        assert!(
            load_catalog_sources(&paths)
                .unwrap_err()
                .contains("bundled indicator source validation failed")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The example indicators in the repository load the way a user's
    /// indicators folder does, and the crossover's alert plots survive.
    #[test]
    fn example_indicators_load_as_installed_sources() {
        let examples =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/indicators");
        let paths = IndicatorCatalogPaths {
            bundled_directory: None,
            installed_directory: examples,
            additional_files: Vec::new(),
        };
        let sources = load_catalog_sources(&paths).unwrap();
        assert!(sources.diagnostics.is_empty(), "{:?}", sources.diagnostics);
        let mut ids = sources
            .installed
            .iter()
            .map(|registration| registration.id.as_str().to_owned())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            [
                "example.cvd-candles",
                "example.ema-crossover",
                "example.rsi-divergence",
                "example.session-vwap",
                "example.volume-delta",
                "example.vwap-bands"
            ]
        );
        let crossover = sources
            .installed
            .iter()
            .find(|registration| registration.id.as_str() == "example.ema-crossover")
            .unwrap();
        let alerts = crossover
            .definition
            .outputs
            .iter()
            .filter(|output| output.alert)
            .count();
        assert_eq!(alerts, 2);
    }

    /// Live reload: unchanged files keep their compiled program, a broken
    /// save keeps the last good version and reports the error, and a fixed
    /// save replaces it.
    #[test]
    fn watched_files_keep_last_good_version() {
        let root =
            std::env::temp_dir().join(format!("orbit-tape-live-reload-{}", std::process::id()));
        let dir = root.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("line.ind");
        std::fs::write(&path, source("demo.line")).unwrap();
        let files = vec![(path.clone(), PackageOrigin::Installed)];

        let (first, table, errors) = load_watched_files(&files, &HashMap::new());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(first.len(), 1);

        let (again, table, errors) = load_watched_files(&files, &table);
        assert!(errors.is_empty());
        assert!(Arc::ptr_eq(&first[0].compiled, &again[0].compiled));

        std::fs::write(&path, "language(1)\nindicator(").unwrap();
        let (broken, table, errors) = load_watched_files(&files, &table);
        assert_eq!(broken.len(), 1, "last good version stays loaded");
        assert!(Arc::ptr_eq(&first[0].compiled, &broken[0].compiled));
        assert!(!errors.is_empty());
        assert!(
            errors
                .iter()
                .all(|error| error.contains("keeping the last good version"))
        );

        std::fs::write(
            &path,
            source("demo.line").replace("chart.close", "chart.open"),
        )
        .unwrap();
        let (fixed, table, errors) = load_watched_files(&files, &table);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!Arc::ptr_eq(&first[0].compiled, &fixed[0].compiled));

        std::fs::remove_file(&path).unwrap();
        let (gone, _, errors) = load_watched_files(&[], &table);
        assert!(gone.is_empty() && errors.is_empty());

        // A file that never compiled is reported and not loaded.
        std::fs::write(&path, "language(1)\nindicator(").unwrap();
        let (none, _, errors) = load_watched_files(&files, &HashMap::new());
        assert!(none.is_empty());
        assert!(!errors.is_empty());
        assert!(errors.iter().all(|error| !error.contains("keeping")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn embedded_bundle_initializes_without_a_source_directory() {
        let root =
            std::env::temp_dir().join(format!("orbit-tape-host-embedded-{}", std::process::id()));
        let paths = IndicatorCatalogPaths {
            bundled_directory: None,
            installed_directory: root.clone(),
            additional_files: Vec::new(),
        };
        initialize_indicator_catalog_with_paths(&paths).unwrap();
        let report = super::super::bundled::load_bundled_sources();
        assert!(!report.registrations.is_empty());
        assert_eq!(catalog().registrations().len(), report.registrations.len());
        assert!(
            catalog()
                .registrations()
                .all(|registration| registration.origin == PackageOrigin::Bundled)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
