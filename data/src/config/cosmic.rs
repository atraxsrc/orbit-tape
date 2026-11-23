//! Reads the active COSMIC desktop theme from its config files.
//!
//! COSMIC stores one RON value per key under
//! `$XDG_CONFIG_HOME/cosmic/<component>/v1/<key>`. The `cosmic-theme` and
//! `cosmic-config` crates only ship with libcosmic and its own iced fork, so the
//! few keys needed here are parsed directly.

use iced_core::{
    Color,
    theme::{Custom, Palette},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MODE_DIR: &str = "com.system76.CosmicTheme.Mode/v1";
const DARK_DIR: &str = "com.system76.CosmicTheme.Dark/v1";
const LIGHT_DIR: &str = "com.system76.CosmicTheme.Light/v1";

/// Theme keys read from the active theme directory.
const THEME_KEYS: [&str; 6] = [
    "background",
    "accent",
    "success",
    "warning",
    "destructive",
    "corner_radii",
];

/// Name of the iced theme built from the COSMIC palette.
pub const THEME_NAME: &str = "COSMIC";

/// Where the app takes its theme from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeSource {
    /// Follow the COSMIC desktop theme, falling back to the selected theme when
    /// it can't be read.
    #[default]
    Cosmic,
    /// Always use the selected built-in or custom theme.
    Selected,
}

impl ThemeSource {
    pub fn follows_cosmic(self) -> bool {
        self == Self::Cosmic
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CosmicThemeError {
    #[error("no config directory")]
    NoConfigDir,
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<ron::error::SpannedError>,
    },
}

/// The parts of a COSMIC theme that map onto the app's palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CosmicTheme {
    pub is_dark: bool,
    pub background: Color,
    pub text: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub destructive: Color,
    /// COSMIC's extra-small radius, used for small controls.
    pub radius_xs: f32,
    /// COSMIC's small radius, used for containers and menus.
    pub radius_s: f32,
}

impl CosmicTheme {
    /// Loads the active theme from the user's config directory.
    pub fn load() -> Result<Self, CosmicThemeError> {
        Self::load_from(&config_root()?)
    }

    /// Loads the active theme from a `cosmic` config root.
    pub fn load_from(root: &Path) -> Result<Self, CosmicThemeError> {
        // A missing mode file means dark, which is COSMIC's default.
        let mode_path = root.join(MODE_DIR).join("is_dark");
        let is_dark = if mode_path.exists() {
            read_ron::<bool>(&mode_path)?
        } else {
            true
        };

        let dir = root.join(if is_dark { DARK_DIR } else { LIGHT_DIR });
        let background: RonContainer = read_ron(&dir.join("background"))?;
        let accent: RonComponent = read_ron(&dir.join("accent"))?;
        let success: RonComponent = read_ron(&dir.join("success"))?;
        let warning: RonComponent = read_ron(&dir.join("warning"))?;
        let destructive: RonComponent = read_ron(&dir.join("destructive"))?;
        let radii: RonCornerRadii = read_ron(&dir.join("corner_radii"))?;

        Ok(Self {
            is_dark,
            background: background.base.into(),
            text: background.on.into(),
            accent: accent.base.into(),
            success: success.base.into(),
            warning: warning.base.into(),
            destructive: destructive.base.into(),
            radius_xs: radii.radius_xs.uniform(),
            radius_s: radii.radius_s.uniform(),
        })
    }

    pub fn palette(&self) -> Palette {
        Palette {
            background: self.background,
            text: self.text,
            primary: self.accent,
            success: self.success,
            warning: self.warning,
            danger: self.destructive,
        }
    }

    pub fn to_iced_theme(&self) -> iced_core::Theme {
        iced_core::Theme::Custom(Custom::new(THEME_NAME.to_string(), self.palette()).into())
    }
}

/// The `cosmic` directory under the user's config directory.
pub fn config_root() -> Result<PathBuf, CosmicThemeError> {
    dirs_next::config_dir()
        .map(|dir| dir.join("cosmic"))
        .ok_or(CosmicThemeError::NoConfigDir)
}

/// Modification times of every file the theme is read from, in a fixed order.
///
/// COSMIC replaces these files when the theme changes, so a different stamp
/// means the theme has to be read again. Missing files count as `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeStamp(Vec<Option<SystemTime>>);

impl ThemeStamp {
    pub fn read() -> Self {
        config_root()
            .map(|root| Self::read_from(&root))
            .unwrap_or_default()
    }

    pub fn read_from(root: &Path) -> Self {
        let modified = |path: PathBuf| std::fs::metadata(path).and_then(|m| m.modified()).ok();

        let mode = modified(root.join(MODE_DIR).join("is_dark"));
        let themes = [DARK_DIR, LIGHT_DIR].into_iter().flat_map(|dir| {
            THEME_KEYS
                .iter()
                .map(move |key| modified(root.join(dir).join(key)))
        });

        Self(std::iter::once(mode).chain(themes).collect())
    }
}

fn read_ron<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CosmicThemeError> {
    let text = std::fs::read_to_string(path).map_err(|source| CosmicThemeError::Read {
        path: path.to_owned(),
        source,
    })?;
    ron::from_str(&text).map_err(|source| CosmicThemeError::Parse {
        path: path.to_owned(),
        source: Box::new(source),
    })
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct RonColor {
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
}

impl From<RonColor> for Color {
    fn from(c: RonColor) -> Self {
        Color::from_rgba(c.red, c.green, c.blue, c.alpha)
    }
}

/// A COSMIC container; only the fields used here.
#[derive(Debug, Deserialize)]
struct RonContainer {
    base: RonColor,
    on: RonColor,
}

/// A COSMIC component (accent, success, ...); only the fields used here.
#[derive(Debug, Deserialize)]
struct RonComponent {
    base: RonColor,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct RonRadius(f32, f32, f32, f32);

impl RonRadius {
    /// COSMIC radii are per corner; the app uses one value, so take the largest.
    fn uniform(self) -> f32 {
        self.0.max(self.1).max(self.2).max(self.3).max(0.0)
    }
}

#[derive(Debug, Deserialize)]
struct RonCornerRadii {
    radius_xs: RonRadius,
    radius_s: RonRadius,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(r: f32, g: f32, b: f32) -> String {
        format!("(red: {r}, green: {g}, blue: {b}, alpha: 1.0)")
    }

    fn component(base: &str) -> String {
        // Extra fields mirror the real files, which carry more than is read.
        format!("(base: {base}, hover: {base}, on: {base}, border: {base})")
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("flowsurface-cosmic-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            Self(root)
        }

        fn write(&self, rel: &str, contents: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn write_theme(&self, dir: &str, background: (f32, f32, f32), radius_xs: f32) {
            let bg = color(background.0, background.1, background.2);
            self.write(
                &format!("{dir}/background"),
                &format!(
                    "(base: {bg}, component: (base: {bg}), divider: {bg}, on: {}, small_widget: {bg})",
                    color(0.9, 0.9, 1.0)
                ),
            );
            self.write(&format!("{dir}/accent"), &component(&color(0.5, 0.6, 1.0)));
            self.write(&format!("{dir}/success"), &component(&color(0.2, 0.8, 0.5)));
            self.write(&format!("{dir}/warning"), &component(&color(0.9, 0.8, 0.3)));
            self.write(
                &format!("{dir}/destructive"),
                &component(&color(0.9, 0.3, 0.3)),
            );
            self.write(
                &format!("{dir}/corner_radii"),
                &format!(
                    "(radius_0: (0.0, 0.0, 0.0, 0.0), radius_xs: ({radius_xs}, {radius_xs}, {radius_xs}, {radius_xs}), \
                     radius_s: (8.0, 8.0, 8.0, 2.0), radius_m: (16.0, 16.0, 16.0, 16.0))"
                ),
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_dark_theme() {
        let fx = Fixture::new("dark");
        fx.write(&format!("{MODE_DIR}/is_dark"), "true");
        fx.write_theme(DARK_DIR, (0.1, 0.1, 0.15), 4.0);
        fx.write_theme(LIGHT_DIR, (0.95, 0.95, 0.95), 4.0);

        let theme = CosmicTheme::load_from(&fx.0).unwrap();
        assert!(theme.is_dark);
        assert_eq!(theme.background, Color::from_rgb(0.1, 0.1, 0.15));
        assert_eq!(theme.text, Color::from_rgb(0.9, 0.9, 1.0));
        assert_eq!(theme.accent, Color::from_rgb(0.5, 0.6, 1.0));
        assert_eq!(theme.success, Color::from_rgb(0.2, 0.8, 0.5));
        assert_eq!(theme.warning, Color::from_rgb(0.9, 0.8, 0.3));
        assert_eq!(theme.destructive, Color::from_rgb(0.9, 0.3, 0.3));
        assert_eq!(theme.radius_xs, 4.0);
        assert_eq!(theme.radius_s, 8.0);

        let palette = theme.palette();
        assert_eq!(palette.primary, theme.accent);
        assert_eq!(palette.danger, theme.destructive);
        assert!(theme.to_iced_theme().extended_palette().is_dark);
    }

    #[test]
    fn follows_light_mode() {
        let fx = Fixture::new("light");
        fx.write(&format!("{MODE_DIR}/is_dark"), "false");
        fx.write_theme(DARK_DIR, (0.1, 0.1, 0.15), 4.0);
        fx.write_theme(LIGHT_DIR, (0.95, 0.95, 0.95), 0.0);

        let theme = CosmicTheme::load_from(&fx.0).unwrap();
        assert!(!theme.is_dark);
        assert_eq!(theme.background, Color::from_rgb(0.95, 0.95, 0.95));
        assert_eq!(theme.radius_xs, 0.0);
    }

    #[test]
    fn missing_mode_defaults_to_dark() {
        let fx = Fixture::new("nomode");
        fx.write_theme(DARK_DIR, (0.1, 0.1, 0.15), 4.0);

        assert!(CosmicTheme::load_from(&fx.0).unwrap().is_dark);
    }

    #[test]
    fn missing_or_broken_files_are_errors() {
        let fx = Fixture::new("broken");
        assert!(matches!(
            CosmicTheme::load_from(&fx.0),
            Err(CosmicThemeError::Read { .. })
        ));

        fx.write_theme(DARK_DIR, (0.1, 0.1, 0.15), 4.0);
        fx.write(&format!("{DARK_DIR}/accent"), "(base: oops)");
        assert!(matches!(
            CosmicTheme::load_from(&fx.0),
            Err(CosmicThemeError::Parse { .. })
        ));
    }

    #[test]
    fn stamp_changes_when_a_file_changes() {
        let fx = Fixture::new("stamp");
        let empty = ThemeStamp::read_from(&fx.0);

        fx.write(&format!("{MODE_DIR}/is_dark"), "true");
        let with_mode = ThemeStamp::read_from(&fx.0);
        assert_ne!(empty, with_mode);
        assert_eq!(with_mode, ThemeStamp::read_from(&fx.0));
    }

    /// Parses the theme of the machine running the tests:
    /// `cargo test -p flowsurface-data cosmic -- --ignored`.
    #[test]
    #[ignore = "reads the real COSMIC config"]
    fn reads_local_theme() {
        let theme = CosmicTheme::load().unwrap();
        println!("{theme:#?}");
    }

    #[test]
    fn theme_source_serde() {
        assert_eq!(
            serde_json::to_string(&ThemeSource::Cosmic).unwrap(),
            "\"cosmic\""
        );
        let parsed: ThemeSource = serde_json::from_str("\"selected\"").unwrap();
        assert_eq!(parsed, ThemeSource::Selected);
        assert_eq!(ThemeSource::default(), ThemeSource::Cosmic);
    }
}
