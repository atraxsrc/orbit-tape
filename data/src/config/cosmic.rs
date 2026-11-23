//! Reads the active COSMIC desktop theme from its config files.
//!
//! COSMIC stores one RON value per key under
//! `$XDG_CONFIG_HOME/cosmic/<component>/v<N>/<key>`. The `cosmic-theme` and
//! `cosmic-config` crates only ship with libcosmic and its own iced fork, so the
//! few keys needed here are parsed directly.
//!
//! The schema version has moved over time (the theme went from `v1` to `v2`,
//! which also switched colors from float structs to `"#RRGGBBAA"` strings) and
//! old versions are left behind on disk, so every component is read from its
//! highest `v<N>` directory and both color formats are accepted.

use iced_core::{
    Color,
    theme::{Custom, Palette},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MODE: &str = "com.system76.CosmicTheme.Mode";
const DARK: &str = "com.system76.CosmicTheme.Dark";
const LIGHT: &str = "com.system76.CosmicTheme.Light";

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
        let is_dark = read_is_dark(root)?;

        let dir = versioned_dir(root, if is_dark { DARK } else { LIGHT });
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

/// The highest `v<N>` directory of a config component, or `v1` if there is none.
fn versioned_dir(root: &Path, component: &str) -> PathBuf {
    let base = root.join(component);
    let newest = std::fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_prefix('v')?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(1);
    base.join(format!("v{newest}"))
}

/// A missing mode file means dark, which is COSMIC's default.
fn read_is_dark(root: &Path) -> Result<bool, CosmicThemeError> {
    let path = versioned_dir(root, MODE).join("is_dark");
    if path.exists() {
        read_ron(&path)
    } else {
        Ok(true)
    }
}

/// Every file the theme is read from, with its modification time.
///
/// COSMIC replaces these files when the theme changes, so a different stamp
/// means the theme has to be read again. Paths are included so a move to a new
/// schema version counts as a change too. Missing files count as `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeStamp(Vec<(PathBuf, Option<SystemTime>)>);

impl ThemeStamp {
    pub fn read() -> Self {
        config_root()
            .map(|root| Self::read_from(&root))
            .unwrap_or_default()
    }

    pub fn read_from(root: &Path) -> Self {
        let mode = versioned_dir(root, MODE).join("is_dark");
        let themes = [DARK, LIGHT].into_iter().flat_map(|component| {
            let dir = versioned_dir(root, component);
            THEME_KEYS.iter().map(move |key| dir.join(key))
        });

        Self(
            std::iter::once(mode)
                .chain(themes)
                .map(|path| {
                    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                    (path, modified)
                })
                .collect(),
        )
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

/// A COSMIC color: `(red:, green:, blue:, alpha:)` floats in `v1`, a
/// `"#RRGGBB[AA]"` string from `v2` on. Both are sRGB.
#[derive(Debug, Clone, Copy)]
struct RonColor(Color);

impl<'de> Deserialize<'de> for RonColor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Rgba {
            red: f32,
            green: f32,
            blue: f32,
            alpha: f32,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Hex(String),
            Rgba(Rgba),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Rgba(c) => Ok(Self(Color::from_rgba(c.red, c.green, c.blue, c.alpha))),
            Repr::Hex(hex) => parse_hex(&hex)
                .map(Self)
                .ok_or_else(|| serde::de::Error::custom(format!("invalid color {hex:?}"))),
        }
    }
}

impl From<RonColor> for Color {
    fn from(c: RonColor) -> Self {
        c.0
    }
}

/// Parses `#RRGGBB` or `#RRGGBBAA`.
fn parse_hex(hex: &str) -> Option<Color> {
    let digits = hex.strip_prefix('#')?;
    if !digits.is_ascii() || !(digits.len() == 6 || digits.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    let alpha = if digits.len() == 8 { byte(6)? } else { 255 };
    Some(Color::from_rgba8(
        byte(0)?,
        byte(2)?,
        byte(4)?,
        f32::from(alpha) / 255.0,
    ))
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

    /// How a fixture writes colors.
    #[derive(Clone, Copy)]
    enum Format {
        /// `v1`: float structs.
        Floats,
        /// `v2`: hex strings.
        Hex,
    }

    fn color(format: Format, rgb: (u8, u8, u8)) -> String {
        let (r, g, b) = rgb;
        match format {
            Format::Floats => format!(
                "(red: {}, green: {}, blue: {}, alpha: 1.0)",
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0
            ),
            Format::Hex => format!("\"#{r:02X}{g:02X}{b:02X}FF\""),
        }
    }

    fn component(base: &str) -> String {
        // Extra fields mirror the real files, which carry more than is read.
        format!("(base: {base}, hover: {base}, on: {base}, border: {base})")
    }

    fn rgb(rgb: (u8, u8, u8)) -> Color {
        Color::from_rgb8(rgb.0, rgb.1, rgb.2)
    }

    const TEXT: (u8, u8, u8) = (230, 230, 255);
    const ACCENT: (u8, u8, u8) = (242, 86, 35);
    const SUCCESS: (u8, u8, u8) = (136, 165, 125);
    const WARNING: (u8, u8, u8) = (245, 110, 15);
    const DESTRUCTIVE: (u8, u8, u8) = (230, 70, 70);

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

        fn write_mode(&self, version: &str, is_dark: bool) {
            self.write(&format!("{MODE}/{version}/is_dark"), &is_dark.to_string());
        }

        fn write_theme(&self, dir: &str, format: Format, background: (u8, u8, u8), radius_xs: f32) {
            let bg = color(format, background);
            let c = |rgb| color(format, rgb);
            self.write(
                &format!("{dir}/background"),
                &format!(
                    "(base: {bg}, component: (base: {bg}), divider: {bg}, on: {}, small_widget: {bg})",
                    c(TEXT)
                ),
            );
            self.write(&format!("{dir}/accent"), &component(&c(ACCENT)));
            self.write(&format!("{dir}/success"), &component(&c(SUCCESS)));
            self.write(&format!("{dir}/warning"), &component(&c(WARNING)));
            self.write(&format!("{dir}/destructive"), &component(&c(DESTRUCTIVE)));
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

    fn assert_close(a: Color, b: Color) {
        let close = |x: f32, y: f32| (x - y).abs() < 1e-6;
        assert!(
            close(a.r, b.r) && close(a.g, b.g) && close(a.b, b.b) && close(a.a, b.a),
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn reads_v1_float_theme() {
        let fx = Fixture::new("v1");
        fx.write_mode("v1", true);
        fx.write_theme(&format!("{DARK}/v1"), Format::Floats, (26, 27, 38), 4.0);
        fx.write_theme(&format!("{LIGHT}/v1"), Format::Floats, (240, 240, 240), 4.0);

        let theme = CosmicTheme::load_from(&fx.0).unwrap();
        assert!(theme.is_dark);
        assert_close(theme.background, rgb((26, 27, 38)));
        assert_close(theme.text, rgb(TEXT));
        assert_close(theme.accent, rgb(ACCENT));
        assert_close(theme.success, rgb(SUCCESS));
        assert_close(theme.warning, rgb(WARNING));
        assert_close(theme.destructive, rgb(DESTRUCTIVE));
        assert_eq!(theme.radius_xs, 4.0);
        assert_eq!(theme.radius_s, 8.0);

        let palette = theme.palette();
        assert_eq!(palette.primary, theme.accent);
        assert_eq!(palette.danger, theme.destructive);
        assert!(theme.to_iced_theme().extended_palette().is_dark);
    }

    #[test]
    fn prefers_the_newest_schema_version() {
        // A stale v1 theme next to the live v2 one, as left by a COSMIC update.
        // The mode file stays in v1.
        let fx = Fixture::new("v2");
        fx.write_mode("v1", true);
        fx.write_theme(&format!("{DARK}/v1"), Format::Floats, (26, 27, 38), 4.0);
        fx.write_theme(&format!("{DARK}/v2"), Format::Hex, (23, 23, 23), 2.0);

        let theme = CosmicTheme::load_from(&fx.0).unwrap();
        assert_close(theme.background, rgb((23, 23, 23)));
        assert_close(theme.accent, rgb(ACCENT));
        assert_eq!(theme.radius_xs, 2.0);
    }

    #[test]
    fn follows_light_mode() {
        let fx = Fixture::new("light");
        fx.write_mode("v1", false);
        fx.write_theme(&format!("{DARK}/v2"), Format::Hex, (23, 23, 23), 4.0);
        fx.write_theme(&format!("{LIGHT}/v2"), Format::Hex, (240, 240, 240), 0.0);

        let theme = CosmicTheme::load_from(&fx.0).unwrap();
        assert!(!theme.is_dark);
        assert_close(theme.background, rgb((240, 240, 240)));
        assert_eq!(theme.radius_xs, 0.0);
    }

    #[test]
    fn missing_mode_defaults_to_dark() {
        let fx = Fixture::new("nomode");
        fx.write_theme(&format!("{DARK}/v1"), Format::Floats, (26, 27, 38), 4.0);

        assert!(CosmicTheme::load_from(&fx.0).unwrap().is_dark);
    }

    #[test]
    fn missing_or_broken_files_are_errors() {
        let fx = Fixture::new("broken");
        assert!(matches!(
            CosmicTheme::load_from(&fx.0),
            Err(CosmicThemeError::Read { .. })
        ));

        fx.write_theme(&format!("{DARK}/v2"), Format::Hex, (23, 23, 23), 4.0);
        fx.write(&format!("{DARK}/v2/accent"), "(base: \"#12\")");
        assert!(matches!(
            CosmicTheme::load_from(&fx.0),
            Err(CosmicThemeError::Parse { .. })
        ));
    }

    #[test]
    fn parses_hex_colors() {
        assert_close(
            parse_hex("#F25623FF").unwrap(),
            Color::from_rgb8(242, 86, 35),
        );
        assert_close(
            parse_hex("#F2562380").unwrap(),
            Color::from_rgba8(242, 86, 35, 128.0 / 255.0),
        );
        assert_close(parse_hex("#171717").unwrap(), Color::from_rgb8(23, 23, 23));
        assert!(parse_hex("171717").is_none());
        assert!(parse_hex("#1717").is_none());
        assert!(parse_hex("#GG1717").is_none());
    }

    #[test]
    fn stamp_changes_when_a_file_or_version_changes() {
        let fx = Fixture::new("stamp");
        let empty = ThemeStamp::read_from(&fx.0);

        fx.write_mode("v1", true);
        let with_mode = ThemeStamp::read_from(&fx.0);
        assert_ne!(empty, with_mode);
        assert_eq!(with_mode, ThemeStamp::read_from(&fx.0));

        fx.write_theme(&format!("{DARK}/v2"), Format::Hex, (23, 23, 23), 4.0);
        assert_ne!(with_mode, ThemeStamp::read_from(&fx.0));
    }

    /// Parses the theme of the machine running the tests:
    /// `cargo test -p flowsurface-data cosmic -- --ignored --nocapture`.
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
