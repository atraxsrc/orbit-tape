use super::{IndicatorId, catalog, language::PackageOrigin};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

/// The placeholder catalog address. While it is in place, the browser asks
/// for a real one (Settings > Indicator catalog, or `ORBIT_TAPE_CATALOG_URL`).
pub const DEFAULT_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/YOUR-GITHUB-USER/YOUR-REPO/main/catalog.json";

/// Pick the catalog address: the saved setting, then the
/// `ORBIT_TAPE_CATALOG_URL` environment variable, then the default.
pub fn resolve_catalog_url(setting: &str) -> String {
    let setting = setting.trim();
    if !setting.is_empty() {
        return setting.to_owned();
    }
    std::env::var("ORBIT_TAPE_CATALOG_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_CATALOG_URL.to_owned())
}

/// Check a catalog address before fetching it: HTTPS only, and not the
/// placeholder.
pub fn validate_catalog_url(url: &str) -> Result<url::Url, String> {
    if url == DEFAULT_CATALOG_URL {
        return Err(
            "No indicator catalog is set. Add its catalog.json address under Settings > Indicator catalog."
                .to_owned(),
        );
    }
    let parsed = url::Url::parse(url).map_err(|error| format!("Invalid catalog URL: {error}"))?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err("The catalog URL must start with https://".to_owned());
    }
    Ok(parsed)
}

/// A static `catalog.json` as written by `orbit-tape-indicator catalog`.
/// Entry paths are relative to the catalog file.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    schema_version: u32,
    language_version: u32,
    pub indicators: Vec<Entry>,
    /// Where the catalog was fetched from; sources resolve against it.
    #[serde(skip)]
    source_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub id: IndicatorId,
    pub name: String,
    pub description: Option<String>,
    pub author: Option<Author>,
    pub bases: Vec<String>,
    pub path: String,
    pub sha256: String,
    pub size_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Author {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct FetchError {
    pub message: String,
    retry_at: Option<Instant>,
}

impl FetchError {
    pub fn retry_after(message: impl Into<String>, delay: Duration) -> Self {
        Self {
            message: message.into(),
            retry_at: Some(Instant::now() + delay),
        }
    }
}

impl From<String> for FetchError {
    fn from(message: String) -> Self {
        Self {
            message,
            retry_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceChange {
    Installed,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installation {
    Bundled,
    Installed,
    NotInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Loads only while the catalog has never been requested.
    Open,
    /// Reloads on request, subject to the retry cooldown.
    Refresh,
}

/// Effect the host performs once a toggle is accepted.
#[derive(Debug, Clone)]
pub enum SourceAction {
    Install { entry: Entry, base: String },
    Remove(IndicatorId),
}

#[derive(Debug, Clone)]
pub enum Message {
    Open,
    Refresh,
    ToggleSource(IndicatorId),
    Fetched(Result<Catalog, FetchError>),
    SourceChanged {
        id: IndicatorId,
        change: SourceChange,
        result: Result<(), FetchError>,
    },
}

#[derive(Default)]
pub struct State {
    catalog: Option<Catalog>,
    status: Status,
    /// Install/remove actions taken from the browser. One is pending while the
    /// loaded catalog does not reflect it yet; live reload catches up on its
    /// own, so this is checked against the current catalog every time.
    changes: BTreeMap<IndicatorId, SourceChange>,
    operation: SourceOperation,
}

#[derive(Default)]
enum Status {
    #[default]
    Unopened,
    Loading,
    Ready,
    Failed(FetchError),
}

#[derive(Default)]
enum SourceOperation {
    #[default]
    Idle,
    Running,
    Failed {
        id: IndicatorId,
        error: FetchError,
    },
}

impl Catalog {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut catalog: Self =
            serde_json::from_slice(bytes).map_err(|error| format!("Invalid catalog: {error}"))?;
        catalog.validate()?;
        catalog
            .indicators
            .sort_by_cached_key(|entry| entry.name.to_lowercase());
        Ok(catalog)
    }

    pub fn source_url(&self) -> &str {
        &self.source_url
    }

    pub fn set_source_url(&mut self, url: impl Into<String>) {
        self.source_url = url.into();
    }

    /// The download address of an entry's source, next to the catalog file.
    pub fn entry_url(catalog_url: &str, entry: &Entry) -> Result<url::Url, String> {
        let base = validate_catalog_url(catalog_url)?;
        let url = base
            .join(&entry.path)
            .map_err(|error| format!("Invalid source path for {}: {error}", entry.id.as_str()))?;
        if url.origin() != base.origin() {
            return Err(format!("Invalid source path for {}", entry.id.as_str()));
        }
        Ok(url)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.language_version != 1 {
            return Err(
                "This catalog requires an unsupported format or language version".to_owned(),
            );
        }

        let mut ids = BTreeSet::new();
        for entry in &self.indicators {
            if !ids.insert(&entry.id) {
                return Err("Catalog contains duplicate IDs".to_owned());
            }

            if !entry.path.ends_with(".ind")
                || entry.path.split('/').any(|part| {
                    part.is_empty()
                        || part == "."
                        || part == ".."
                        || part.contains(['\\', ':', '?', '#', '%'])
                })
            {
                return Err(format!("Invalid source path for {}", entry.id.as_str()));
            }

            if entry.size_bytes == 0
                || entry.size_bytes > indicator_engine::CompileLimits::default().max_source_bytes
                || entry.sha256.len() != 64
                || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(format!(
                    "Invalid source size or checksum for {}",
                    entry.id.as_str()
                ));
            }
        }
        Ok(())
    }
}

impl State {
    pub fn catalog(&self) -> Option<&Catalog> {
        self.catalog.as_ref()
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.status, Status::Loading)
    }

    pub fn error(&self) -> Option<&FetchError> {
        match &self.status {
            Status::Failed(error) => Some(error),
            _ => None,
        }
    }

    pub fn error_for(&self, id: &IndicatorId) -> Option<&FetchError> {
        match &self.operation {
            SourceOperation::Failed { id: failed, error } if failed == id => Some(error),
            _ => None,
        }
    }

    pub fn installation(&self, entry: &Entry) -> Installation {
        let origin = catalog()
            .get(&entry.id)
            .map(|registration| registration.origin);
        if origin == Some(PackageOrigin::Bundled) {
            return Installation::Bundled;
        }

        let present = match self.changes.get(&entry.id) {
            Some(SourceChange::Installed) => true,
            Some(SourceChange::Removed) => false,
            None => origin.is_some(),
        };
        if present {
            Installation::Installed
        } else {
            Installation::NotInstalled
        }
    }

    pub fn restart_pending(&self) -> bool {
        self.changes.keys().any(|id| self.restart_pending_for(id))
    }

    pub fn restart_pending_for(&self, id: &IndicatorId) -> bool {
        let loaded = catalog().get(id).is_some();
        match self.changes.get(id) {
            Some(SourceChange::Installed) => !loaded,
            Some(SourceChange::Removed) => loaded,
            None => false,
        }
    }

    pub fn can_refresh(&self) -> bool {
        !self.is_loading() && self.can_request()
    }

    pub fn can_toggle(&self, installation: Installation) -> bool {
        !matches!(installation, Installation::Bundled) && !self.is_running() && self.can_request()
    }

    pub fn begin_load(&mut self, trigger: Trigger) -> bool {
        let allowed = match trigger {
            Trigger::Open => self.is_unopened() && self.can_request(),
            Trigger::Refresh => self.can_refresh(),
        };
        if allowed {
            self.status = Status::Loading;
        }
        allowed
    }

    pub fn loaded(&mut self, catalog: Catalog) {
        self.catalog = Some(catalog);
        self.status = Status::Ready;
    }

    pub fn failed(&mut self, error: FetchError) {
        self.status = Status::Failed(error);
    }

    pub fn begin_toggle(&mut self, id: &IndicatorId) -> Option<SourceAction> {
        let manifest = self.catalog.as_ref()?;
        let entry = manifest
            .indicators
            .iter()
            .find(|entry| entry.id == *id)?
            .clone();
        let base = manifest.source_url().to_owned();

        let installation = self.installation(&entry);
        if !self.can_toggle(installation) {
            return None;
        }
        self.operation = SourceOperation::Running;

        Some(if matches!(installation, Installation::Installed) {
            SourceAction::Remove(entry.id)
        } else {
            SourceAction::Install { entry, base }
        })
    }

    pub fn operation_succeeded(&mut self, id: IndicatorId, change: SourceChange) {
        self.record_change(id, change);
        self.operation = SourceOperation::Idle;
    }

    pub fn operation_failed(&mut self, id: IndicatorId, error: FetchError) {
        self.operation = SourceOperation::Failed { id, error };
    }

    fn is_unopened(&self) -> bool {
        matches!(self.status, Status::Unopened)
    }

    fn is_running(&self) -> bool {
        matches!(self.operation, SourceOperation::Running)
    }

    fn can_request(&self) -> bool {
        let catalog_retry = self.error().and_then(|error| error.retry_at);
        let operation_retry = match &self.operation {
            SourceOperation::Failed { error, .. } => error.retry_at,
            _ => None,
        };
        catalog_retry
            .into_iter()
            .chain(operation_retry)
            .max()
            .is_none_or(|at| Instant::now() >= at)
    }

    fn record_change(&mut self, id: IndicatorId, change: SourceChange) {
        self.changes.insert(id, change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `orbit-tape-indicator catalog` writes.
    const CLI_CATALOG: &str = r#"{
      "generator_version": "0.10.0",
      "indicators": [{
        "author": null, "bases": ["time", "tick"], "description": "d",
        "id": "example.ema-crossover", "name": "EMA Crossover",
        "path": "example/ema-crossover.ind",
        "sha256": "a576f182e19160f3dfe4f8654aaa386842bb68cd91808c696fc697ee4dd59e91",
        "size_bytes": 2031, "source_url": null
      }],
      "language_version": 1,
      "schema_version": 1
    }"#;

    #[test]
    fn parses_cli_catalog_and_resolves_sources_next_to_it() {
        let catalog = Catalog::parse(CLI_CATALOG.as_bytes()).unwrap();
        let url = "https://raw.githubusercontent.com/someone/indicators/main/catalog.json";
        let source = Catalog::entry_url(url, &catalog.indicators[0]).unwrap();
        assert_eq!(
            source.as_str(),
            "https://raw.githubusercontent.com/someone/indicators/main/example/ema-crossover.ind"
        );
    }

    #[test]
    fn rejects_placeholder_and_plain_http() {
        assert!(validate_catalog_url(DEFAULT_CATALOG_URL).is_err());
        assert!(validate_catalog_url("http://example.com/catalog.json").is_err());
        assert!(validate_catalog_url("https://example.com/catalog.json").is_ok());
        assert_eq!(
            resolve_catalog_url("  https://example.com/c.json "),
            "https://example.com/c.json"
        );
    }

    #[test]
    fn rejects_paths_that_leave_the_catalog_folder() {
        let escaped = CLI_CATALOG.replace("example/ema-crossover.ind", "../ema-crossover.ind");
        assert!(Catalog::parse(escaped.as_bytes()).is_err());
        let absolute = CLI_CATALOG.replace("example/ema-crossover.ind", "//evil.example/x.ind");
        assert!(Catalog::parse(absolute.as_bytes()).is_err());
    }
}
